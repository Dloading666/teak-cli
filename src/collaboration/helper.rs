//! `teak-collab` helper CLI and local transport boundary.
//!
//! The helper is intentionally usable from either a sidecar binary or the
//! Teak main executable's exact hidden subcommand. Broker/domain logic remains
//! behind `BrokerTransport`, so this module does not depend on Tauri state.

use super::grok::{
    ENV_AUTH_MODE, ENV_BODY_DIR, ENV_CAPABILITY, ENV_CAPABILITY_HANDLE, ENV_ENDPOINT,
    ENV_GENERATION, ENV_MEMBER, ENV_MEMBER_ID, ENV_PROTOCOL_VERSION,
};
use super::protocol::{
    decode_json_line, encode_json_line, validate_alias, validate_generation, validate_uuid,
    AuthProof, ClientOperation, ClientRequest, OutboundMessageKind, ReportStatus, ResponseStatus,
    RuntimeClaim, ServerFrame, ServerResponse, StdioInputEnvelope, StdioOutputEnvelope,
    WakeEnvelope, WireError, MAX_FRAME_BYTES, PROTOCOL_NAME, PROTOCOL_VERSION,
};
use clap::{Parser, Subcommand, ValueEnum};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const ENV_JOURNAL_DIR: &str = "TEAK_COLLAB_JOURNAL_DIR";
const JOURNAL_VERSION: u16 = 1;
const JOURNAL_MAX_BYTES: u64 = 2 * 1024 * 1024;
const JOURNAL_MAX_ENTRIES: usize = 2_048;
const RESOLVED_RETENTION_SECS: u64 = 7 * 24 * 60 * 60;
const LOCK_STALE_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl HelperError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: sanitize_message(message.into()),
            retryable: false,
        }
    }

    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: sanitize_message(message.into()),
            retryable: true,
        }
    }

    fn output(&self) -> StdioOutputEnvelope {
        StdioOutputEnvelope {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            ok: false,
            request_id: None,
            data: None,
            error: Some(WireError {
                code: self.code.clone(),
                message: self.message.clone(),
                retryable: Some(self.retryable),
            }),
        }
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HelperError {}

#[derive(Clone, PartialEq, Eq)]
pub struct RuntimeContext {
    pub endpoint: String,
    pub member_id: String,
    pub claim: RuntimeClaim,
    pub auth: AuthProof,
    pub journal_dir: PathBuf,
    pub body_dir: Option<PathBuf>,
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeContext")
            .field("endpoint", &self.endpoint)
            .field("member_id", &self.member_id)
            .field("claim", &self.claim)
            .field("auth", &self.auth)
            .field("journal_dir", &self.journal_dir)
            .field("body_dir", &self.body_dir)
            .finish()
    }
}

impl RuntimeContext {
    pub fn from_env() -> Result<Self, HelperError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, HelperError> {
        let endpoint = required_env(&mut lookup, ENV_ENDPOINT)?;
        validate_endpoint(&endpoint)?;
        let member_alias = required_env(&mut lookup, ENV_MEMBER)?;
        validate_alias(&member_alias).map_err(protocol_error)?;
        let member_id = required_env(&mut lookup, ENV_MEMBER_ID)?;
        validate_uuid("member_id", &member_id).map_err(protocol_error)?;
        let generation = required_env(&mut lookup, ENV_GENERATION)?;
        validate_generation(&generation).map_err(protocol_error)?;

        let version = required_env(&mut lookup, ENV_PROTOCOL_VERSION)?;
        if version != PROTOCOL_VERSION.to_string() {
            return Err(HelperError::new(
                "unsupported_version",
                "helper protocol version does not match this binary",
            ));
        }

        let mode = required_env(&mut lookup, ENV_AUTH_MODE)?;
        let bearer = lookup(ENV_CAPABILITY);
        let handle = lookup(ENV_CAPABILITY_HANDLE);
        let auth = match mode.as_str() {
            "peer" if bearer.is_none() && handle.is_none() => AuthProof::Peer,
            "handle" if bearer.is_none() => AuthProof::Handle {
                handle: handle.ok_or_else(|| {
                    HelperError::new(
                        "missing_capability_handle",
                        "handle auth requires a capability handle",
                    )
                })?,
            },
            "bearer" if handle.is_none() => AuthProof::Bearer {
                token: bearer.ok_or_else(|| {
                    HelperError::new(
                        "missing_capability",
                        "bearer auth requires a capability token",
                    )
                })?,
            },
            "peer" | "handle" | "bearer" => {
                return Err(HelperError::new(
                    "ambiguous_auth",
                    "auth mode and supplied capability variables conflict",
                ));
            }
            _ => {
                return Err(HelperError::new(
                    "invalid_auth_mode",
                    "unknown collaboration auth mode",
                ));
            }
        };
        validate_auth_for_context(&auth)?;

        let journal_dir = lookup(ENV_JOURNAL_DIR)
            .map(PathBuf::from)
            .unwrap_or_else(default_journal_dir);
        if !journal_dir.is_absolute() {
            return Err(HelperError::new(
                "journal_dir_not_absolute",
                "client operation journal directory must be absolute",
            ));
        }
        let body_dir = lookup(ENV_BODY_DIR).map(PathBuf::from);
        if body_dir.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(HelperError::new(
                "body_dir_not_absolute",
                "helper body directory must be absolute",
            ));
        }

        Ok(Self {
            endpoint,
            member_id,
            claim: RuntimeClaim {
                member_alias,
                generation,
            },
            auth,
            journal_dir,
            body_dir,
        })
    }
}

fn required_env(
    lookup: &mut impl FnMut(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, HelperError> {
    lookup(name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            HelperError::new(
                "missing_runtime_context",
                format!("required collaboration environment variable {name} is missing"),
            )
        })
}

#[derive(Debug, Parser)]
#[command(name = "teak-collab", disable_help_subcommand = true)]
pub struct HelperCli {
    /// Stable idempotency key. Omit on a first attempt; reuse the returned ID
    /// after an uncertain result.
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    /// Generation-scoped, Teak-owned JSON file. This is the preferred body
    /// channel because the helper command then needs no shell pipe/redirection.
    #[arg(long, global = true)]
    pub body_file: Option<PathBuf>,
    #[command(subcommand)]
    pub command: HelperCommand,
}

#[derive(Debug, Subcommand)]
pub enum HelperCommand {
    /// Run the persistent, ID-only monitor stream.
    Listen,
    /// List aliases/actions authorized by the broker.
    Allowed,
    /// Check helper/broker/runtime health.
    Health,
    /// Allocate a private, generation-scoped JSON body file.
    Body {
        #[command(subcommand)]
        command: BodyCommand,
    },
    Send {
        #[arg(long)]
        to: String,
        #[arg(long, value_enum)]
        kind: CliMessageKind,
        #[arg(long)]
        task: Option<String>,
        /// Short message body. Prefer this over stdin/pipes for a single argv.
        #[arg(long)]
        text: Option<String>,
    },
    Inbox {
        #[command(subcommand)]
        command: InboxCommand,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum BodyCommand {
    New,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliMessageKind {
    Message,
    Question,
    Progress,
}

impl From<CliMessageKind> for OutboundMessageKind {
    fn from(value: CliMessageKind) -> Self {
        match value {
            CliMessageKind::Message => Self::Message,
            CliMessageKind::Question => Self::Question,
            CliMessageKind::Progress => Self::Progress,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum InboxCommand {
    Receive,
    Ack {
        #[arg(long)]
        message: String,
        #[arg(long)]
        lease_epoch: u64,
        #[arg(long)]
        lease: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    Assign {
        #[arg(long)]
        to: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        instructions: Option<String>,
    },
    Accept {
        #[arg(long)]
        task: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        lease_epoch: u64,
        #[arg(long)]
        lease: String,
    },
    Start {
        #[arg(long)]
        task: String,
    },
    Report {
        #[arg(long)]
        task: String,
        #[arg(long, value_enum)]
        status: CliReportStatus,
        #[arg(long)]
        summary: Option<String>,
    },
    ReportAck {
        #[arg(long)]
        task: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        lease_epoch: u64,
        #[arg(long)]
        lease: String,
    },
    Cancel {
        #[arg(long)]
        task: String,
    },
    CancelAck {
        #[arg(long)]
        task: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        lease_epoch: u64,
        #[arg(long)]
        lease: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliReportStatus {
    Completed,
    Failed,
}

impl From<CliReportStatus> for ReportStatus {
    fn from(value: CliReportStatus) -> Self {
        match value {
            CliReportStatus::Completed => Self::Completed,
            CliReportStatus::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum TasksCommand {
    Pending,
}

impl HelperCommand {
    fn requires_body(&self) -> bool {
        matches!(
            self,
            Self::Send { .. }
                | Self::Task {
                    command: TaskCommand::Assign { .. } | TaskCommand::Report { .. },
                }
        )
    }

    fn has_inline_body(&self) -> bool {
        match self {
            Self::Send { text: Some(_), .. } => true,
            Self::Task {
                command:
                    TaskCommand::Assign {
                        title: Some(_),
                        instructions: Some(_),
                        ..
                    },
            } => true,
            Self::Task {
                command:
                    TaskCommand::Report {
                        summary: Some(_), ..
                    },
            } => true,
            _ => false,
        }
    }

    fn requires_stdin(&self) -> bool {
        self.requires_body() && !self.has_inline_body()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextInput {
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskAssignInput {
    title: String,
    instructions: String,
    #[serde(default)]
    scope: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskReportInput {
    summary: String,
}

pub trait BrokerTransport {
    fn request(&mut self, request: &ClientRequest) -> Result<ServerResponse, HelperError>;

    fn listen(
        &mut self,
        request: &ClientRequest,
        on_wake: &mut dyn FnMut(WakeEnvelope) -> Result<(), HelperError>,
    ) -> Result<(), HelperError>;
}

pub trait ClientOperationJournal {
    fn prepare(
        &mut self,
        requested_id: Option<&str>,
        fingerprint: &str,
        generation: &str,
    ) -> Result<String, HelperError>;

    fn mark_committed(&mut self, request_id: &str) -> Result<(), HelperError>;

    fn mark_rejected(&mut self, request_id: &str) -> Result<(), HelperError>;
}

/// Binary/hidden-subcommand entry point using process args, env, and stdio.
pub fn run_cli() -> i32 {
    run_cli_from(std::env::args_os().skip(1))
}

/// Main-crate integration entry point. For `teak-cli __collab ...`, call this
/// with `std::env::args_os().skip(2)` before the generic legacy `__*` exit.
pub fn run_cli_from<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(error) => return write_process_error(error, 2),
    };
    let stdin_is_terminal = io::stdin().is_terminal();
    if cli.command.requires_stdin() && cli.body_file.is_none() && stdin_is_terminal {
        return write_process_error(
            HelperError::new(
                "stdin_required",
                "this command requires a versioned JSON envelope on stdin",
            ),
            2,
        );
    }
    let context = match RuntimeContext::from_env() {
        Ok(context) => context,
        Err(error) => return write_process_error(error, 2),
    };
    let mut transport = match DefaultBrokerTransport::new(&context.endpoint) {
        Ok(transport) => transport,
        Err(error) => return write_process_error(error, 1),
    };
    let mut journal =
        match FileOperationJournal::new(context.journal_dir.clone(), &context.member_id) {
            Ok(journal) => journal,
            Err(error) => return write_process_error(error, 1),
        };
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    match execute_parsed(
        cli,
        context,
        &mut transport,
        &mut journal,
        &mut stdin,
        &mut stdout,
    ) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            let _ = write_output_line(&mut stdout, &error.output());
            1
        }
    }
}

fn parse_cli<I, T>(args: I) -> Result<HelperCli, HelperError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv =
        std::iter::once(OsString::from("teak-collab")).chain(args.into_iter().map(Into::into));
    HelperCli::try_parse_from(argv).map_err(|error| {
        let kind = error.kind();
        if matches!(
            kind,
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
        ) {
            HelperError::new("help_requested", error.to_string())
        } else {
            // Do not echo arbitrary argv: lease/capability-like values must not
            // be reflected into a monitor notification or diagnostic log.
            HelperError::new(
                "invalid_cli_arguments",
                format!("CLI parse failed ({kind:?})"),
            )
        }
    })
}

fn write_process_error(error: HelperError, exit_code: i32) -> i32 {
    let mut stdout = io::stdout().lock();
    let _ = write_output_line(&mut stdout, &error.output());
    exit_code
}

/// Test/integration seam that avoids process-global env and sockets.
#[allow(dead_code)] // used by broker integration/tests; sidecar main calls run_cli_from
pub fn execute_with<I, T, R, W>(
    args: I,
    context: RuntimeContext,
    transport: &mut dyn BrokerTransport,
    journal: &mut dyn ClientOperationJournal,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<(), HelperError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
    R: Read,
    W: Write,
{
    let cli = parse_cli(args)?;
    execute_parsed(cli, context, transport, journal, stdin, stdout).map(|_| ())
}

fn execute_parsed<R: Read, W: Write>(
    cli: HelperCli,
    context: RuntimeContext,
    transport: &mut dyn BrokerTransport,
    journal: &mut dyn ClientOperationJournal,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<bool, HelperError> {
    if let HelperCommand::Body {
        command: BodyCommand::New,
    } = &cli.command
    {
        if cli.request_id.is_some() || cli.body_file.is_some() {
            return Err(HelperError::new(
                "unexpected_body_option",
                "body allocation accepts neither request-id nor body-file",
            ));
        }
        let body_dir = context.body_dir.as_deref().ok_or_else(|| {
            HelperError::new(
                "body_file_disabled",
                "this runtime has no Teak-owned body directory",
            )
        })?;
        let path = allocate_body_file(body_dir)?;
        let path = path.to_str().ok_or_else(|| {
            HelperError::new(
                "non_utf8_body_path",
                "allocated body path is not valid UTF-8",
            )
        })?;
        write_output_line(
            stdout,
            &StdioOutputEnvelope {
                protocol: PROTOCOL_NAME.to_string(),
                version: PROTOCOL_VERSION,
                ok: true,
                request_id: None,
                data: Some(serde_json::json!({"path": path})),
                error: None,
            },
        )?;
        return Ok(true);
    }
    if cli.command.has_inline_body() && cli.body_file.is_some() {
        return Err(HelperError::new(
            "conflicting_body",
            "inline body flags cannot be combined with --body-file",
        ));
    }
    if !cli.command.requires_body() && cli.body_file.is_some() {
        return Err(HelperError::new(
            "unexpected_body_file",
            "this command does not accept a body file",
        ));
    }
    let operation = operation_from_cli(
        &cli.command,
        cli.body_file.as_deref(),
        context.body_dir.as_deref(),
        stdin,
    )?;
    let mut request_id = None;
    if operation.is_mutating() {
        let fingerprint = operation_fingerprint(&operation)?;
        request_id = Some(journal.prepare(
            cli.request_id.as_deref(),
            &fingerprint,
            &context.claim.generation,
        )?);
    } else if cli.request_id.is_some() {
        return Err(HelperError::new(
            "unexpected_request_id",
            "read-only operations do not accept request ids",
        ));
    }

    let request = ClientRequest::new(request_id.clone(), context.claim, context.auth, operation)
        .map_err(protocol_error)?;

    if matches!(request.operation, ClientOperation::Listen) {
        transport.listen(&request, &mut |wake| {
            wake.validate().map_err(protocol_error)?;
            let line = encode_json_line(&wake).map_err(protocol_error)?;
            stdout
                .write_all(&line)
                .and_then(|_| stdout.flush())
                .map_err(|error| {
                    HelperError::new(
                        "stdout_write_failed",
                        format!("wake output failed: {error}"),
                    )
                })
        })?;
        return Ok(true);
    }

    let response = transport.request(&request)?;
    response
        .validate_for(request.request_id.as_deref())
        .map_err(protocol_error)?;
    if let Some(request_id) = &request.request_id {
        match (
            &response.status,
            response.error.as_ref().and_then(|error| error.retryable),
        ) {
            (ResponseStatus::Ok, _) => journal.mark_committed(request_id)?,
            (ResponseStatus::Error, Some(true)) => {}
            (ResponseStatus::Error, _) => journal.mark_rejected(request_id)?,
        }
    }
    let output = StdioOutputEnvelope::from_response(response);
    let broker_ok = output.ok;
    write_output_line(stdout, &output)?;
    Ok(broker_ok)
}

fn operation_from_cli<R: Read>(
    command: &HelperCommand,
    body_file: Option<&Path>,
    body_dir: Option<&Path>,
    stdin: &mut R,
) -> Result<ClientOperation, HelperError> {
    let operation = match command {
        HelperCommand::Listen => ClientOperation::Listen,
        HelperCommand::Allowed => ClientOperation::Allowed,
        HelperCommand::Health => ClientOperation::Health,
        HelperCommand::Body { .. } => {
            return Err(HelperError::new(
                "internal_dispatch_error",
                "body allocation was not handled locally",
            ));
        }
        HelperCommand::Send {
            to,
            kind,
            task,
            text,
        } => {
            let input = if let Some(text) = text {
                reject_partial_inline(text, body_file)?;
                TextInput { text: text.clone() }
            } else {
                read_command_body(stdin, body_file, body_dir)?
            };
            ClientOperation::Send {
                to_alias: to.clone(),
                kind: (*kind).into(),
                task_id: task.clone(),
                text: input.text,
            }
        }
        HelperCommand::Inbox { command } => match command {
            InboxCommand::Receive => ClientOperation::InboxReceive,
            InboxCommand::Ack {
                message,
                lease_epoch,
                lease,
            } => ClientOperation::InboxAck {
                message_id: message.clone(),
                lease_epoch: *lease_epoch,
                lease_token: lease.clone(),
            },
        },
        HelperCommand::Task { command } => match command {
            TaskCommand::Assign {
                to,
                title,
                instructions,
            } => {
                let input = match (title, instructions) {
                    (Some(title), Some(instructions)) => {
                        reject_partial_inline(title, body_file)?;
                        TaskAssignInput {
                            title: title.clone(),
                            instructions: instructions.clone(),
                            scope: None,
                        }
                    }
                    (None, None) => read_command_body(stdin, body_file, body_dir)?,
                    _ => {
                        return Err(HelperError::new(
                            "incomplete_inline_body",
                            "task assign inline body requires both --title and --instructions",
                        ));
                    }
                };
                ClientOperation::TaskAssign {
                    to_alias: to.clone(),
                    title: input.title,
                    instructions: input.instructions,
                    scope: input.scope,
                }
            }
            TaskCommand::Accept {
                task,
                message,
                lease_epoch,
                lease,
            } => ClientOperation::TaskAccept {
                task_id: task.clone(),
                message_id: message.clone(),
                lease_epoch: *lease_epoch,
                lease_token: lease.clone(),
            },
            TaskCommand::Start { task } => ClientOperation::TaskStart {
                task_id: task.clone(),
            },
            TaskCommand::Report {
                task,
                status,
                summary,
            } => {
                let input = if let Some(summary) = summary {
                    reject_partial_inline(summary, body_file)?;
                    TaskReportInput {
                        summary: summary.clone(),
                    }
                } else {
                    read_command_body(stdin, body_file, body_dir)?
                };
                ClientOperation::TaskReport {
                    task_id: task.clone(),
                    status: (*status).into(),
                    summary: input.summary,
                }
            }
            TaskCommand::ReportAck {
                task,
                message,
                lease_epoch,
                lease,
            } => ClientOperation::TaskReportAck {
                task_id: task.clone(),
                message_id: message.clone(),
                lease_epoch: *lease_epoch,
                lease_token: lease.clone(),
            },
            TaskCommand::Cancel { task } => ClientOperation::TaskCancel {
                task_id: task.clone(),
                reason: None,
            },
            TaskCommand::CancelAck {
                task,
                message,
                lease_epoch,
                lease,
            } => ClientOperation::TaskCancelAck {
                task_id: task.clone(),
                message_id: message.clone(),
                lease_epoch: *lease_epoch,
                lease_token: lease.clone(),
            },
        },
        HelperCommand::Tasks { command } => match command {
            TasksCommand::Pending => ClientOperation::TasksPending,
        },
    };
    operation.validate().map_err(protocol_error)?;
    Ok(operation)
}

fn reject_partial_inline(value: &str, body_file: Option<&Path>) -> Result<(), HelperError> {
    if body_file.is_some() {
        return Err(HelperError::new(
            "conflicting_body",
            "inline body flags cannot be combined with --body-file",
        ));
    }
    if value.is_empty() {
        return Err(HelperError::new(
            "empty_inline_body",
            "inline body flags must not be empty",
        ));
    }
    Ok(())
}

fn read_command_body<T: DeserializeOwned>(
    stdin: &mut impl Read,
    body_file: Option<&Path>,
    body_dir: Option<&Path>,
) -> Result<T, HelperError> {
    if let Some(body_file) = body_file {
        let body_dir = body_dir.ok_or_else(|| {
            HelperError::new(
                "body_file_disabled",
                "this runtime has no Teak-owned body directory",
            )
        })?;
        let bytes = read_private_body_file(body_file, body_dir)?;
        return decode_stdin_body(&bytes);
    }
    read_stdin_body(stdin)
}

fn read_stdin_body<T: DeserializeOwned>(stdin: &mut impl Read) -> Result<T, HelperError> {
    let mut bytes = Vec::new();
    stdin
        .take((MAX_FRAME_BYTES + 2) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HelperError::new(
                "stdin_read_failed",
                format!("could not read stdin: {error}"),
            )
        })?;
    if bytes.is_empty() {
        return Err(HelperError::new(
            "stdin_required",
            "a versioned JSON stdin envelope is required",
        ));
    }
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(HelperError::new(
            "stdin_too_large",
            "stdin JSON envelope exceeds the local protocol frame limit",
        ));
    }
    decode_stdin_body(&bytes)
}

fn decode_stdin_body<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, HelperError> {
    let envelope: StdioInputEnvelope = serde_json::from_slice(bytes).map_err(|_| {
        HelperError::new(
            "invalid_stdin_json",
            "stdin must contain one valid versioned JSON envelope",
        )
    })?;
    envelope.validate().map_err(protocol_error)?;
    serde_json::from_value(envelope.body).map_err(|_| {
        HelperError::new(
            "invalid_stdin_body",
            "stdin body does not match this helper command",
        )
    })
}

fn read_private_body_file(path: &Path, body_dir: &Path) -> Result<Vec<u8>, HelperError> {
    if !path.is_absolute() || path.parent() != Some(body_dir) {
        return Err(HelperError::new(
            "body_file_outside_runtime",
            "body file must be an immediate child of the runtime body directory",
        ));
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            HelperError::new("invalid_body_file", "body file name must be valid UTF-8")
        })?;
    let id = file_name
        .strip_suffix(".json")
        .ok_or_else(|| HelperError::new("invalid_body_file", "body file name must end in .json"))?;
    validate_uuid("body_file", id).map_err(protocol_error)?;
    validate_private_directory(body_dir, "body directory")?;

    let metadata = fs::symlink_metadata(path).map_err(|error| {
        HelperError::new(
            "body_file_read_failed",
            format!("could not inspect body file: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_FRAME_BYTES as u64
    {
        return Err(HelperError::new(
            "unsafe_body_file",
            "body file must be a bounded regular non-symlink file",
        ));
    }
    validate_private_file(path, &metadata)?;

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|error| {
        HelperError::new(
            "body_file_read_failed",
            format!("could not open body file safely: {error}"),
        )
    })?;
    let opened = file.metadata().map_err(|error| {
        HelperError::new(
            "body_file_read_failed",
            format!("could not inspect open body file: {error}"),
        )
    })?;
    validate_private_file(path, &opened)?;
    let mut bytes = Vec::new();
    file.take((MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            HelperError::new(
                "body_file_read_failed",
                format!("could not read body file: {error}"),
            )
        })?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES {
        return Err(HelperError::new(
            "invalid_body_file",
            "body file is empty or exceeds the frame limit",
        ));
    }
    Ok(bytes)
}

fn allocate_body_file(body_dir: &Path) -> Result<PathBuf, HelperError> {
    validate_private_directory(body_dir, "body directory")?;
    for _ in 0..8 {
        let path = body_dir.join(format!("{}.json", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(file) => {
                file.sync_all().map_err(|error| {
                    HelperError::new(
                        "body_file_allocate_failed",
                        format!("could not persist allocated body file: {error}"),
                    )
                })?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(HelperError::new(
                    "body_file_allocate_failed",
                    format!("could not allocate private body file: {error}"),
                ));
            }
        }
    }
    Err(HelperError::new(
        "body_file_allocate_failed",
        "could not allocate a unique private body file",
    ))
}

fn write_output_line(
    stdout: &mut impl Write,
    output: &StdioOutputEnvelope,
) -> Result<(), HelperError> {
    let line = encode_json_line(output).map_err(protocol_error)?;
    stdout
        .write_all(&line)
        .and_then(|_| stdout.flush())
        .map_err(|error| HelperError::new("stdout_write_failed", format!("output failed: {error}")))
}

fn operation_fingerprint(operation: &ClientOperation) -> Result<String, HelperError> {
    let canonical = serde_json::to_vec(&(PROTOCOL_VERSION, operation)).map_err(|error| {
        HelperError::new(
            "fingerprint_failed",
            format!("could not canonicalize operation: {error}"),
        )
    })?;
    Ok(sha256_hex(&canonical))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Pending,
    Committed,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct JournalEntry {
    request_id: String,
    fingerprint: String,
    original_generation: String,
    state: JournalState,
    created_unix_secs: u64,
    updated_unix_secs: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    version: u16,
    entries: Vec<JournalEntry>,
}

pub struct FileOperationJournal {
    path: PathBuf,
    lock_path: PathBuf,
}

impl FileOperationJournal {
    pub fn new(root: PathBuf, member_id: &str) -> Result<Self, HelperError> {
        validate_uuid("member_id", member_id).map_err(protocol_error)?;
        ensure_private_dir(&root)?;
        Ok(Self {
            path: root.join(format!("{member_id}.json")),
            lock_path: root.join(format!("{member_id}.lock")),
        })
    }

    fn update<T>(
        &self,
        callback: impl FnOnce(&mut JournalFile) -> Result<T, HelperError>,
    ) -> Result<T, HelperError> {
        let _lock = JournalLock::acquire(&self.lock_path)?;
        let mut journal = load_journal(&self.path)?;
        prune_journal(&mut journal);
        let result = callback(&mut journal)?;
        write_journal_atomic(&self.path, &journal)?;
        Ok(result)
    }
}

impl ClientOperationJournal for FileOperationJournal {
    fn prepare(
        &mut self,
        requested_id: Option<&str>,
        fingerprint: &str,
        generation: &str,
    ) -> Result<String, HelperError> {
        validate_generation(generation).map_err(protocol_error)?;
        validate_fingerprint(fingerprint)?;
        if let Some(request_id) = requested_id {
            validate_uuid("request_id", request_id).map_err(protocol_error)?;
        }
        self.update(|journal| {
            if let Some(request_id) = requested_id {
                if let Some(entry) = journal
                    .entries
                    .iter()
                    .find(|entry| entry.request_id == request_id)
                {
                    if entry.fingerprint != fingerprint {
                        return Err(HelperError::new(
                            "request_id_conflict",
                            "request id already belongs to a different operation",
                        ));
                    }
                    return Ok(request_id.to_string());
                }
                push_pending(journal, request_id.to_string(), fingerprint, generation)?;
                return Ok(request_id.to_string());
            }

            if let Some(entry) = journal.entries.iter().rev().find(|entry| {
                matches!(entry.state, JournalState::Pending | JournalState::Committed)
                    && entry.fingerprint == fingerprint
            }) {
                return Ok(entry.request_id.clone());
            }

            let request_id = Uuid::new_v4().to_string();
            push_pending(journal, request_id.clone(), fingerprint, generation)?;
            Ok(request_id)
        })
    }

    fn mark_committed(&mut self, request_id: &str) -> Result<(), HelperError> {
        mark_journal_state(self, request_id, JournalState::Committed)
    }

    fn mark_rejected(&mut self, request_id: &str) -> Result<(), HelperError> {
        mark_journal_state(self, request_id, JournalState::Rejected)
    }
}

fn mark_journal_state(
    journal: &FileOperationJournal,
    request_id: &str,
    state: JournalState,
) -> Result<(), HelperError> {
    validate_uuid("request_id", request_id).map_err(protocol_error)?;
    journal.update(|file| {
        let entry = file
            .entries
            .iter_mut()
            .find(|entry| entry.request_id == request_id)
            .ok_or_else(|| {
                HelperError::new(
                    "journal_entry_missing",
                    "client operation journal lost the request id",
                )
            })?;
        entry.state = state;
        entry.updated_unix_secs = now_secs();
        Ok(())
    })
}

fn push_pending(
    journal: &mut JournalFile,
    request_id: String,
    fingerprint: &str,
    generation: &str,
) -> Result<(), HelperError> {
    if journal.entries.len() >= JOURNAL_MAX_ENTRIES {
        return Err(HelperError::new(
            "journal_capacity_reached",
            "client operation journal has too many unresolved entries",
        ));
    }
    let now = now_secs();
    journal.entries.push(JournalEntry {
        request_id,
        fingerprint: fingerprint.to_string(),
        original_generation: generation.to_string(),
        state: JournalState::Pending,
        created_unix_secs: now,
        updated_unix_secs: now,
    });
    Ok(())
}

fn prune_journal(journal: &mut JournalFile) {
    let cutoff = now_secs().saturating_sub(RESOLVED_RETENTION_SECS);
    journal
        .entries
        .retain(|entry| entry.state == JournalState::Pending || entry.updated_unix_secs >= cutoff);
    if journal.entries.len() > JOURNAL_MAX_ENTRIES {
        let excess = journal.entries.len() - JOURNAL_MAX_ENTRIES;
        let mut removed = 0usize;
        journal.entries.retain(|entry| {
            if removed < excess && entry.state != JournalState::Pending {
                removed += 1;
                false
            } else {
                true
            }
        });
    }
}

fn load_journal(path: &Path) -> Result<JournalFile, HelperError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(JournalFile {
                version: JOURNAL_VERSION,
                entries: Vec::new(),
            });
        }
        Err(error) => {
            return Err(HelperError::new(
                "journal_read_failed",
                format!("could not inspect operation journal: {error}"),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(HelperError::new(
            "unsafe_journal_path",
            "operation journal must be a regular non-symlink file",
        ));
    }
    if metadata.len() > JOURNAL_MAX_BYTES {
        return Err(HelperError::new(
            "journal_too_large",
            "operation journal exceeds the safe size limit",
        ));
    }
    validate_private_file(path, &metadata)?;
    let file = File::open(path).map_err(|error| {
        HelperError::new(
            "journal_read_failed",
            format!("could not open operation journal: {error}"),
        )
    })?;
    let journal: JournalFile = serde_json::from_reader(file).map_err(|_| {
        HelperError::new(
            "journal_corrupt",
            "operation journal is not valid versioned JSON",
        )
    })?;
    if journal.version != JOURNAL_VERSION {
        return Err(HelperError::new(
            "journal_version_mismatch",
            "operation journal version is unsupported",
        ));
    }
    for entry in &journal.entries {
        validate_uuid("request_id", &entry.request_id).map_err(protocol_error)?;
        validate_generation(&entry.original_generation).map_err(protocol_error)?;
        validate_fingerprint(&entry.fingerprint)?;
    }
    Ok(journal)
}

fn write_journal_atomic(path: &Path, journal: &JournalFile) -> Result<(), HelperError> {
    let parent = path.parent().ok_or_else(|| {
        HelperError::new(
            "invalid_journal_path",
            "operation journal has no parent directory",
        )
    })?;
    let temp = parent.join(format!(".journal-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|error| {
        HelperError::new(
            "journal_write_failed",
            format!("could not create operation journal temp file: {error}"),
        )
    })?;
    let write_result = (|| -> Result<(), HelperError> {
        serde_json::to_writer(&mut file, journal).map_err(|error| {
            HelperError::new(
                "journal_write_failed",
                format!("could not encode operation journal: {error}"),
            )
        })?;
        file.write_all(b"\n")
            .and_then(|_| file.flush())
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                HelperError::new(
                    "journal_write_failed",
                    format!("could not persist operation journal: {error}"),
                )
            })
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        HelperError::new(
            "journal_write_failed",
            format!("could not atomically replace operation journal: {error}"),
        )
    })?;
    Ok(())
}

struct JournalLock {
    path: PathBuf,
}

impl JournalLock {
    fn acquire(path: &Path) -> Result<Self, HelperError> {
        for _ in 0..100 {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(path) {
                Ok(mut file) => {
                    let _ = writeln!(file, "{} {}", std::process::id(), now_secs());
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(path) {
                        let _ = fs::remove_file(path);
                        continue;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(HelperError::new(
                        "journal_lock_failed",
                        format!("could not lock operation journal: {error}"),
                    ));
                }
            }
        }
        Err(HelperError::retryable(
            "journal_busy",
            "operation journal is busy; retry with the same request id",
        ))
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age.as_secs() > LOCK_STALE_SECS)
        .unwrap_or(false)
}

fn ensure_private_dir(path: &Path) -> Result<(), HelperError> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            HelperError::new(
                "journal_dir_failed",
                format!("could not inspect journal directory: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(HelperError::new(
                "unsafe_journal_dir",
                "journal directory must be a non-symlink directory",
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|error| {
            HelperError::new(
                "journal_dir_failed",
                format!("could not create journal directory: {error}"),
            )
        })?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = fs::metadata(path).map_err(|error| {
            HelperError::new(
                "journal_dir_failed",
                format!("could not inspect journal permissions: {error}"),
            )
        })?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(HelperError::new(
                "unsafe_journal_owner",
                "journal directory is not owned by the current user",
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
                HelperError::new(
                    "journal_permissions_failed",
                    format!("could not restrict journal directory: {error}"),
                )
            })?;
        }
    }
    Ok(())
}

fn validate_private_directory(path: &Path, label: &str) -> Result<(), HelperError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        HelperError::new(
            "private_dir_failed",
            format!("could not inspect {label}: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(HelperError::new(
            "unsafe_private_dir",
            format!("{label} must be a non-symlink directory"),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(HelperError::new(
                "unsafe_private_dir_permissions",
                format!("{label} is not private to the current user"),
            ));
        }
    }
    Ok(())
}

fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<(), HelperError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(HelperError::new(
                "unsafe_journal_permissions",
                format!(
                    "journal file {} is not private to the current user",
                    path.display()
                ),
            ));
        }
    }
    let _ = path;
    Ok(())
}

fn default_journal_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".teak-cli")
        .join("collaboration")
        .join("client-ops")
}

#[cfg(unix)]
pub struct UnixSocketTransport {
    endpoint: PathBuf,
}

#[cfg(unix)]
impl UnixSocketTransport {
    pub fn new(endpoint: &str) -> Result<Self, HelperError> {
        validate_endpoint(endpoint)?;
        Ok(Self {
            endpoint: PathBuf::from(endpoint),
        })
    }

    fn connect(&self, streaming: bool) -> Result<std::os::unix::net::UnixStream, HelperError> {
        validate_socket_file(&self.endpoint)?;
        let stream = std::os::unix::net::UnixStream::connect(&self.endpoint).map_err(|error| {
            HelperError::retryable(
                "broker_unavailable",
                format!("could not connect to collaboration broker: {error}"),
            )
        })?;
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| {
                HelperError::new(
                    "transport_setup_failed",
                    format!("could not set socket write timeout: {error}"),
                )
            })?;
        if !streaming {
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .map_err(|error| {
                    HelperError::new(
                        "transport_setup_failed",
                        format!("could not set socket read timeout: {error}"),
                    )
                })?;
        }
        Ok(stream)
    }
}

#[cfg(unix)]
impl BrokerTransport for UnixSocketTransport {
    fn request(&mut self, request: &ClientRequest) -> Result<ServerResponse, HelperError> {
        let mut stream = self.connect(false)?;
        write_request(&mut stream, request)?;
        let mut reader = BufReader::new(stream);
        let frame = read_server_frame(&mut reader)?;
        match frame {
            ServerFrame::Response(response) => Ok(response),
            ServerFrame::Wake(_) | ServerFrame::Heartbeat { .. } => Err(HelperError::new(
                "unexpected_server_frame",
                "broker returned a streaming frame to a request/response operation",
            )),
        }
    }

    fn listen(
        &mut self,
        request: &ClientRequest,
        on_wake: &mut dyn FnMut(WakeEnvelope) -> Result<(), HelperError>,
    ) -> Result<(), HelperError> {
        let mut stream = self.connect(true)?;
        write_request(&mut stream, request)?;
        let mut reader = BufReader::new(stream);
        let mut ready = false;
        loop {
            match read_server_frame(&mut reader)? {
                ServerFrame::Response(response) => {
                    response
                        .validate_for(request.request_id.as_deref())
                        .map_err(protocol_error)?;
                    if response.status == ResponseStatus::Error {
                        let error = response.error.unwrap_or(WireError {
                            code: "listener_rejected".to_string(),
                            message: "broker rejected listener".to_string(),
                            retryable: Some(false),
                        });
                        return Err(HelperError {
                            code: error.code,
                            message: sanitize_message(error.message),
                            retryable: error.retryable.unwrap_or(false),
                        });
                    }
                    if ready {
                        return Err(HelperError::new(
                            "duplicate_listener_ready",
                            "broker sent listener readiness more than once",
                        ));
                    }
                    ready = true;
                }
                ServerFrame::Wake(wake) if ready => on_wake(wake)?,
                ServerFrame::Wake(_) => {
                    return Err(HelperError::new(
                        "wake_before_ready",
                        "broker sent a wake before listener authentication completed",
                    ));
                }
                ServerFrame::Heartbeat { .. } => {
                    // Socket-only liveness control. Never emit to stdout.
                }
            }
        }
    }
}

#[cfg(unix)]
pub type DefaultBrokerTransport = UnixSocketTransport;

#[cfg(windows)]
pub struct DefaultBrokerTransport;

#[cfg(windows)]
impl DefaultBrokerTransport {
    pub fn new(_endpoint: &str) -> Result<Self, HelperError> {
        Err(HelperError::new(
            "named_pipe_not_integrated",
            "Windows named-pipe transport must be supplied by the Teak broker integration",
        ))
    }
}

#[cfg(windows)]
impl BrokerTransport for DefaultBrokerTransport {
    fn request(&mut self, _request: &ClientRequest) -> Result<ServerResponse, HelperError> {
        Err(HelperError::new(
            "named_pipe_not_integrated",
            "Windows named-pipe transport is not integrated",
        ))
    }

    fn listen(
        &mut self,
        _request: &ClientRequest,
        _on_wake: &mut dyn FnMut(WakeEnvelope) -> Result<(), HelperError>,
    ) -> Result<(), HelperError> {
        Err(HelperError::new(
            "named_pipe_not_integrated",
            "Windows named-pipe transport is not integrated",
        ))
    }
}

fn write_request(stream: &mut impl Write, request: &ClientRequest) -> Result<(), HelperError> {
    let line = encode_json_line(request).map_err(protocol_error)?;
    stream
        .write_all(&line)
        .and_then(|_| stream.flush())
        .map_err(|error| {
            HelperError::retryable(
                "transport_write_failed",
                format!("could not write broker request: {error}"),
            )
        })
}

fn read_server_frame(reader: &mut impl BufRead) -> Result<ServerFrame, HelperError> {
    let mut line = Vec::new();
    let bytes_read = reader
        .take((MAX_FRAME_BYTES + 2) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|error| {
            HelperError::retryable(
                "transport_read_failed",
                format!("could not read broker response: {error}"),
            )
        })?;
    if bytes_read == 0 {
        return Err(HelperError::retryable(
            "broker_disconnected",
            "collaboration broker closed the local connection",
        ));
    }
    if line.len() > MAX_FRAME_BYTES + 1 || !line.ends_with(b"\n") {
        return Err(HelperError::new(
            "invalid_server_frame",
            "broker response exceeded the frame limit or lacked a newline",
        ));
    }
    decode_json_line(&line).map_err(protocol_error)
}

#[cfg(unix)]
fn validate_socket_file(path: &Path) -> Result<(), HelperError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        HelperError::retryable(
            "broker_unavailable",
            format!("could not inspect collaboration socket: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(HelperError::new(
            "unsafe_broker_endpoint",
            "collaboration endpoint is not a Unix socket",
        ));
    }
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(HelperError::new(
            "unsafe_broker_permissions",
            "collaboration socket is not private to the current user",
        ));
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<(), HelperError> {
    if endpoint.is_empty() || endpoint.len() > 512 || endpoint.contains(['\0', '\n', '\r']) {
        return Err(HelperError::new(
            "invalid_endpoint",
            "broker endpoint is empty, too long, or contains control characters",
        ));
    }
    #[cfg(unix)]
    if !Path::new(endpoint).is_absolute() {
        return Err(HelperError::new(
            "endpoint_not_absolute",
            "Unix broker endpoint must be absolute",
        ));
    }
    #[cfg(windows)]
    if !endpoint.starts_with(r"\\.\pipe\") {
        return Err(HelperError::new(
            "invalid_named_pipe",
            "Windows broker endpoint must be a named pipe",
        ));
    }
    Ok(())
}

fn validate_auth_for_context(auth: &AuthProof) -> Result<(), HelperError> {
    let value = match auth {
        AuthProof::Peer => return Ok(()),
        AuthProof::Handle { handle } => handle,
        AuthProof::Bearer { token } => token,
    };
    if value.is_empty()
        || value.len() > super::protocol::MAX_TOKEN_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'=')
        })
    {
        return Err(HelperError::new(
            "invalid_credential",
            "capability material has an invalid length or character",
        ));
    }
    Ok(())
}

fn validate_fingerprint(fingerprint: &str) -> Result<(), HelperError> {
    if fingerprint.len() != 64
        || !fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(HelperError::new(
            "invalid_fingerprint",
            "operation fingerprint must be lowercase SHA-256 hex",
        ));
    }
    Ok(())
}

fn protocol_error(error: super::protocol::ProtocolError) -> HelperError {
    HelperError::new(error.code, error.message)
}

fn sanitize_message(mut message: String) -> String {
    message.retain(|character| character == '\n' || character == '\t' || !character.is_control());
    if message.len() > 1_024 {
        message.truncate(1_024);
    }
    message
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// Small self-contained SHA-256 implementation keeps the client-operation
// fingerprint stable without adding another direct crate dependency.
fn sha256_hex(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for block_index in 0..(padded.len() / 64) {
        let chunk = &padded[block_index * 64..(block_index + 1) * 64];
        let mut words = [0u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes(
                chunk[offset..offset + 4]
                    .try_into()
                    .expect("four-byte SHA word"),
            );
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const GENERATION: &str = "42";
    const TASK_ID: &str = "6f75a9c5-e9f2-4cae-975e-bb2d60f62c10";

    #[derive(Default)]
    struct MemoryJournal {
        entries: HashMap<String, (String, bool)>,
    }

    impl ClientOperationJournal for MemoryJournal {
        fn prepare(
            &mut self,
            requested_id: Option<&str>,
            fingerprint: &str,
            _generation: &str,
        ) -> Result<String, HelperError> {
            if let Some(requested_id) = requested_id {
                if let Some((existing, _)) = self.entries.get(requested_id) {
                    if existing != fingerprint {
                        return Err(HelperError::new("request_id_conflict", "conflict"));
                    }
                    return Ok(requested_id.to_string());
                }
            }
            if requested_id.is_none() {
                if let Some((id, _)) = self
                    .entries
                    .iter()
                    .find(|(_, (existing, _))| existing == fingerprint)
                {
                    return Ok(id.clone());
                }
            }
            let id = requested_id
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            self.entries
                .insert(id.clone(), (fingerprint.to_string(), false));
            Ok(id)
        }

        fn mark_committed(&mut self, request_id: &str) -> Result<(), HelperError> {
            self.entries.get_mut(request_id).unwrap().1 = true;
            Ok(())
        }

        fn mark_rejected(&mut self, request_id: &str) -> Result<(), HelperError> {
            self.mark_committed(request_id)
        }
    }

    struct MockTransport {
        requests: Vec<ClientRequest>,
    }

    impl BrokerTransport for MockTransport {
        fn request(&mut self, request: &ClientRequest) -> Result<ServerResponse, HelperError> {
            self.requests.push(request.clone());
            Ok(ServerResponse {
                protocol: PROTOCOL_NAME.to_string(),
                version: PROTOCOL_VERSION,
                request_id: request.request_id.clone(),
                status: ResponseStatus::Ok,
                data: Some(serde_json::json!({"accepted": true})),
                error: None,
            })
        }

        fn listen(
            &mut self,
            _request: &ClientRequest,
            on_wake: &mut dyn FnMut(WakeEnvelope) -> Result<(), HelperError>,
        ) -> Result<(), HelperError> {
            on_wake(WakeEnvelope {
                protocol: PROTOCOL_NAME.to_string(),
                version: PROTOCOL_VERSION,
                message_id: "3db65974-21b6-4f1c-86b9-30ee415d9a4c".to_string(),
                kind: "task_assignment".to_string(),
                sender_alias: "main".to_string(),
                task_id: Some(TASK_ID.to_string()),
            })
        }
    }

    struct RejectTransport;

    impl BrokerTransport for RejectTransport {
        fn request(&mut self, request: &ClientRequest) -> Result<ServerResponse, HelperError> {
            Ok(ServerResponse {
                protocol: PROTOCOL_NAME.to_string(),
                version: PROTOCOL_VERSION,
                request_id: request.request_id.clone(),
                status: ResponseStatus::Error,
                data: None,
                error: Some(WireError {
                    code: "acl_denied".to_string(),
                    message: "operation rejected".to_string(),
                    retryable: Some(false),
                }),
            })
        }

        fn listen(
            &mut self,
            _request: &ClientRequest,
            _on_wake: &mut dyn FnMut(WakeEnvelope) -> Result<(), HelperError>,
        ) -> Result<(), HelperError> {
            unreachable!()
        }
    }

    struct CommitThenDisconnectTransport {
        requests: Vec<ClientRequest>,
        durable_by_request: HashMap<String, String>,
        disconnect_once: bool,
        executions: usize,
    }

    impl BrokerTransport for CommitThenDisconnectTransport {
        fn request(&mut self, request: &ClientRequest) -> Result<ServerResponse, HelperError> {
            self.requests.push(request.clone());
            let request_id = request
                .request_id
                .clone()
                .expect("mutating operation request ID");
            let durable_id = self
                .durable_by_request
                .entry(request_id.clone())
                .or_insert_with(|| {
                    self.executions += 1;
                    Uuid::new_v4().to_string()
                })
                .clone();
            if self.disconnect_once {
                self.disconnect_once = false;
                return Err(HelperError::retryable(
                    "transport_read_failed",
                    "broker committed before the response connection closed",
                ));
            }
            Ok(ServerResponse {
                protocol: PROTOCOL_NAME.to_string(),
                version: PROTOCOL_VERSION,
                request_id: Some(request_id),
                status: ResponseStatus::Ok,
                data: Some(serde_json::json!({"durableId": durable_id})),
                error: None,
            })
        }

        fn listen(
            &mut self,
            _request: &ClientRequest,
            _on_wake: &mut dyn FnMut(WakeEnvelope) -> Result<(), HelperError>,
        ) -> Result<(), HelperError> {
            unreachable!()
        }
    }

    fn context(journal_dir: PathBuf) -> RuntimeContext {
        RuntimeContext {
            endpoint: "/tmp/unused.sock".to_string(),
            member_id: "c8b503be-7cff-46ef-b447-79ee0a60deaf".to_string(),
            claim: RuntimeClaim {
                member_alias: "worker-a".to_string(),
                generation: GENERATION.to_string(),
            },
            auth: AuthProof::Peer,
            journal_dir,
            body_dir: None,
        }
    }

    fn input(body: Value) -> Vec<u8> {
        serde_json::to_vec(&StdioInputEnvelope {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            body,
        })
        .unwrap()
    }

    #[test]
    fn sha256_matches_standard_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn runtime_context_rejects_ambiguous_auth() {
        let values = HashMap::from([
            (ENV_ENDPOINT, "/tmp/test.sock"),
            (ENV_MEMBER, "worker-a"),
            (ENV_MEMBER_ID, "c8b503be-7cff-46ef-b447-79ee0a60deaf"),
            (ENV_GENERATION, GENERATION),
            (ENV_PROTOCOL_VERSION, "1"),
            (ENV_AUTH_MODE, "peer"),
            (ENV_CAPABILITY, "secret"),
        ]);
        let error = RuntimeContext::from_lookup(|name| values.get(name).map(|v| v.to_string()))
            .unwrap_err();
        assert_eq!(error.code, "ambiguous_auth");
    }

    #[test]
    fn send_reads_versioned_stdin_and_never_puts_body_in_argv() {
        let mut transport = MockTransport { requests: vec![] };
        let mut journal = MemoryJournal::default();
        let stdin_bytes = input(serde_json::json!({"text": "line 1\nline 2 'quoted'"}));
        let mut stdin = stdin_bytes.as_slice();
        let mut stdout = Vec::new();
        execute_with(
            ["send", "--to", "main", "--kind", "message"],
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut stdin,
            &mut stdout,
        )
        .unwrap();
        assert_eq!(transport.requests.len(), 1);
        match &transport.requests[0].operation {
            ClientOperation::Send { text, .. } => {
                assert_eq!(text, "line 1\nline 2 'quoted'");
            }
            other => panic!("unexpected operation: {other:?}"),
        }
        assert!(!String::from_utf8(stdout).unwrap().contains("line 1"));
    }

    #[test]
    fn listen_stdout_contains_id_only_wake() {
        let mut transport = MockTransport { requests: vec![] };
        let mut journal = MemoryJournal::default();
        let mut stdin = io::empty();
        let mut stdout = Vec::new();
        execute_with(
            ["listen"],
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut stdin,
            &mut stdout,
        )
        .unwrap();
        let wake: WakeEnvelope = decode_json_line(&stdout).unwrap();
        assert_eq!(wake.sender_alias, "main");
        let rendered = String::from_utf8(stdout).unwrap();
        assert!(!rendered.contains("instructions"));
        assert!(!rendered.contains("payload"));
    }

    #[test]
    fn file_journal_reuses_committed_operation_until_caller_uses_a_new_id() {
        let root = std::env::temp_dir().join(format!("teak-helper-test-{}", Uuid::new_v4()));
        let mut journal =
            FileOperationJournal::new(root.clone(), "c8b503be-7cff-46ef-b447-79ee0a60deaf")
                .unwrap();
        let fingerprint = sha256_hex(b"operation-a");
        let first = journal.prepare(None, &fingerprint, GENERATION).unwrap();
        let retry = journal.prepare(None, &fingerprint, GENERATION).unwrap();
        assert_eq!(retry, first);
        journal.mark_committed(&first).unwrap();
        let lost_response_retry = journal.prepare(None, &fingerprint, GENERATION).unwrap();
        assert_eq!(lost_response_retry, first);
        let requested_repeat = Uuid::new_v4().to_string();
        let deliberate_repeat = journal
            .prepare(Some(&requested_repeat), &fingerprint, GENERATION)
            .unwrap();
        assert_eq!(deliberate_repeat, requested_repeat);
        let conflict = journal
            .prepare(Some(&first), &sha256_hex(b"different"), GENERATION)
            .unwrap_err();
        assert_eq!(conflict.code, "request_id_conflict");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn response_loss_after_broker_commit_reuses_journaled_request_and_durable_result() {
        let root =
            std::env::temp_dir().join(format!("teak-helper-response-loss-{}", Uuid::new_v4()));
        let mut journal =
            FileOperationJournal::new(root.clone(), "c8b503be-7cff-46ef-b447-79ee0a60deaf")
                .expect("file journal");
        let mut transport = CommitThenDisconnectTransport {
            requests: Vec::new(),
            durable_by_request: HashMap::new(),
            disconnect_once: true,
            executions: 0,
        };

        let first_input = input(serde_json::json!({"text": "exactly once after response loss"}));
        let first_error = execute_with(
            ["send", "--to", "main", "--kind", "message"],
            context(root.clone()),
            &mut transport,
            &mut journal,
            &mut first_input.as_slice(),
            &mut Vec::new(),
        )
        .expect_err("first response is lost after commit");
        assert_eq!(first_error.code, "transport_read_failed");

        let second_input = input(serde_json::json!({"text": "exactly once after response loss"}));
        let mut second_output = Vec::new();
        execute_with(
            ["send", "--to", "main", "--kind", "message"],
            context(root.clone()),
            &mut transport,
            &mut journal,
            &mut second_input.as_slice(),
            &mut second_output,
        )
        .expect("retry receives committed result");

        assert_eq!(transport.requests.len(), 2);
        assert_eq!(
            transport.requests[0].request_id,
            transport.requests[1].request_id
        );
        assert_eq!(transport.executions, 1);
        let request_id = transport.requests[1]
            .request_id
            .as_deref()
            .expect("stable request ID");
        let expected_durable = transport
            .durable_by_request
            .get(request_id)
            .expect("committed durable result");
        let output: StdioOutputEnvelope = decode_json_line(&second_output).expect("helper output");
        assert!(output.ok);
        assert_eq!(output.request_id.as_deref(), Some(request_id));
        assert_eq!(
            output
                .data
                .as_ref()
                .and_then(|data| data.get("durableId"))
                .and_then(Value::as_str),
            Some(expected_durable.as_str())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_report_requires_stdin_summary() {
        let mut transport = MockTransport { requests: vec![] };
        let mut journal = MemoryJournal::default();
        let mut stdin = io::empty();
        let mut stdout = Vec::new();
        let error = execute_with(
            ["task", "report", "--task", TASK_ID, "--status", "completed"],
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut stdin,
            &mut stdout,
        )
        .unwrap_err();
        assert_eq!(error.code, "stdin_required");
    }

    #[test]
    fn body_file_flow_needs_no_shell_redirection() {
        let root = std::env::temp_dir().join(format!("teak-body-test-{}", Uuid::new_v4()));
        let body_dir = root.join("bodies");
        ensure_private_dir(&body_dir).unwrap();
        let mut runtime = context(root.join("journal"));
        runtime.body_dir = Some(body_dir.clone());
        let mut transport = MockTransport { requests: vec![] };
        let mut journal = MemoryJournal::default();
        let mut empty = io::empty();
        let mut allocation_output = Vec::new();
        execute_with(
            ["body", "new"],
            runtime.clone(),
            &mut transport,
            &mut journal,
            &mut empty,
            &mut allocation_output,
        )
        .unwrap();
        let allocation: StdioOutputEnvelope = decode_json_line(&allocation_output).unwrap();
        let path = PathBuf::from(
            allocation
                .data
                .and_then(|data| data.get("path").and_then(Value::as_str).map(str::to_string))
                .unwrap(),
        );
        let body = input(serde_json::json!({"text": "body-file message"}));
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&body).unwrap();
        drop(file);

        let mut no_stdin = io::empty();
        let mut send_output = Vec::new();
        execute_with(
            [
                "--body-file",
                path.to_str().unwrap(),
                "send",
                "--to",
                "main",
                "--kind",
                "message",
            ],
            runtime,
            &mut transport,
            &mut journal,
            &mut no_stdin,
            &mut send_output,
        )
        .unwrap();
        assert_eq!(transport.requests.len(), 1);
        assert!(matches!(
            &transport.requests[0].operation,
            ClientOperation::Send { text, .. } if text == "body-file message"
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn inline_flags_send_assign_and_report_without_stdin_or_body_file() {
        let mut transport = MockTransport { requests: vec![] };
        let mut journal = MemoryJournal::default();
        let mut stdin = io::empty();
        let mut stdout = Vec::new();
        execute_with(
            [
                "send",
                "--to",
                "main",
                "--kind",
                "message",
                "--text",
                "inline hello",
            ],
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut stdin,
            &mut stdout,
        )
        .unwrap();
        assert!(matches!(
            &transport.requests[0].operation,
            ClientOperation::Send { text, .. } if text == "inline hello"
        ));

        transport.requests.clear();
        execute_with(
            [
                "task",
                "assign",
                "--to",
                "worker-a",
                "--title",
                "Return constant 42",
                "--instructions",
                "Return the constant 42 through an explicit completed report.",
            ],
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut io::empty(),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(
            &transport.requests[0].operation,
            ClientOperation::TaskAssign { title, instructions, .. }
                if title == "Return constant 42"
                    && instructions == "Return the constant 42 through an explicit completed report."
        ));

        transport.requests.clear();
        execute_with(
            [
                "task",
                "report",
                "--task",
                TASK_ID,
                "--status",
                "completed",
                "--summary",
                "GROK_REAL_E2E_REPORT_42",
            ],
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut io::empty(),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(matches!(
            &transport.requests[0].operation,
            ClientOperation::TaskReport { summary, .. } if summary == "GROK_REAL_E2E_REPORT_42"
        ));
    }

    #[test]
    fn inline_flags_conflict_with_body_file_and_reject_partial_assign() {
        let error = execute_with(
            [
                "--body-file",
                "/tmp/not-used.json",
                "send",
                "--to",
                "main",
                "--kind",
                "message",
                "--text",
                "nope",
            ],
            context(PathBuf::from("/tmp")),
            &mut MockTransport { requests: vec![] },
            &mut MemoryJournal::default(),
            &mut io::empty(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, "conflicting_body");

        let partial = execute_with(
            [
                "task",
                "assign",
                "--to",
                "worker-a",
                "--title",
                "only title",
            ],
            context(PathBuf::from("/tmp")),
            &mut MockTransport { requests: vec![] },
            &mut MemoryJournal::default(),
            &mut io::empty(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert_eq!(partial.code, "incomplete_inline_body");
    }

    #[test]
    fn broker_rejection_is_a_nonzero_outcome_with_one_json_envelope() {
        let cli = parse_cli(["task", "start", "--task", TASK_ID]).unwrap();
        let mut transport = RejectTransport;
        let mut journal = MemoryJournal::default();
        let mut stdin = io::empty();
        let mut stdout = Vec::new();
        let ok = execute_parsed(
            cli,
            context(PathBuf::from("/tmp")),
            &mut transport,
            &mut journal,
            &mut stdin,
            &mut stdout,
        )
        .unwrap();
        assert!(!ok);
        let output: StdioOutputEnvelope = decode_json_line(&stdout).unwrap();
        assert!(!output.ok);
        assert_eq!(output.error.unwrap().code, "acl_denied");
        assert_eq!(stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
    }
}
