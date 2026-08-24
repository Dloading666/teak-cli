//! Local authenticated broker transport for Grok Build collaboration.
//!
//! The broker is deliberately independent from the PTY data path.  Grok's
//! long-lived `listen` helper receives ID-only wake envelopes over a private
//! Unix socket; message bodies are leased through separate authenticated
//! request/response connections.

use super::model::{
    now_ms, AcceptTaskRequest, AckMessageRequest, AssignTaskRequest, CallerIdentity,
    CancelAckRequest, CancelTaskRequest, CollaborationError, LeaseRequest, ListenerState,
    MessageKind, ReportAckRequest, ReportStatus as ModelReportStatus, ReportTaskRequest,
    RuntimeState, SendMessageRequest, DEFAULT_LEASE_MS,
};
use super::protocol::{
    encode_json_line, AuthProof, ClientOperation, ClientRequest, OutboundMessageKind, ReportStatus,
    ResponseStatus, ServerFrame, ServerResponse, WakeEnvelope, WireError, MAX_FRAME_BYTES,
    PROTOCOL_NAME, PROTOCOL_VERSION,
};
use super::service::AuthenticatedRejectionReason;
use super::CollaborationService;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const LISTENER_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_CLIENTS: usize = 32;

struct ActiveClientPermit {
    active_clients: Arc<AtomicUsize>,
}

impl Drop for ActiveClientPermit {
    fn drop(&mut self) {
        self.active_clients.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_acquire_client(active_clients: &Arc<AtomicUsize>) -> Option<ActiveClientPermit> {
    if active_clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
        active_clients.fetch_sub(1, Ordering::AcqRel);
        return None;
    }
    Some(ActiveClientPermit {
        active_clients: active_clients.clone(),
    })
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ListenerKey {
    member_id: String,
    terminal_generation: i64,
}

#[derive(Default)]
struct ActiveListenerRegistry {
    listeners: Mutex<HashSet<ListenerKey>>,
}

impl ActiveListenerRegistry {
    fn acquire(self: &Arc<Self>, caller: &CallerIdentity) -> Option<ActiveListenerPermit> {
        let key = ListenerKey {
            member_id: caller.member_id.clone(),
            terminal_generation: caller.terminal_generation,
        };
        let mut listeners = self
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !listeners.insert(key.clone()) {
            return None;
        }
        drop(listeners);
        Some(ActiveListenerPermit {
            registry: self.clone(),
            key,
        })
    }
}

struct ActiveListenerPermit {
    registry: Arc<ActiveListenerRegistry>,
    key: ListenerKey,
}

impl Drop for ActiveListenerPermit {
    fn drop(&mut self) {
        self.registry
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

#[derive(Clone, Default)]
pub struct BrokerNotifier {
    inner: Arc<(Mutex<u64>, Condvar)>,
}

impl BrokerNotifier {
    pub fn notify(&self) {
        let (sequence, condition) = &*self.inner;
        if let Ok(mut sequence) = sequence.lock() {
            *sequence = sequence.wrapping_add(1);
            condition.notify_all();
        }
    }

    fn sequence(&self) -> u64 {
        self.inner.0.lock().map(|value| *value).unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn test_sequence(&self) -> u64 {
        self.sequence()
    }

    fn wait(&self, previous: u64, timeout: Duration) -> u64 {
        let (sequence, condition) = &*self.inner;
        let Ok(sequence) = sequence.lock() else {
            return previous;
        };
        if *sequence != previous {
            return *sequence;
        }
        condition
            .wait_timeout(sequence, timeout)
            .map(|(sequence, _)| *sequence)
            .unwrap_or(previous)
    }
}

pub struct BrokerHandle {
    endpoint: PathBuf,
    shutdown: Arc<AtomicBool>,
    notifier: BrokerNotifier,
    thread: Option<JoinHandle<()>>,
}

impl BrokerHandle {
    pub fn notifier(&self) -> BrokerNotifier {
        self.notifier.clone()
    }

    pub fn shutdown(&mut self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        self.notifier.notify();
        #[cfg(unix)]
        {
            // Wake a nonblocking accept loop promptly. Failure is expected if
            // the listener has already exited.
            let _ = std::os::unix::net::UnixStream::connect(&self.endpoint);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(unix)]
pub fn start_broker(
    service: Arc<CollaborationService>,
    endpoint: PathBuf,
) -> Result<BrokerHandle, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    if !endpoint.is_absolute() {
        return Err("collaboration broker endpoint must be absolute".to_string());
    }
    let parent = endpoint
        .parent()
        .ok_or_else(|| "collaboration broker endpoint has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create collaboration directory: {error}"))?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not protect collaboration directory: {error}"))?;

    if let Ok(metadata) = std::fs::symlink_metadata(&endpoint) {
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_socket()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("refusing to replace an unsafe broker endpoint".to_string());
        }
        if UnixStream::connect(&endpoint).is_ok() {
            return Err("another collaboration broker is already listening".to_string());
        }
        std::fs::remove_file(&endpoint)
            .map_err(|error| format!("could not remove stale broker socket: {error}"))?;
    }

    let listener = UnixListener::bind(&endpoint)
        .map_err(|error| format!("could not bind collaboration broker: {error}"))?;
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("could not protect collaboration broker: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("could not configure collaboration broker: {error}"))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let notifier = BrokerNotifier::default();
    let active_clients = Arc::new(AtomicUsize::new(0));
    let active_listeners = Arc::new(ActiveListenerRegistry::default());
    let thread_shutdown = shutdown.clone();
    let thread_notifier = notifier.clone();
    let thread_endpoint = endpoint.clone();
    let thread = thread::Builder::new()
        .name("teak-collab-broker".to_string())
        .spawn(move || {
            while !thread_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if thread_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        if let Err(error) = stream.set_nonblocking(false) {
                            eprintln!("[collaboration] could not configure broker client: {error}");
                            drop(stream);
                            continue;
                        }
                        if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(10))) {
                            eprintln!(
                                "[collaboration] could not bound broker client read: {error}"
                            );
                            drop(stream);
                            continue;
                        }
                        let Some(client_permit) = try_acquire_client(&active_clients) else {
                            drop(stream);
                            continue;
                        };
                        let service = service.clone();
                        let shutdown = thread_shutdown.clone();
                        let notifier = thread_notifier.clone();
                        let active_listeners = active_listeners.clone();
                        if let Err(error) = thread::Builder::new()
                            .name("teak-collab-client".to_string())
                            .spawn(move || {
                                // The permit is captured before spawning. If thread creation
                                // fails, dropping the unstarted closure releases capacity too.
                                let _client_permit = client_permit;
                                handle_client(
                                    stream,
                                    service,
                                    shutdown,
                                    notifier,
                                    active_listeners,
                                );
                            })
                        {
                            eprintln!("[collaboration] could not start broker client: {error}");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(error) => {
                        eprintln!("[collaboration] broker accept failed: {error}");
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(&thread_endpoint);
        })
        .map_err(|error| format!("could not start collaboration broker thread: {error}"))?;

    Ok(BrokerHandle {
        endpoint,
        shutdown,
        notifier,
        thread: Some(thread),
    })
}

#[cfg(not(unix))]
pub fn start_broker(
    _service: Arc<CollaborationService>,
    _endpoint: PathBuf,
) -> Result<BrokerHandle, String> {
    Err("Grok collaboration transport is currently available on macOS/Linux only".to_string())
}

#[cfg(unix)]
fn handle_client(
    stream: std::os::unix::net::UnixStream,
    service: Arc<CollaborationService>,
    shutdown: Arc<AtomicBool>,
    notifier: BrokerNotifier,
    active_listeners: Arc<ActiveListenerRegistry>,
) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err(error) => {
            let _ = write_error(&mut writer, None, "invalid_request", &error, false);
            return;
        }
    };
    if let Err(error) = request.validate() {
        let _ = write_error(
            &mut writer,
            request.request_id.clone(),
            error.code,
            &error.message,
            false,
        );
        return;
    }
    let caller = match authenticate_request(&service, &request) {
        Ok(caller) => caller,
        Err(error) => {
            let response = response_from_error(request.request_id.clone(), error);
            let _ = write_frame(&mut writer, &ServerFrame::Response(response));
            return;
        }
    };

    if matches!(request.operation, ClientOperation::Listen) {
        let Some(_listener_permit) = active_listeners.acquire(&caller) else {
            let _ = write_error(
                &mut writer,
                request.request_id,
                "listener_already_active",
                "a listener is already active for this runtime generation",
                false,
            );
            return;
        };
        handle_listener(
            &mut writer,
            &service,
            &caller,
            request.request_id,
            &shutdown,
            &notifier,
        );
        return;
    }

    let request_id = request.request_id.clone();
    match dispatch(&service, &caller, request.operation, request_id.as_deref()) {
        Ok((data, mutated)) => {
            if mutated {
                notifier.notify();
            }
            let _ = write_frame(
                &mut writer,
                &ServerFrame::Response(ok_response(request_id, data)),
            );
        }
        Err(error) => {
            if let Some(reason) = authenticated_rejection_reason(&error) {
                // Authentication already succeeded above, and the service
                // re-authenticates the exact capability before deriving the
                // event actor. Audit failure must not change or weaken the
                // original denial returned to the helper.
                if service.record_request_rejected(&caller, reason).is_err() {
                    eprintln!("[collaboration] could not record authenticated request rejection");
                }
            }
            let _ = write_frame(
                &mut writer,
                &ServerFrame::Response(response_from_error(request_id, error)),
            );
        }
    }
}

/// Collapse internal authorization failures into a deliberately small public
/// audit vocabulary. Cross-team and nonexistent aliases share the same code,
/// and no raw error string or attacker-controlled value reaches metadata.
fn authenticated_rejection_reason(
    error: &CollaborationError,
) -> Option<AuthenticatedRejectionReason> {
    match error {
        CollaborationError::Unauthorized(code) => Some(match *code {
            "target_not_allowed" | "acl" | "assigner_role" | "assignee_role" => {
                AuthenticatedRejectionReason::TargetPolicy
            }
            "task_owner" | "task_recipient" | "assignment_scope" | "report_recipient"
            | "report_scope" | "task_assigner" | "cancel_scope" | "reply_task_scope" => {
                AuthenticatedRejectionReason::TaskScope
            }
            "message_recipient" | "reply_scope" | "retry_scope" => {
                AuthenticatedRejectionReason::ResourceScope
            }
            _ => AuthenticatedRejectionReason::Policy,
        }),
        CollaborationError::NotFound(_) => Some(AuthenticatedRejectionReason::ResourceScope),
        _ => None,
    }
}

#[cfg(unix)]
fn read_request(
    reader: &mut BufReader<std::os::unix::net::UnixStream>,
) -> Result<ClientRequest, String> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take((MAX_FRAME_BYTES + 2) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|error| format!("could not read broker request: {error}"))?;
    if read == 0 || line.len() > MAX_FRAME_BYTES + 1 || !line.ends_with(b"\n") {
        return Err("broker request must be one bounded JSON line".to_string());
    }
    super::protocol::decode_json_line(&line).map_err(|error| error.message)
}

fn authenticate_request(
    service: &CollaborationService,
    request: &ClientRequest,
) -> Result<CallerIdentity, CollaborationError> {
    let generation = request
        .claim
        .generation
        .parse::<i64>()
        .map_err(|_| CollaborationError::Unauthorized("invalid_claim"))?;
    match &request.auth {
        AuthProof::Bearer { token } => {
            service.authenticate_claim(&request.claim.member_alias, generation, token)
        }
        AuthProof::Peer | AuthProof::Handle { .. } => {
            Err(CollaborationError::Unauthorized("unsupported_auth"))
        }
    }
}

fn dispatch(
    service: &CollaborationService,
    caller: &CallerIdentity,
    operation: ClientOperation,
    request_id: Option<&str>,
) -> Result<(Value, bool), CollaborationError> {
    let required_request_id = || {
        request_id
            .map(str::to_owned)
            .ok_or_else(|| CollaborationError::InvalidInput("request id is required".to_string()))
    };
    let (value, mutated) = match operation {
        ClientOperation::Listen => {
            return Err(CollaborationError::InvalidInput(
                "listener operation reached request dispatcher".to_string(),
            ))
        }
        ClientOperation::Allowed => (to_value(service.allowed(caller)?)?, false),
        ClientOperation::Health => (
            json!({"ok": true, "pendingMessages": service.pending_count(caller)?}),
            false,
        ),
        ClientOperation::InboxReceive => (
            to_value(service.lease_next(
                caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )?)?,
            false,
        ),
        ClientOperation::TasksPending => (to_value(service.tasks_pending(caller)?)?, false),
        ClientOperation::Send {
            to_alias,
            kind,
            task_id,
            text,
        } => {
            let kind = match kind {
                OutboundMessageKind::Message => MessageKind::Message,
                OutboundMessageKind::Question => MessageKind::Question,
                OutboundMessageKind::Progress => MessageKind::Progress,
            };
            (
                to_value(service.send_message(
                    caller,
                    SendMessageRequest {
                        recipient_alias: to_alias,
                        kind,
                        task_id,
                        reply_to_message_id: None,
                        payload_text: text,
                        request_id: required_request_id()?,
                        retry_of_message_id: None,
                        not_before: None,
                        expires_at: None,
                    },
                )?)?,
                true,
            )
        }
        ClientOperation::InboxAck {
            message_id,
            lease_epoch,
            lease_token,
        } => (
            to_value(service.ack_message(
                caller,
                AckMessageRequest {
                    message_id,
                    lease_epoch: epoch_i64(lease_epoch)?,
                    lease_token,
                },
            )?)?,
            true,
        ),
        ClientOperation::TaskAssign {
            to_alias,
            title,
            instructions,
            scope,
        } => (
            to_value(service.assign_task(
                caller,
                AssignTaskRequest {
                    assignee_alias: to_alias,
                    title,
                    instructions,
                    optional_scope_json: scope.map(|value| value.to_string()),
                    request_id: required_request_id()?,
                    expires_at: None,
                },
            )?)?,
            true,
        ),
        ClientOperation::TaskAccept {
            task_id,
            message_id,
            lease_epoch,
            lease_token,
        } => (
            to_value(service.accept_task(
                caller,
                AcceptTaskRequest {
                    task_id,
                    assignment_message_id: message_id,
                    lease_epoch: epoch_i64(lease_epoch)?,
                    lease_token,
                },
            )?)?,
            true,
        ),
        ClientOperation::TaskStart { task_id } => {
            (to_value(service.start_task(caller, &task_id)?)?, true)
        }
        ClientOperation::TaskReport {
            task_id,
            status,
            summary,
        } => {
            let status = match status {
                ReportStatus::Completed => ModelReportStatus::Completed,
                ReportStatus::Failed => ModelReportStatus::Failed,
            };
            (
                to_value(service.report_task(
                    caller,
                    ReportTaskRequest {
                        task_id,
                        status,
                        payload_text: summary,
                        request_id: required_request_id()?,
                    },
                )?)?,
                true,
            )
        }
        ClientOperation::TaskReportAck {
            task_id,
            message_id,
            lease_epoch,
            lease_token,
        } => (
            to_value(service.ack_report(
                caller,
                ReportAckRequest {
                    task_id,
                    report_message_id: message_id,
                    lease_epoch: epoch_i64(lease_epoch)?,
                    lease_token,
                },
            )?)?,
            true,
        ),
        ClientOperation::TaskCancel { task_id, reason } => (
            to_value(service.cancel_task(
                caller,
                CancelTaskRequest {
                    task_id,
                    reason: reason.unwrap_or_default(),
                    request_id: required_request_id()?,
                },
            )?)?,
            true,
        ),
        ClientOperation::TaskCancelAck {
            task_id,
            message_id,
            lease_epoch,
            lease_token,
        } => (
            to_value(service.ack_cancel(
                caller,
                CancelAckRequest {
                    task_id,
                    cancel_message_id: message_id,
                    lease_epoch: epoch_i64(lease_epoch)?,
                    lease_token,
                    payload_text: String::new(),
                    request_id: required_request_id()?,
                },
            )?)?,
            true,
        ),
    };
    Ok((value, mutated))
}

fn epoch_i64(epoch: u64) -> Result<i64, CollaborationError> {
    i64::try_from(epoch)
        .map_err(|_| CollaborationError::InvalidInput("lease epoch is out of range".to_string()))
}

fn to_value(value: impl Serialize) -> Result<Value, CollaborationError> {
    serde_json::to_value(value).map_err(|_| {
        CollaborationError::InvalidInput("could not encode collaboration response".to_string())
    })
}

#[cfg(unix)]
fn handle_listener(
    writer: &mut std::os::unix::net::UnixStream,
    service: &CollaborationService,
    caller: &CallerIdentity,
    request_id: Option<String>,
    shutdown: &AtomicBool,
    notifier: &BrokerNotifier,
) {
    if service
        .update_runtime_state(caller, ListenerState::Ready, RuntimeState::Idle)
        .is_err()
    {
        let _ = write_frame(
            writer,
            &ServerFrame::Response(response_from_error(
                request_id,
                CollaborationError::Unauthorized("listener_registration"),
            )),
        );
        return;
    }
    if write_frame(
        writer,
        &ServerFrame::Response(ok_response(request_id, json!({"listening": true}))),
    )
    .is_err()
    {
        let _ = service.update_runtime_state(caller, ListenerState::Offline, RuntimeState::Unknown);
        return;
    }

    let mut observed_sequence = notifier.sequence();
    let mut last_message_wake: Option<String> = None;
    let mut last_control_wake: Option<String> = None;
    let mut last_heartbeat = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        match service.peek_next_pending(caller) {
            Ok(Some(message)) => {
                if last_message_wake.as_deref() != Some(message.id.as_str()) {
                    let sender_alias = match service.store().member(&message.sender_member_id) {
                        Ok(member) => member.alias,
                        Err(_) => break,
                    };
                    let wake = WakeEnvelope {
                        protocol: PROTOCOL_NAME.to_string(),
                        version: PROTOCOL_VERSION,
                        message_id: message.id.clone(),
                        kind: message.kind.to_string(),
                        sender_alias,
                        task_id: message.task_id,
                    };
                    if write_frame(writer, &ServerFrame::Wake(wake)).is_err() {
                        break;
                    }
                    last_message_wake = Some(message.id);
                }
            }
            Ok(None) => {
                last_message_wake = None;
                match service.peek_next_control_wake(caller) {
                    Ok(Some(control))
                        if last_control_wake.as_deref() != Some(control.id.as_str()) =>
                    {
                        let wake = WakeEnvelope {
                            protocol: PROTOCOL_NAME.to_string(),
                            version: PROTOCOL_VERSION,
                            // The v1 wake field is an opaque durable wake ID.
                            // For backend controls it is the append-only event
                            // ID, not an inbox message to receive or ACK.
                            message_id: control.id.clone(),
                            kind: control.kind,
                            sender_alias: "teak-broker".to_string(),
                            task_id: Some(control.task_id),
                        };
                        if write_frame(writer, &ServerFrame::Wake(wake)).is_err() {
                            break;
                        }
                        last_control_wake = Some(control.id);
                    }
                    Ok(Some(_)) | Ok(None) => {}
                    Err(error) => {
                        eprintln!("[collaboration] control wake check ended: {error}");
                        break;
                    }
                }
            }
            Err(error) => {
                eprintln!("[collaboration] listener delivery check ended: {error}");
                break;
            }
        }

        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            if service.touch_runtime_heartbeat(caller).is_err()
                || write_frame(
                    writer,
                    &ServerFrame::Heartbeat {
                        unix_ms: now_ms().max(0) as u64,
                    },
                )
                .is_err()
            {
                break;
            }
            last_heartbeat = Instant::now();
        }
        observed_sequence = notifier.wait(observed_sequence, LISTENER_POLL_INTERVAL);
    }
    let _ = service.update_runtime_state(caller, ListenerState::Offline, RuntimeState::Unknown);
}

fn ok_response(request_id: Option<String>, data: Value) -> ServerResponse {
    ServerResponse {
        protocol: PROTOCOL_NAME.to_string(),
        version: PROTOCOL_VERSION,
        request_id,
        status: ResponseStatus::Ok,
        data: Some(data),
        error: None,
    }
}

fn response_from_error(request_id: Option<String>, error: CollaborationError) -> ServerResponse {
    let (code, message, retryable) = match error {
        CollaborationError::Database(_) | CollaborationError::PoisonedLock => (
            "broker_unavailable",
            "collaboration broker is temporarily unavailable",
            true,
        ),
        CollaborationError::Suspended => (
            "collaboration_suspended",
            "collaboration mode or this team is paused",
            false,
        ),
        CollaborationError::Capacity(limit) if limit.contains("rate") => (
            "rate_limited",
            "collaboration request rate limit reached",
            true,
        ),
        CollaborationError::Capacity(_) => (
            "capacity_reached",
            "collaboration capacity limit reached",
            false,
        ),
        CollaborationError::Conflict(_) | CollaborationError::InvalidState { .. } => (
            "state_conflict",
            "collaboration state changed; refresh before retrying",
            false,
        ),
        CollaborationError::InvalidInput(_) => (
            "invalid_request",
            "collaboration request was invalid",
            false,
        ),
        CollaborationError::NotFound(_)
        | CollaborationError::Unauthorized(_)
        | CollaborationError::StaleGeneration => {
            ("access_denied", "collaboration request was denied", false)
        }
    };
    ServerResponse {
        protocol: PROTOCOL_NAME.to_string(),
        version: PROTOCOL_VERSION,
        request_id,
        status: ResponseStatus::Error,
        data: None,
        error: Some(WireError {
            code: code.to_string(),
            message: message.to_string(),
            retryable: Some(retryable),
        }),
    }
}

#[cfg(unix)]
fn write_frame(
    writer: &mut std::os::unix::net::UnixStream,
    frame: &ServerFrame,
) -> std::io::Result<()> {
    let line = encode_json_line(frame)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.message))?;
    writer.write_all(&line)?;
    writer.flush()
}

#[cfg(unix)]
fn write_error(
    writer: &mut std::os::unix::net::UnixStream,
    request_id: Option<String>,
    code: &str,
    message: &str,
    retryable: bool,
) -> std::io::Result<()> {
    write_frame(
        writer,
        &ServerFrame::Response(ServerResponse {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            request_id,
            status: ResponseStatus::Error,
            data: None,
            error: Some(WireError {
                code: code.to_string(),
                message: message.to_string(),
                retryable: Some(retryable),
            }),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::collaboration::grok::{
        build_launch_spec, install_helper_shim, probe_grok, CollaborationRole, CollaborationRules,
        GrokLaunchConfig, GrokLaunchMode, GrokLaunchSpec, HelperInvocation,
    };
    #[cfg(unix)]
    use crate::collaboration::helper::ENV_JOURNAL_DIR;
    #[cfg(unix)]
    use crate::collaboration::model::{
        ActorType, AuthMethod, MessageState, NewMember, NewRuntime, NewTeam,
        ReplaceTeamConfigRequest, Role, TaskState, TeamConfigMemberInput,
    };
    #[cfg(unix)]
    use crate::collaboration::protocol::{decode_json_line, ClientRequest, RuntimeClaim};
    #[cfg(unix)]
    use std::fs::{File, OpenOptions};
    #[cfg(unix)]
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    #[cfg(unix)]
    use std::process::{Child, Command, Stdio};
    #[cfg(unix)]
    use uuid::Uuid;

    #[test]
    fn lease_epoch_rejects_values_outside_i64() {
        assert!(epoch_i64(i64::MAX as u64).is_ok());
        assert!(epoch_i64(i64::MAX as u64 + 1).is_err());
    }

    #[test]
    fn authorization_errors_do_not_reveal_target_existence() {
        let response = response_from_error(
            Some("a".to_string()),
            CollaborationError::NotFound("member"),
        );
        let error = response.error.unwrap();
        assert_eq!(error.code, "access_denied");
        assert!(!error.message.contains("member"));
        assert_eq!(
            authenticated_rejection_reason(&CollaborationError::Unauthorized(
                "future_sensitive_internal_reason",
            )),
            Some(AuthenticatedRejectionReason::Policy)
        );
        assert_eq!(
            authenticated_rejection_reason(&CollaborationError::InvalidInput(
                "ordinary validation failure".into(),
            )),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn broker_audits_only_authenticated_redacted_policy_rejections() {
        let service = Arc::new(CollaborationService::in_memory().expect("service"));
        service.set_global_enabled(true).expect("global on");
        let team = service
            .create_team(NewTeam {
                name: "Security audit team".into(),
                workspace_fingerprint: "/tmp/security-audit-workspace".into(),
                enabled: false,
            })
            .expect("team");
        let config = service
            .replace_team_config(
                &team.id,
                ReplaceTeamConfigRequest {
                    name: team.name,
                    workspace_fingerprint: team.workspace_fingerprint,
                    members: vec![
                        TeamConfigMemberInput {
                            id: None,
                            alias: "main".into(),
                            display_name: "Main".into(),
                            avatar_id: "cedar".into(),
                            role: Role::Leader,
                            enabled: true,
                            grok_session_id: Some("grok-security-main".into()),
                        },
                        TeamConfigMemberInput {
                            id: None,
                            alias: "worker-a".into(),
                            display_name: "Worker A".into(),
                            avatar_id: "moss".into(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some("grok-security-worker-a".into()),
                        },
                        TeamConfigMemberInput {
                            id: None,
                            alias: "worker-b".into(),
                            display_name: "Worker B".into(),
                            avatar_id: "ember".into(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some("grok-security-worker-b".into()),
                        },
                    ],
                },
            )
            .expect("roster");
        service.set_team_enabled(&team.id, true).expect("team on");

        let runtimes = [
            ("main", "leader.bearer.must-not-log", 1_i64),
            ("worker-a", "worker-a.bearer.must-not-log", 2_i64),
            ("worker-b", "worker-b.bearer.must-not-log", 3_i64),
        ];
        for (alias, secret, generation) in runtimes {
            let member = config
                .members
                .iter()
                .find(|member| member.alias == alias)
                .expect("configured member");
            let binding = config
                .bindings
                .iter()
                .find(|binding| binding.member_id == member.id && binding.released_at.is_none())
                .expect("active binding");
            service
                .register_runtime(NewRuntime {
                    member_id: member.id.clone(),
                    binding_id: binding.id.clone(),
                    terminal_session_id: format!("terminal-{alias}"),
                    terminal_generation: generation,
                    observed_grok_session_id: binding.grok_session_id.clone(),
                    process_id: None,
                    auth_method: AuthMethod::EnvBearer,
                    bearer_secret: Some(secret.into()),
                    token_epoch: 1,
                    attested_workspace_fingerprint: "/tmp/security-audit-workspace".into(),
                    grok_version: "1.0.5".into(),
                    helper_protocol_version: "1".into(),
                    capability_probe_result: "test".into(),
                    listener_state: ListenerState::Ready,
                    runtime_state: RuntimeState::Idle,
                })
                .expect("runtime");
        }

        let main = config
            .members
            .iter()
            .find(|member| member.alias == "main")
            .expect("main")
            .clone();
        let worker_a = config
            .members
            .iter()
            .find(|member| member.alias == "worker-a")
            .expect("worker a")
            .clone();
        let worker_b = config
            .members
            .iter()
            .find(|member| member.alias == "worker-b")
            .expect("worker b")
            .clone();

        let other_team = service
            .create_team(NewTeam {
                name: "Other security team".into(),
                workspace_fingerprint: "/tmp/other-security-workspace".into(),
                enabled: false,
            })
            .expect("other team");
        service
            .add_member(NewMember {
                team_id: other_team.id,
                alias: "cross-team-sensitive-target".into(),
                display_name: "Cross Team".into(),
                avatar_id: "other".into(),
                role: Role::Leader,
                enabled: true,
            })
            .expect("cross-team member");

        let root = PathBuf::from("/tmp").join(format!(
            "teak-security-audit-{}",
            &Uuid::new_v4().simple().to_string()[..12]
        ));
        let endpoint = root.join("broker.sock");
        let mut broker = start_broker(service.clone(), endpoint.clone()).expect("broker");

        let rejection_events = || {
            service
                .store()
                .events_after(0, 10_000)
                .expect("events")
                .into_iter()
                .filter(|event| event.event_type == "request_rejected")
                .collect::<Vec<_>>()
        };
        assert!(rejection_events().is_empty());

        // A forged alias with another member's bearer is rejected before an
        // actor exists, so it must not be able to manufacture an audit row.
        assert_access_denied(request_frame(
            &endpoint,
            &request(
                "main",
                1,
                "worker-b.bearer.must-not-log",
                Some("00000000-0000-4000-8000-000000000001".into()),
                ClientOperation::Send {
                    to_alias: "worker-a".into(),
                    kind: OutboundMessageKind::Message,
                    task_id: None,
                    text: "UNAUTHENTICATED_BODY_MUST_NOT_LOG".into(),
                },
            ),
        ));
        assert!(rejection_events().is_empty());

        assert_access_denied(request_frame(
            &endpoint,
            &request(
                "worker-b",
                3,
                "worker-b.bearer.must-not-log",
                Some("00000000-0000-4000-8000-000000000002".into()),
                ClientOperation::Send {
                    to_alias: "worker-a".into(),
                    kind: OutboundMessageKind::Message,
                    task_id: None,
                    text: "WORKER_TO_WORKER_BODY_MUST_NOT_LOG".into(),
                },
            ),
        ));

        for (request_id, target) in [
            (
                "00000000-0000-4000-8000-000000000003",
                "cross-team-sensitive-target",
            ),
            (
                "00000000-0000-4000-8000-000000000004",
                "does-not-exist-sensitive-target",
            ),
        ] {
            assert_access_denied(request_frame(
                &endpoint,
                &request(
                    "main",
                    1,
                    "leader.bearer.must-not-log",
                    Some(request_id.into()),
                    ClientOperation::Send {
                        to_alias: target.into(),
                        kind: OutboundMessageKind::Message,
                        task_id: None,
                        text: "CROSS_TEAM_BODY_MUST_NOT_LOG".into(),
                    },
                ),
            ));
        }

        // A normal leader-to-worker assignment is the legitimate control and
        // provides a real task for the task-owner spoof attempt below.
        let assignment: crate::collaboration::model::TaskMessageOutcome =
            serde_json::from_value(response_data(request_frame(
                &endpoint,
                &request(
                    "main",
                    1,
                    "leader.bearer.must-not-log",
                    Some("00000000-0000-4000-8000-000000000005".into()),
                    ClientOperation::TaskAssign {
                        to_alias: "worker-a".into(),
                        title: "Legitimate assignment".into(),
                        instructions: "Legitimate control body".into(),
                        scope: None,
                    },
                ),
            )))
            .expect("assignment outcome");

        assert_access_denied(request_frame(
            &endpoint,
            &request(
                "worker-b",
                3,
                "worker-b.bearer.must-not-log",
                Some("00000000-0000-4000-8000-000000000006".into()),
                ClientOperation::TaskReport {
                    task_id: assignment.task.id.clone(),
                    status: ReportStatus::Completed,
                    summary: "TASK_REPORT_BODY_MUST_NOT_LOG".into(),
                },
            ),
        ));

        assert_access_denied(request_frame(
            &endpoint,
            &request(
                "worker-b",
                3,
                "worker-b.bearer.must-not-log",
                Some("00000000-0000-4000-8000-000000000007".into()),
                ClientOperation::InboxAck {
                    message_id: "11111111-1111-4111-8111-111111111111".into(),
                    lease_epoch: 7,
                    lease_token: "LEASE_SECRET_MUST_NOT_LOG".into(),
                },
            ),
        ));

        let events = rejection_events();
        assert_eq!(events.len(), 5);
        assert_eq!(
            events
                .iter()
                .map(|event| (
                    event.actor_member_id.as_deref(),
                    event.redacted_metadata_json.as_str(),
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    Some(worker_b.id.as_str()),
                    r#"{"reasonCode":"target_policy_denied"}"#,
                ),
                (
                    Some(main.id.as_str()),
                    r#"{"reasonCode":"target_policy_denied"}"#,
                ),
                (
                    Some(main.id.as_str()),
                    r#"{"reasonCode":"target_policy_denied"}"#,
                ),
                (
                    Some(worker_b.id.as_str()),
                    r#"{"reasonCode":"task_scope_denied"}"#,
                ),
                (
                    Some(worker_b.id.as_str()),
                    r#"{"reasonCode":"resource_scope_denied"}"#,
                ),
            ]
        );
        for event in &events {
            assert_eq!(event.team_id, team.id);
            assert_eq!(event.aggregate_type, "security");
            assert_eq!(event.event_type, "request_rejected");
            assert_eq!(event.actor_type, ActorType::Member);
            assert_eq!(
                event.aggregate_id,
                event.actor_member_id.as_deref().unwrap()
            );
            let metadata: Value = serde_json::from_str(&event.redacted_metadata_json)
                .expect("valid redacted metadata");
            assert_eq!(metadata.as_object().expect("metadata object").len(), 1);
        }
        let serialized_events = serde_json::to_string(&events).expect("serialize audit events");
        for sensitive in [
            "leader.bearer.must-not-log",
            "worker-a.bearer.must-not-log",
            "worker-b.bearer.must-not-log",
            "LEASE_SECRET_MUST_NOT_LOG",
            "WORKER_TO_WORKER_BODY_MUST_NOT_LOG",
            "CROSS_TEAM_BODY_MUST_NOT_LOG",
            "TASK_REPORT_BODY_MUST_NOT_LOG",
            "cross-team-sensitive-target",
            "does-not-exist-sensitive-target",
            "11111111-1111-4111-8111-111111111111",
            "00000000-0000-4000-8000-000000000006",
        ] {
            assert!(!serialized_events.contains(sensitive), "leaked {sensitive}");
        }

        // The five denied operations only append their audit rows. They never
        // queue a message, mutate the legitimate task, or target worker B.
        {
            let connection = service.store().lock().expect("store lock");
            let message_count: i64 = connection
                .query_row("SELECT COUNT(*) FROM collab_message", [], |row| row.get(0))
                .expect("message count");
            let task_count: i64 = connection
                .query_row("SELECT COUNT(*) FROM collab_task", [], |row| row.get(0))
                .expect("task count");
            assert_eq!(message_count, 1);
            assert_eq!(task_count, 1);
        }
        let unchanged_task = service
            .store()
            .task(&assignment.task.id)
            .expect("task remains");
        assert_eq!(unchanged_task.state, TaskState::Assigned);
        assert!(unchanged_task.terminal_report_message_id.is_none());
        let worker_a_caller = service
            .authenticate_claim("worker-a", 2, "worker-a.bearer.must-not-log")
            .expect("worker a caller");
        let worker_b_caller = service
            .authenticate_claim("worker-b", 3, "worker-b.bearer.must-not-log")
            .expect("worker b caller");
        assert_eq!(service.pending_count(&worker_a_caller).unwrap(), 1);
        assert_eq!(service.pending_count(&worker_b_caller).unwrap(), 0);
        assert_eq!(assignment.task.assignee_member_id, worker_a.id);

        broker.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unstarted_client_handler_releases_its_capacity_permit() {
        let active_clients = Arc::new(AtomicUsize::new(0));
        let permit = try_acquire_client(&active_clients).expect("capacity available");
        assert_eq!(active_clients.load(Ordering::Acquire), 1);

        // `Builder::spawn` drops the captured closure when OS thread creation
        // fails. Dropping this equivalent unstarted handler must release the
        // permit without executing its body.
        let unstarted_handler = move || {
            let _client_permit = permit;
        };
        drop(unstarted_handler);

        assert_eq!(active_clients.load(Ordering::Acquire), 0);
    }

    #[test]
    fn client_capacity_rejection_does_not_increment_the_counter() {
        let active_clients = Arc::new(AtomicUsize::new(MAX_CLIENTS));
        assert!(try_acquire_client(&active_clients).is_none());
        assert_eq!(active_clients.load(Ordering::Acquire), MAX_CLIENTS);
    }

    #[test]
    fn listener_registry_allows_one_concurrent_generation_and_releases_on_drop() {
        let registry = Arc::new(ActiveListenerRegistry::default());
        let caller = CallerIdentity {
            member_id: "member-a".to_string(),
            terminal_generation: 42,
            token_epoch: 7,
            bearer_secret: Some("never-store-this-bearer".to_string()),
        };
        let start = Arc::new(std::sync::Barrier::new(3));
        let release_winner = Arc::new(std::sync::Barrier::new(2));
        let (result_sender, result_receiver) = std::sync::mpsc::channel();
        let mut threads = Vec::new();

        for _ in 0..2 {
            let registry = registry.clone();
            let caller = caller.clone();
            let start = start.clone();
            let release_winner = release_winner.clone();
            let result_sender = result_sender.clone();
            threads.push(thread::spawn(move || {
                start.wait();
                let permit = registry.acquire(&caller);
                result_sender
                    .send(permit.is_some())
                    .expect("publish acquisition result");
                if let Some(_permit) = permit {
                    release_winner.wait();
                }
            }));
        }
        drop(result_sender);
        start.wait();

        let results = [
            result_receiver.recv().expect("first result"),
            result_receiver.recv().expect("second result"),
        ];
        assert_eq!(results.into_iter().filter(|acquired| *acquired).count(), 1);
        assert!(registry.acquire(&caller).is_none());
        let active_debug = format!(
            "{:?}",
            registry
                .listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );
        assert!(!active_debug.contains("never-store-this-bearer"));

        release_winner.wait();
        for thread in threads {
            thread.join().expect("listener contender");
        }
        let replacement = registry
            .acquire(&caller)
            .expect("disconnect releases listener key");
        drop(replacement);
        assert!(registry
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_broker_restart_reconnects_listener_and_preserves_fifo_end_to_end() {
        let service = Arc::new(CollaborationService::in_memory().expect("service"));
        service.set_global_enabled(true).expect("global on");
        let leader_session = Uuid::new_v4().to_string();
        let worker_session = Uuid::new_v4().to_string();
        let team = service
            .create_team(NewTeam {
                name: "Broker team".to_string(),
                workspace_fingerprint: "/tmp/broker-workspace".to_string(),
                enabled: false,
            })
            .expect("team");
        let config = service
            .replace_team_config(
                &team.id,
                ReplaceTeamConfigRequest {
                    name: team.name,
                    workspace_fingerprint: team.workspace_fingerprint,
                    members: vec![
                        TeamConfigMemberInput {
                            id: None,
                            alias: "main".to_string(),
                            display_name: "Main".to_string(),
                            avatar_id: "cedar".to_string(),
                            role: Role::Leader,
                            enabled: true,
                            grok_session_id: Some(leader_session.clone()),
                        },
                        TeamConfigMemberInput {
                            id: None,
                            alias: "worker-a".to_string(),
                            display_name: "Worker A".to_string(),
                            avatar_id: "moss".to_string(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some(worker_session.clone()),
                        },
                    ],
                },
            )
            .expect("roster");
        service.set_team_enabled(&team.id, true).expect("team on");
        let leader = config
            .members
            .iter()
            .find(|member| member.role == Role::Leader)
            .unwrap();
        let worker = config
            .members
            .iter()
            .find(|member| member.role == Role::Worker)
            .unwrap();
        let leader_binding = config
            .bindings
            .iter()
            .find(|binding| binding.member_id == leader.id && binding.released_at.is_none())
            .unwrap();
        let worker_binding = config
            .bindings
            .iter()
            .find(|binding| binding.member_id == worker.id && binding.released_at.is_none())
            .unwrap();
        let leader_secret = "leader.secret";
        let worker_secret = "worker.secret";
        service
            .register_runtime(NewRuntime {
                member_id: leader.id.clone(),
                binding_id: leader_binding.id.clone(),
                terminal_session_id: "terminal-main".to_string(),
                terminal_generation: 1,
                observed_grok_session_id: leader_session,
                process_id: None,
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(leader_secret.to_string()),
                token_epoch: 1,
                attested_workspace_fingerprint: "/tmp/broker-workspace".to_string(),
                grok_version: "1.0.5".to_string(),
                helper_protocol_version: "1".to_string(),
                capability_probe_result: "test".to_string(),
                listener_state: ListenerState::Connecting,
                runtime_state: RuntimeState::Unknown,
            })
            .expect("leader runtime");
        service
            .register_runtime(NewRuntime {
                member_id: worker.id.clone(),
                binding_id: worker_binding.id.clone(),
                terminal_session_id: "terminal-worker".to_string(),
                terminal_generation: 2,
                observed_grok_session_id: worker_session,
                process_id: None,
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(worker_secret.to_string()),
                token_epoch: 2,
                attested_workspace_fingerprint: "/tmp/broker-workspace".to_string(),
                grok_version: "1.0.5".to_string(),
                helper_protocol_version: "1".to_string(),
                capability_probe_result: "test".to_string(),
                listener_state: ListenerState::Connecting,
                runtime_state: RuntimeState::Unknown,
            })
            .expect("worker runtime");

        // macOS sockaddr_un is only 104 bytes; /var/folders/... temp roots
        // can exceed it before the socket name is appended.
        let root = PathBuf::from("/tmp")
            .join(format!("tb-{}", &Uuid::new_v4().simple().to_string()[..12]));
        let endpoint = root.join("broker.sock");
        let mut broker = start_broker(service.clone(), endpoint.clone()).expect("broker");

        let listen_request = request("worker-a", 2, worker_secret, None, ClientOperation::Listen);
        let mut listener_stream = UnixStream::connect(&endpoint).expect("listener connect");
        listener_stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        listener_stream
            .write_all(&encode_json_line(&listen_request).unwrap())
            .unwrap();
        let mut listener = BufReader::new(listener_stream);
        let ServerFrame::Response(ready) = read_frame(&mut listener) else {
            panic!("listener must receive readiness response")
        };
        assert_eq!(ready.status, ResponseStatus::Ok, "{:?}", ready.error);

        let leader_listen_request =
            request("main", 1, leader_secret, None, ClientOperation::Listen);
        let mut leader_listener_stream = UnixStream::connect(&endpoint).expect("leader connect");
        leader_listener_stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        leader_listener_stream
            .write_all(&encode_json_line(&leader_listen_request).unwrap())
            .unwrap();
        let mut leader_listener = BufReader::new(leader_listener_stream);
        let ServerFrame::Response(leader_ready) = read_frame(&mut leader_listener) else {
            panic!("leader listener must receive readiness response")
        };
        assert_eq!(
            leader_ready.status,
            ResponseStatus::Ok,
            "{:?}",
            leader_ready.error
        );

        let duplicate = request_frame(&endpoint, &listen_request);
        let ServerFrame::Response(duplicate) = duplicate else {
            panic!("duplicate listener must receive a response")
        };
        assert_eq!(duplicate.status, ResponseStatus::Error);
        let duplicate_error = duplicate.error.expect("duplicate rejection");
        assert_eq!(duplicate_error.code, "listener_already_active");
        assert!(!duplicate_error.message.contains(worker_secret));

        let assignment_request_id = Uuid::new_v4().to_string();
        let assignment = request(
            "main",
            1,
            leader_secret,
            Some(assignment_request_id),
            ClientOperation::TaskAssign {
                to_alias: "worker-a".to_string(),
                title: "Implement broker test".to_string(),
                instructions: "Return an explicit report".to_string(),
                scope: None,
            },
        );
        let assignment_response = request_frame(&endpoint, &assignment);
        let assignment_data = response_data(assignment_response);
        let outcome: crate::collaboration::model::TaskMessageOutcome =
            serde_json::from_value(assignment_data).unwrap();

        let wake = read_frame(&mut listener);
        let ServerFrame::Wake(wake) = wake else {
            panic!("expected worker wake")
        };
        assert_eq!(wake.message_id, outcome.message.id);
        assert_eq!(wake.sender_alias, "main");
        assert_eq!(wake.task_id.as_deref(), Some(outcome.task.id.as_str()));

        let receive = request(
            "worker-a",
            2,
            worker_secret,
            None,
            ClientOperation::InboxReceive,
        );
        let leased: Option<crate::collaboration::model::LeasedMessage> =
            serde_json::from_value(response_data(request_frame(&endpoint, &receive))).unwrap();
        let leased = leased.expect("leased assignment");
        assert_eq!(leased.message.payload_text, outcome.message.payload_text);

        let accept = request(
            "worker-a",
            2,
            worker_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskAccept {
                task_id: outcome.task.id,
                message_id: leased.message.id,
                lease_epoch: leased.lease_epoch as u64,
                lease_token: leased.lease_token,
            },
        );
        let accepted: crate::collaboration::model::Task =
            serde_json::from_value(response_data(request_frame(&endpoint, &accept))).unwrap();
        assert_eq!(
            accepted.state,
            crate::collaboration::model::TaskState::Accepted
        );

        let start = request(
            "worker-a",
            2,
            worker_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskStart {
                task_id: accepted.id.clone(),
            },
        );
        let started: crate::collaboration::model::Task =
            serde_json::from_value(response_data(request_frame(&endpoint, &start))).unwrap();
        assert_eq!(
            started.state,
            crate::collaboration::model::TaskState::Running
        );
        let report = request(
            "worker-a",
            2,
            worker_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskReport {
                task_id: started.id.clone(),
                status: ReportStatus::Completed,
                summary: "deterministic broker report".to_string(),
            },
        );
        let reported: crate::collaboration::model::TaskMessageOutcome =
            serde_json::from_value(response_data(request_frame(&endpoint, &report))).unwrap();
        assert_eq!(reported.task.id, started.id);
        assert_eq!(
            reported.message.task_id.as_deref(),
            Some(started.id.as_str())
        );

        let ServerFrame::Wake(report_wake) = read_frame(&mut leader_listener) else {
            panic!("leader listener must receive report wake")
        };
        assert_eq!(report_wake.message_id, reported.message.id);
        assert_eq!(report_wake.task_id.as_deref(), Some(started.id.as_str()));
        let leader_receive = request(
            "main",
            1,
            leader_secret,
            None,
            ClientOperation::InboxReceive,
        );
        let report_lease: Option<crate::collaboration::model::LeasedMessage> =
            serde_json::from_value(response_data(request_frame(&endpoint, &leader_receive)))
                .expect("leader report lease");
        let report_lease = report_lease.expect("queued report");
        assert_eq!(report_lease.message.id, reported.message.id);
        assert_eq!(report_lease.message.task_id, reported.message.task_id);
        let report_ack = request(
            "main",
            1,
            leader_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskReportAck {
                task_id: started.id,
                message_id: report_lease.message.id,
                lease_epoch: report_lease.lease_epoch as u64,
                lease_token: report_lease.lease_token,
            },
        );
        let completed: crate::collaboration::model::Task =
            serde_json::from_value(response_data(request_frame(&endpoint, &report_ack))).unwrap();
        assert_eq!(
            completed.state,
            crate::collaboration::model::TaskState::ReportedCompleted
        );
        leader_listener
            .get_mut()
            .shutdown(std::net::Shutdown::Both)
            .expect("disconnect leader listener");
        drop(leader_listener);

        let reminder_assignment = request(
            "main",
            1,
            leader_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskAssign {
                to_alias: "worker-a".to_string(),
                title: "Exercise durable report reminder".to_string(),
                instructions: "Wait for the backend-only control wake".to_string(),
                scope: None,
            },
        );
        let reminder_outcome: crate::collaboration::model::TaskMessageOutcome =
            serde_json::from_value(response_data(request_frame(
                &endpoint,
                &reminder_assignment,
            )))
            .expect("reminder assignment");
        let ServerFrame::Wake(reminder_assignment_wake) = read_frame(&mut listener) else {
            panic!("worker must receive reminder assignment")
        };
        assert_eq!(
            reminder_assignment_wake.message_id,
            reminder_outcome.message.id
        );
        let reminder_lease: Option<crate::collaboration::model::LeasedMessage> =
            serde_json::from_value(response_data(request_frame(&endpoint, &receive)))
                .expect("reminder assignment lease");
        let reminder_lease = reminder_lease.expect("reminder assignment envelope");
        let reminder_accept = request(
            "worker-a",
            2,
            worker_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskAccept {
                task_id: reminder_outcome.task.id.clone(),
                message_id: reminder_lease.message.id,
                lease_epoch: reminder_lease.lease_epoch as u64,
                lease_token: reminder_lease.lease_token,
            },
        );
        let _: crate::collaboration::model::Task =
            serde_json::from_value(response_data(request_frame(&endpoint, &reminder_accept)))
                .expect("accept reminder assignment");
        let reminder_start = request(
            "worker-a",
            2,
            worker_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskStart {
                task_id: reminder_outcome.task.id.clone(),
            },
        );
        let _: crate::collaboration::model::Task =
            serde_json::from_value(response_data(request_frame(&endpoint, &reminder_start)))
                .expect("start reminder assignment");
        service
            .observe_ready_runtime_state("terminal-worker", 2, RuntimeState::Busy)
            .expect("worker busy edge");
        service
            .observe_ready_runtime_state("terminal-worker", 2, RuntimeState::Idle)
            .expect("worker idle edge");
        broker.notifier().notify();
        let ServerFrame::Wake(first_control_wake) = read_frame(&mut listener) else {
            panic!("worker must receive report-required control wake")
        };
        assert_eq!(first_control_wake.kind, "report_required");
        assert_eq!(first_control_wake.sender_alias, "teak-broker");
        assert_eq!(
            first_control_wake.task_id.as_deref(),
            Some(reminder_outcome.task.id.as_str())
        );
        let durable_control_id = first_control_wake.message_id;

        listener
            .get_mut()
            .shutdown(std::net::Shutdown::Both)
            .expect("disconnect original listener");
        drop(listener);
        let mut follow_ups = Vec::new();
        for text in [
            "first while listener is offline",
            "second while listener is offline",
        ] {
            let follow_up = request(
                "main",
                1,
                leader_secret,
                Some(Uuid::new_v4().to_string()),
                ClientOperation::Send {
                    to_alias: "worker-a".to_string(),
                    kind: OutboundMessageKind::Message,
                    task_id: None,
                    text: text.to_string(),
                },
            );
            let persisted: crate::collaboration::model::Message =
                serde_json::from_value(response_data(request_frame(&endpoint, &follow_up)))
                    .expect("offline follow-up message");
            follow_ups.push(persisted);
        }
        assert_ne!(follow_ups[0].id, follow_ups[1].id);

        // Broker-only restart: the DB and exact PTY generation remain alive.
        // Both committed messages must survive and reconnect in edge FIFO.
        broker.shutdown();
        broker = start_broker(service.clone(), endpoint.clone()).expect("restarted broker");

        let reconnect_deadline = Instant::now() + Duration::from_secs(3);
        let mut replacement = loop {
            let mut stream = UnixStream::connect(&endpoint).expect("replacement connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("replacement timeout");
            stream
                .write_all(&encode_json_line(&listen_request).unwrap())
                .expect("replacement request");
            let mut candidate = BufReader::new(stream);
            let ServerFrame::Response(response) = read_frame(&mut candidate) else {
                panic!("replacement listener must receive readiness")
            };
            if response.status == ResponseStatus::Ok {
                break candidate;
            }
            assert_eq!(
                response.error.expect("replacement rejection").code,
                "listener_already_active"
            );
            assert!(
                Instant::now() < reconnect_deadline,
                "disconnected listener did not release its registry permit"
            );
            thread::sleep(Duration::from_millis(25));
        };
        for expected in &follow_ups {
            let ServerFrame::Wake(replacement_wake) = read_frame(&mut replacement) else {
                panic!("replacement listener must receive pending wake")
            };
            assert_eq!(replacement_wake.message_id, expected.id);

            let leased: Option<crate::collaboration::model::LeasedMessage> =
                serde_json::from_value(response_data(request_frame(&endpoint, &receive)))
                    .expect("lease offline message after restart");
            let leased = leased.expect("one queued offline message");
            assert_eq!(leased.message.id, expected.id);
            assert_eq!(leased.message.payload_text, expected.payload_text);

            let ack = request(
                "worker-a",
                2,
                worker_secret,
                Some(Uuid::new_v4().to_string()),
                ClientOperation::InboxAck {
                    message_id: leased.message.id,
                    lease_epoch: leased.lease_epoch as u64,
                    lease_token: leased.lease_token,
                },
            );
            let acknowledged: crate::collaboration::model::Message =
                serde_json::from_value(response_data(request_frame(&endpoint, &ack)))
                    .expect("acknowledge offline message");
            assert_eq!(acknowledged.state, MessageState::Acknowledged);
        }
        let ServerFrame::Wake(replayed_control_wake) = read_frame(&mut replacement) else {
            panic!("reconnected worker must receive durable report-required wake")
        };
        assert_eq!(replayed_control_wake.message_id, durable_control_id);
        assert_eq!(replayed_control_wake.kind, "report_required");
        assert_eq!(replayed_control_wake.sender_alias, "teak-broker");
        assert_eq!(
            replayed_control_wake.task_id.as_deref(),
            Some(reminder_outcome.task.id.as_str())
        );

        let reminder_report = request(
            "worker-a",
            2,
            worker_secret,
            Some(Uuid::new_v4().to_string()),
            ClientOperation::TaskReport {
                task_id: reminder_outcome.task.id,
                status: ReportStatus::Completed,
                summary: "explicit report clears durable control".to_string(),
            },
        );
        let _: crate::collaboration::model::TaskMessageOutcome =
            serde_json::from_value(response_data(request_frame(&endpoint, &reminder_report)))
                .expect("explicit reminder report");
        let ServerFrame::Response(no_more) = request_frame(&endpoint, &receive) else {
            panic!("empty inbox must return a response")
        };
        assert_eq!(no_more.status, ResponseStatus::Ok, "{:?}", no_more.error);
        assert!(
            no_more.data.is_none() || no_more.data == Some(Value::Null),
            "empty inbox must not contain another delivery"
        );

        broker.shutdown();
        let _ = std::fs::remove_dir_all(root);
    }

    /// Opt-in local compatibility gate for the exact Grok Build version in
    /// `grok.rs`. This test makes two real headless Grok sessions use the
    /// production hidden helper and broker. It is ignored because it consumes
    /// multiple model turns and requires a logged-in local Grok installation.
    ///
    /// Build the helper first, then run serially:
    ///
    /// ```text
    /// cargo build --bin teak-cli
    /// TEAK_COLLAB_GROK_BINARY=/absolute/path/to/grok \
    /// TEAK_COLLAB_HELPER_BINARY="$PWD/target/debug/teak-cli" \
    /// cargo test --bin teak-cli \
    ///   collaboration::broker::tests::real_grok_two_session_collaboration_end_to_end \
    ///   -- --ignored --nocapture --test-threads=1
    /// ```
    /// Set `TEAK_COLLAB_E2E_PRESERVE_PRIVATE_DIAGNOSTICS=1` only for local
    /// debugging. On failure it keeps the owner-only 0700 test root and 0600
    /// streams after generated native sessions have been deleted. Default CI
    /// and local runs remove them.
    #[cfg(unix)]
    #[test]
    #[ignore = "requires an authenticated local Grok Build 1.0.5 and consumes model turns"]
    fn real_grok_two_session_collaboration_end_to_end() {
        const WORKER_SEED_MARKER: &str = "TEAK_REAL_E2E_WORKER_SEED_V1";
        const LEADER_SEED_MARKER: &str = "TEAK_REAL_E2E_LEADER_SEED_V1";
        const WORKER_BOOTSTRAP_MARKER: &str = "TEAK_REAL_E2E_WORKER_BOOTSTRAP_V1";
        const LEADER_BOOTSTRAP_MARKER: &str = "TEAK_REAL_E2E_LEADER_BOOTSTRAP_V1";

        let grok_binary = required_e2e_binary("TEAK_COLLAB_GROK_BINARY");
        let helper_binary = required_e2e_binary("TEAK_COLLAB_HELPER_BINARY");
        let probe = probe_grok(&grok_binary).expect("probe real Grok binary");
        assert!(
            probe.supported,
            "real Grok binary is outside the pinned compatibility matrix: {:?}",
            probe.unsupported_reason
        );

        let suffix = &Uuid::new_v4().simple().to_string()[..12];
        let root = PathBuf::from("/tmp").join(format!("teak-grok-e2e-{suffix}"));
        let workspace = root.join("workspace");
        let journal_dir = root.join("journal");
        let leader_body_dir = root.join("leader-bodies");
        let worker_body_dir = root.join("worker-bodies");
        for directory in [
            &root,
            &workspace,
            &journal_dir,
            &leader_body_dir,
            &worker_body_dir,
        ] {
            create_private_directory(directory);
        }
        // Exercise the production no-space helper shim. This is essential for
        // the normal macOS application path (`Teak CLI.app`), because Grok
        // 1.0.5 can drop quotes when reproducing a permission-scoped command.
        let helper = install_helper_shim(&root.join("bin"), &helper_binary)
            .expect("install private no-space helper shim");

        let service = Arc::new(CollaborationService::in_memory().expect("service"));
        service.set_global_enabled(true).expect("global on");
        let leader_session = Uuid::new_v4().to_string();
        let worker_session = Uuid::new_v4().to_string();
        let team = service
            .create_team(NewTeam {
                name: "Real Grok E2E".to_string(),
                workspace_fingerprint: workspace.to_string_lossy().to_string(),
                enabled: false,
            })
            .expect("team");
        let config = service
            .replace_team_config(
                &team.id,
                ReplaceTeamConfigRequest {
                    name: team.name,
                    workspace_fingerprint: team.workspace_fingerprint,
                    members: vec![
                        TeamConfigMemberInput {
                            id: None,
                            alias: "main".to_string(),
                            display_name: "Main".to_string(),
                            avatar_id: "cedar".to_string(),
                            role: Role::Leader,
                            enabled: true,
                            grok_session_id: Some(leader_session.clone()),
                        },
                        TeamConfigMemberInput {
                            id: None,
                            alias: "worker-a".to_string(),
                            display_name: "Worker A".to_string(),
                            avatar_id: "moss".to_string(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some(worker_session.clone()),
                        },
                    ],
                },
            )
            .expect("roster");
        service.set_team_enabled(&team.id, true).expect("team on");
        let leader = config
            .members
            .iter()
            .find(|member| member.role == Role::Leader)
            .expect("leader");
        let worker = config
            .members
            .iter()
            .find(|member| member.role == Role::Worker)
            .expect("worker");
        let leader_binding = config
            .bindings
            .iter()
            .find(|binding| binding.member_id == leader.id && binding.released_at.is_none())
            .expect("leader binding");
        let worker_binding = config
            .bindings
            .iter()
            .find(|binding| binding.member_id == worker.id && binding.released_at.is_none())
            .expect("worker binding");

        // Capabilities remain environment-only. Never include these values in
        // prompts, argv, success output, or assertion messages.
        let leader_secret = format!("e2e.{}", Uuid::new_v4().simple());
        let worker_secret = format!("e2e.{}", Uuid::new_v4().simple());
        let leader_generation = 10_001;
        let worker_generation = 20_001;
        let leader_runtime = service
            .register_runtime(NewRuntime {
                member_id: leader.id.clone(),
                binding_id: leader_binding.id.clone(),
                terminal_session_id: Uuid::new_v4().to_string(),
                terminal_generation: leader_generation,
                observed_grok_session_id: leader_session.clone(),
                process_id: None,
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(leader_secret.clone()),
                token_epoch: 1,
                attested_workspace_fingerprint: workspace.to_string_lossy().to_string(),
                grok_version: probe.version.to_string(),
                helper_protocol_version: PROTOCOL_VERSION.to_string(),
                capability_probe_result: "real-grok-e2e".to_string(),
                listener_state: ListenerState::Connecting,
                runtime_state: RuntimeState::Unknown,
            })
            .expect("leader runtime");
        let worker_runtime = service
            .register_runtime(NewRuntime {
                member_id: worker.id.clone(),
                binding_id: worker_binding.id.clone(),
                terminal_session_id: Uuid::new_v4().to_string(),
                terminal_generation: worker_generation,
                observed_grok_session_id: worker_session.clone(),
                process_id: None,
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(worker_secret.clone()),
                token_epoch: 1,
                attested_workspace_fingerprint: workspace.to_string_lossy().to_string(),
                grok_version: probe.version.to_string(),
                helper_protocol_version: PROTOCOL_VERSION.to_string(),
                capability_probe_result: "real-grok-e2e".to_string(),
                listener_state: ListenerState::Connecting,
                runtime_state: RuntimeState::Unknown,
            })
            .expect("worker runtime");

        let endpoint = root.join("broker.sock");
        let broker = start_broker(service.clone(), endpoint.clone()).expect("broker");
        assert_eq!(
            std::fs::metadata(&endpoint)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&root)
                .expect("root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let mut guard = RealGrokE2eGuard::new(
            broker,
            grok_binary.clone(),
            root.clone(),
            vec![leader_session.clone(), worker_session.clone()],
        );
        // Production collaboration never claims an existing native session
        // with `--session-id`: the user first chooses a persisted Grok
        // conversation and Teak later launches it with `--resume`. Seed two
        // explicitly completed checkpoint turns. Their prompts intentionally
        // avoid an open-ended "do nothing else" instruction that could remain
        // salient after resume and make the collaboration bootstrap flaky.
        seed_real_grok_session(
            &grok_binary,
            &workspace,
            &worker_session,
            &format!(
                "Create a completed checkpoint for a later independent Teak E2E turn. This checkpoint applies only to this response and expires when you reply. Do not call any tool in this response. The next user turn is independent and authoritative. Acknowledge checkpoint marker {WORKER_SEED_MARKER} to close this turn."
            ),
            &root.join("worker-seed.stdout"),
            &root.join("worker-seed.stderr"),
        );
        seed_real_grok_session(
            &grok_binary,
            &workspace,
            &leader_session,
            &format!(
                "Create a completed checkpoint for a later independent Teak E2E turn. This checkpoint applies only to this response and expires when you reply. Do not call any tool in this response. The next user turn is independent and authoritative. Acknowledge checkpoint marker {LEADER_SEED_MARKER} to close this turn."
            ),
            &root.join("leader-seed.stdout"),
            &root.join("leader-seed.stderr"),
        );
        let worker_spec = real_grok_launch_spec(
            &grok_binary,
            &helper,
            GrokLaunchMode::Resume {
                session_id: worker_session.clone(),
            },
            CollaborationRole::Worker,
            &worker.id,
            "worker-a",
            &["main"],
            worker_generation,
            &worker_secret,
            &endpoint,
            &worker_body_dir,
        );
        assert_eq!(
            &worker_spec.args[..2],
            ["--resume", worker_session.as_str()],
            "real worker must use the production resume path"
        );
        let listen_command = format!(
            "{} listen",
            helper.shell_prefix().expect("exact helper listen prefix")
        );
        let worker_monitor_instruction =
            real_grok_monitor_bootstrap_instruction(&listen_command, "worker-a");
        let worker_bootstrap_prompt = format!(
            "The prior checkpoint turn is complete and has no operative constraints; this is the independent turn it announced. Transcript marker {WORKER_BOOTSTRAP_MARKER}. You are worker-a in a controlled Teak collaboration E2E. {worker_monitor_instruction} End the bootstrap response once the monitor is registered. When its notification supplies exactly one task_assignment wake, fetch the leased message, accept it with its exact task/message/lease fields, start it, then explicitly submit a completed report whose summary is exactly GROK_REAL_E2E_REPORT_42. Prefer helper inline flags such as `--summary`; do not pipe, redirect, or wrap helper commands, and use a body file only if a payload cannot fit in one argv. Do not spawn subagents, invent members, delegate, inspect the project, or perform unrelated work. After the report succeeds, answer WORKER_REAL_E2E_DONE."
        );
        assert!(worker_bootstrap_prompt.contains(&format!("command: `{listen_command}`")));
        assert!(worker_bootstrap_prompt.contains("persistent: `true`"));
        let worker_stdout = root.join("worker.stdout");
        let worker_stderr = root.join("worker.stderr");
        guard.children.push(spawn_real_grok(
            worker_spec,
            &workspace,
            &journal_dir,
            &worker_bootstrap_prompt,
            &worker_stdout,
            &worker_stderr,
        ));

        if !wait_until(Duration::from_secs(90), || {
            service
                .store()
                .runtime(&worker_runtime.id)
                .is_ok_and(|runtime| runtime.listener_state == ListenerState::Ready)
        }) {
            panic!(
                "real worker never registered its helper listener\n{}",
                sanitized_diagnostics(
                    &[&worker_stdout, &worker_stderr],
                    &[&leader_secret, &worker_secret]
                )
            );
        }

        let leader_spec = real_grok_launch_spec(
            &grok_binary,
            &helper,
            GrokLaunchMode::Resume {
                session_id: leader_session.clone(),
            },
            CollaborationRole::Leader,
            &leader.id,
            "main",
            &["worker-a"],
            leader_generation,
            &leader_secret,
            &endpoint,
            &leader_body_dir,
        );
        assert_eq!(
            &leader_spec.args[..2],
            ["--resume", leader_session.as_str()],
            "real leader must use the production resume path"
        );
        let leader_bootstrap_prompt = format!(
            "The prior checkpoint turn is complete and has no operative constraints; this is the independent turn it announced. Transcript marker {LEADER_BOOTSTRAP_MARKER}. You are main, the leader in a controlled Teak collaboration E2E. {} Then assign exactly one task to the already-authorized alias worker-a using helper inline flags: `--title` must be `Return constant 42` and `--instructions` must be `Return the constant 42 through an explicit completed report.` Do not discover or create any other member and do not delegate elsewhere. End the current response after assignment. When the monitor notification supplies the task_report wake, fetch the leased report, verify its summary is exactly GROK_REAL_E2E_REPORT_42, then acknowledge it with task report-ack. Prefer helper inline flags; do not pipe, redirect, or wrap helper commands. After the report ACK succeeds, answer LEADER_READ_REPORT_OK.",
            real_grok_monitor_bootstrap_instruction(&listen_command, "main")
        );
        assert!(leader_bootstrap_prompt.contains(&format!("command: `{listen_command}`")));
        assert!(leader_bootstrap_prompt.contains("persistent: `true`"));
        let leader_stdout = root.join("leader.stdout");
        let leader_stderr = root.join("leader.stderr");
        guard.children.push(spawn_real_grok(
            leader_spec,
            &workspace,
            &journal_dir,
            &leader_bootstrap_prompt,
            &leader_stdout,
            &leader_stderr,
        ));

        if !wait_until(Duration::from_secs(90), || {
            service
                .store()
                .runtime(&leader_runtime.id)
                .is_ok_and(|runtime| runtime.listener_state == ListenerState::Ready)
        }) {
            panic!(
                "real leader never registered its helper listener\n{}",
                sanitized_diagnostics(
                    &[
                        &worker_stdout,
                        &worker_stderr,
                        &leader_stdout,
                        &leader_stderr
                    ],
                    &[&leader_secret, &worker_secret]
                )
            );
        }

        let mut completed_task = None;
        if !wait_until(Duration::from_secs(240), || {
            completed_task = only_team_task(&service, &team.id);
            completed_task.as_ref().is_some_and(|task| {
                if task.state != TaskState::ReportedCompleted {
                    return false;
                }
                let Some(report_id) = task.terminal_report_message_id.as_deref() else {
                    return false;
                };
                service.store().message(report_id).is_ok_and(|message| {
                    message.state == MessageState::Acknowledged
                        && message.payload_text == "GROK_REAL_E2E_REPORT_42"
                })
            })
        }) {
            panic!(
                "real Grok sessions did not complete and acknowledge the report\n{}",
                sanitized_diagnostics(
                    &[
                        &worker_stdout,
                        &worker_stderr,
                        &leader_stdout,
                        &leader_stderr
                    ],
                    &[&leader_secret, &worker_secret]
                )
            );
        }

        let task = completed_task.expect("completed task");
        assert_eq!(task.title, "Return constant 42");
        assert_eq!(task.assigner_member_id, leader.id);
        assert_eq!(task.assignee_member_id, worker.id);
        assert_eq!(task.assignee_generation, worker_generation);
        assert!(task.accepted_at.is_some(), "worker must explicitly accept");
        assert!(task.started_at.is_some(), "worker must explicitly start");
        let assignment = service
            .store()
            .message(&task.assignment_message_id)
            .expect("assignment message");
        assert_eq!(assignment.state, MessageState::Acknowledged);

        if !wait_until(Duration::from_secs(60), || {
            streaming_text(&leader_stdout).contains("LEADER_READ_REPORT_OK")
        }) {
            panic!(
                "leader acknowledged the report but did not emit its final read marker\n{}",
                sanitized_diagnostics(
                    &[
                        &worker_stdout,
                        &worker_stderr,
                        &leader_stdout,
                        &leader_stderr
                    ],
                    &[&leader_secret, &worker_secret]
                )
            );
        }
        let worker_monitor_alive = guard.children[0]
            .try_wait()
            .is_ok_and(|status| status.is_none());
        let leader_monitor_alive = guard.children[1]
            .try_wait()
            .is_ok_and(|status| status.is_none());
        if !worker_monitor_alive || !leader_monitor_alive {
            panic!(
                "a persistent Grok monitor exited after the collaboration wake flow\n{}",
                sanitized_diagnostics(
                    &[
                        &worker_stdout,
                        &worker_stderr,
                        &leader_stdout,
                        &leader_stderr
                    ],
                    &[&leader_secret, &worker_secret]
                )
            );
        }
        assert_eq!(
            service
                .store()
                .runtime(&worker_runtime.id)
                .expect("worker runtime after report")
                .listener_state,
            ListenerState::Ready
        );
        assert_eq!(
            service
                .store()
                .runtime(&leader_runtime.id)
                .expect("leader runtime after report ACK")
                .listener_state,
            ListenerState::Ready
        );

        // Stop the live monitor processes before asking Grok to export the
        // sessions. The export remains private and is never included in a
        // panic: it is used only to prove that the original seed turn and the
        // resumed bootstrap turn coexist in the same persisted transcript.
        guard.stop_runtime();
        assert_real_grok_history_preserved(
            &grok_binary,
            &root,
            &worker_session,
            WORKER_SEED_MARKER,
            WORKER_BOOTSTRAP_MARKER,
            "worker",
        );
        assert_real_grok_history_preserved(
            &grok_binary,
            &root,
            &leader_session,
            LEADER_SEED_MARKER,
            LEADER_BOOTSTRAP_MARKER,
            "leader",
        );
        guard
            .finish()
            .expect("clean up generated real Grok E2E sessions");

        // Intentionally identifier- and capability-free evidence for
        // --nocapture and repeated stress runs.
        println!("REAL_GROK_E2E_OK resume=verified history=preserved assignment=acknowledged report=acknowledged monitors=alive");
    }

    #[cfg(unix)]
    fn required_e2e_binary(name: &str) -> PathBuf {
        let value = std::env::var_os(name).unwrap_or_else(|| {
            panic!("{name} must name an absolute executable for the ignored real Grok E2E")
        });
        let path = PathBuf::from(value);
        assert!(path.is_absolute(), "{name} must be absolute");
        std::fs::canonicalize(path).unwrap_or_else(|error| panic!("invalid {name}: {error}"))
    }

    #[cfg(unix)]
    fn create_private_directory(path: &Path) {
        std::fs::create_dir_all(path).expect("create private E2E directory");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("protect private E2E directory");
    }

    #[cfg(unix)]
    fn seed_real_grok_session(
        grok_binary: &Path,
        workspace: &Path,
        session_id: &str,
        prompt: &str,
        stdout_path: &Path,
        stderr_path: &Path,
    ) {
        let stdout = private_output_file(stdout_path);
        let stderr = private_output_file(stderr_path);
        let mut child = Command::new(grok_binary)
            .args([
                "--cwd",
                &workspace.to_string_lossy(),
                "--session-id",
                session_id,
                "--no-subagents",
                "--disable-web-search",
                "--no-plan",
                "--permission-mode",
                "dontAsk",
                // A seed is only a persisted checkpoint, but Grok may spend
                // one agentic turn recovering from an unnecessary denied tool
                // attempt before emitting the acknowledgement. A limit of one
                // made this ignored compatibility smoke fail before `--resume`
                // with `max turns reached`, hiding the surface under test.
                "--max-turns",
                "3",
                "--output-format",
                "streaming-json",
                "--single",
                prompt,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("start real Grok seed turn");
        let status =
            wait_for_child_bounded(&mut child, Duration::from_secs(120), "real Grok seed turn")
                .unwrap_or_else(|error| {
                    panic!(
                        "{error}\n{}",
                        sanitized_diagnostics(&[stdout_path, stderr_path], &[])
                    )
                });
        assert!(
            status.success(),
            "real Grok seed exited unsuccessfully\n{}",
            sanitized_diagnostics(&[stdout_path, stderr_path], &[])
        );
    }

    #[cfg(unix)]
    fn real_grok_launch_spec(
        grok_binary: &Path,
        helper: &HelperInvocation,
        mode: GrokLaunchMode,
        role: CollaborationRole,
        member_id: &str,
        alias: &str,
        allowed_aliases: &[&str],
        generation: i64,
        secret: &str,
        endpoint: &Path,
        body_dir: &Path,
    ) -> GrokLaunchSpec {
        let rules = CollaborationRules {
            role,
            self_alias: alias.to_string(),
            allowed_aliases: allowed_aliases
                .iter()
                .map(|alias| (*alias).to_string())
                .collect(),
        }
        .render(helper)
        .expect("render collaboration rules");
        build_launch_spec(GrokLaunchConfig {
            binary: grok_binary.to_path_buf(),
            mode,
            rules,
            allow_rules: vec![helper.grok_allow_rule().expect("helper allow")],
            deny_rules: Vec::new(),
            helper: helper.clone(),
            endpoint: endpoint.to_string_lossy().to_string(),
            body_dir: Some(body_dir.to_string_lossy().to_string()),
            member_id: member_id.to_string(),
            member_alias: alias.to_string(),
            generation: generation.to_string(),
            auth: AuthProof::Bearer {
                token: secret.to_string(),
            },
        })
        .expect("build real Grok launch spec")
    }

    #[cfg(unix)]
    fn real_grok_monitor_bootstrap_instruction(listen_command: &str, alias: &str) -> String {
        format!(
            "Use Grok's `monitor` tool exactly once with these parameters: command: `{listen_command}`; description: `Teak collaboration inbox for {alias}`; persistent: `true`. Do not use background Bash and do not alter, chain, pipe, redirect, or wrap the command."
        )
    }

    #[cfg(unix)]
    fn spawn_real_grok(
        mut spec: GrokLaunchSpec,
        workspace: &Path,
        journal_dir: &Path,
        prompt: &str,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Child {
        spec.args.extend([
            "--cwd".to_string(),
            workspace.to_string_lossy().to_string(),
            "--disable-web-search".to_string(),
            "--no-plan".to_string(),
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
            "--max-turns".to_string(),
            "30".to_string(),
            "--output-format".to_string(),
            "streaming-json".to_string(),
            "--single".to_string(),
            prompt.to_string(),
        ]);
        let stdout = private_output_file(stdout_path);
        let stderr = private_output_file(stderr_path);
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(workspace)
            .env(ENV_JOURNAL_DIR, journal_dir)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        for (name, value) in spec.extra_env {
            command.env(name, value);
        }
        command.spawn().expect("spawn real Grok session")
    }

    #[cfg(unix)]
    fn private_output_file(path: &Path) -> File {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .expect("open private E2E output");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .expect("protect private E2E output");
        file
    }

    #[cfg(unix)]
    fn wait_for_child_bounded(
        child: &mut Child,
        timeout: Duration,
        label: &str,
    ) -> Result<std::process::ExitStatus, String> {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{label} timed out after {}s", timeout.as_secs()));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("could not wait for {label}: {error}"));
                }
            }
        }
    }

    #[cfg(unix)]
    fn assert_real_grok_history_preserved(
        grok_binary: &Path,
        root: &Path,
        session_id: &str,
        seed_marker: &str,
        bootstrap_marker: &str,
        label: &str,
    ) {
        let export_path = root.join(format!("{label}.export.md"));
        let stderr_path = root.join(format!("{label}.export.stderr"));
        let stdout = private_output_file(&export_path);
        let stderr = private_output_file(&stderr_path);
        let mut child = Command::new(grok_binary)
            .args(["export", session_id])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("start private Grok history export");
        let status = wait_for_child_bounded(
            &mut child,
            Duration::from_secs(30),
            "private Grok history export",
        )
        .unwrap_or_else(|error| {
            panic!(
                "{error}\n{}",
                sanitized_diagnostics(&[&export_path, &stderr_path], &[])
            )
        });
        assert!(
            status.success(),
            "private Grok history export exited unsuccessfully\n{}",
            sanitized_diagnostics(&[&export_path, &stderr_path], &[])
        );

        const MAX_EXPORT_BYTES: u64 = 8 * 1024 * 1024;
        let bytes = std::fs::metadata(&export_path)
            .expect("private Grok history export metadata")
            .len();
        assert!(
            bytes <= MAX_EXPORT_BYTES,
            "private Grok history export exceeded the 8 MiB verification bound"
        );
        let transcript = std::fs::read_to_string(&export_path)
            .expect("read private Grok history export as UTF-8");
        assert!(
            transcript.contains(seed_marker),
            "resumed Grok transcript lost its completed seed checkpoint"
        );
        assert!(
            transcript.contains(bootstrap_marker),
            "resumed Grok bootstrap was not appended to the seeded transcript"
        );
    }

    #[cfg(unix)]
    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(250));
        }
        predicate()
    }

    #[cfg(unix)]
    fn only_team_task(
        service: &CollaborationService,
        team_id: &str,
    ) -> Option<super::super::model::Task> {
        let connection = service.store().lock().ok()?;
        let mut statement = connection
            .prepare("SELECT id FROM collab_task WHERE team_id=?1 ORDER BY created_at,id")
            .ok()?;
        let ids = statement
            .query_map([team_id], |row| row.get::<_, String>(0))
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        drop(statement);
        drop(connection);
        if ids.len() != 1 {
            return None;
        }
        service.store().task(&ids[0]).ok()
    }

    #[cfg(unix)]
    fn read_bounded(path: &Path) -> String {
        let Ok(mut file) = File::open(path) else {
            return String::new();
        };
        const MAX_TAIL_BYTES: u64 = 1024 * 1024;
        let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if length > MAX_TAIL_BYTES {
            let _ = file.seek(SeekFrom::Start(length - MAX_TAIL_BYTES));
        }
        let mut bytes = Vec::new();
        let _ = std::io::Read::by_ref(&mut file)
            .take(MAX_TAIL_BYTES)
            .read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[cfg(unix)]
    fn streaming_text(path: &Path) -> String {
        read_bounded(path)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|event| {
                event
                    .get("data")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    #[cfg(unix)]
    fn sanitized_diagnostics(paths: &[&Path], secrets: &[&str]) -> String {
        paths
            .iter()
            .map(|path| {
                let bytes = std::fs::metadata(path)
                    .map(|metadata| metadata.len())
                    .unwrap_or_default();
                let content = read_bounded(path);
                let mut event_count = 0usize;
                let mut text_count = 0usize;
                let mut failed_tool_updates = 0usize;
                let mut end_count = 0usize;
                let mut tool_calls = std::collections::HashMap::<String, (String, String)>::new();
                let mut failed_tools = Vec::new();
                let mut end_reasons = Vec::new();
                let mut stream_error_categories = Vec::new();
                for event in content
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                {
                    event_count += 1;
                    match event.get("type").and_then(Value::as_str) {
                        Some("text") => text_count += 1,
                        Some("tool_call") => {
                            if let Some(tool_call_id) =
                                event.get("toolCallId").and_then(Value::as_str)
                            {
                                tool_calls.insert(
                                    tool_call_id.to_string(),
                                    (
                                        safe_tool_label(
                                            event.get("toolName").and_then(Value::as_str),
                                        ),
                                        safe_tool_title(
                                            event.get("title").and_then(Value::as_str),
                                        ),
                                    ),
                                );
                            }
                        }
                        Some("tool_call_update")
                            if event.get("status").and_then(Value::as_str) == Some("failed") =>
                        {
                            failed_tool_updates += 1;
                            if failed_tools.len() < 4 {
                                let (tool, title) = event
                                    .get("toolCallId")
                                    .and_then(Value::as_str)
                                    .and_then(|tool_call_id| tool_calls.get(tool_call_id))
                                    .cloned()
                                    .unwrap_or_else(|| ("unknown".to_string(), "unknown".to_string()));
                                failed_tools.push(format!(
                                    "tool={tool},title={title},status=failed,category={}",
                                    safe_failure_category(&event)
                                ));
                            }
                        }
                        Some("end") => {
                            end_count += 1;
                            if end_reasons.len() < 4 {
                                end_reasons.push(safe_stop_reason(
                                    event.get("stopReason").and_then(Value::as_str),
                                ));
                            }
                        }
                        Some("error") if stream_error_categories.len() < 4 => {
                            stream_error_categories.push(safe_failure_category(&event));
                        }
                        _ => {}
                    }
                }
                let unparsed_stream_category = if event_count == 0 && !content.trim().is_empty() {
                    safe_failure_category_text(&content)
                } else {
                    "none"
                };
                // Never dump model/tool streams here: inbox lease tokens are
                // intentionally present in those private files and are not
                // known to the harness ahead of time. A metadata-only summary
                // keeps failure output capability-free.
                let mut text = format!(
                    "bytes={bytes} parsed_events={event_count} text_events={text_count} failed_tool_updates={failed_tool_updates} failed_tools=[{}] end_events={end_count} end_reasons=[{}] stream_error_categories=[{}] unparsed_stream_category={unparsed_stream_category} worker_marker={} leader_marker={} (contents suppressed)",
                    failed_tools.join(";"),
                    end_reasons.join(","),
                    stream_error_categories.join(","),
                    streaming_text(path).contains("WORKER_REAL_E2E_DONE"),
                    streaming_text(path).contains("LEADER_READ_REPORT_OK")
                );
                for secret in secrets {
                    text = text.replace(secret, "[REDACTED]");
                }
                format!(
                    "{}:\n{}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(unix)]
    fn safe_tool_label(value: Option<&str>) -> String {
        safe_known_label(
            value,
            &[
                ("monitor", "monitor"),
                ("bash", "shell"),
                ("shell", "shell"),
                ("write", "write"),
                ("read", "read"),
                ("edit", "edit"),
                ("grep", "search"),
                ("search", "search"),
                ("agent", "agent"),
            ],
        )
    }

    #[cfg(unix)]
    fn safe_tool_title(value: Option<&str>) -> String {
        safe_known_label(
            value,
            &[
                ("monitor", "monitor"),
                ("background", "background_command"),
                ("bash", "shell_command"),
                ("shell", "shell_command"),
                ("command", "shell_command"),
                ("write", "write"),
                ("read", "read"),
                ("edit", "edit"),
                ("search", "search"),
            ],
        )
    }

    #[cfg(unix)]
    fn safe_known_label(value: Option<&str>, known: &[(&str, &str)]) -> String {
        let Some(value) = value else {
            return "unknown".to_string();
        };
        let value = value.to_ascii_lowercase();
        known
            .iter()
            .find_map(|(needle, label)| value.contains(needle).then_some((*label).to_string()))
            .unwrap_or_else(|| "other".to_string())
    }

    #[cfg(unix)]
    fn safe_failure_category(event: &Value) -> &'static str {
        // Classification is intentionally lossy. The source event can contain
        // commands, body text, and lease capabilities; none of it may reach a
        // panic or CI log.
        safe_failure_category_text(&event.to_string())
    }

    #[cfg(unix)]
    fn safe_failure_category_text(text: &str) -> &'static str {
        let text = text.to_ascii_lowercase();
        if text.contains("max turns reached") || text.contains("max_turns_reached") {
            "max_turns"
        } else if [
            "permission",
            "not permitted",
            "denied",
            "not allowed",
            "approval",
            "requires confirmation",
            "blocked by policy",
        ]
        .iter()
        .any(|needle| text.contains(needle))
        {
            "permission_denied"
        } else if ["command not found", "no such file", "enoent"]
            .iter()
            .any(|needle| text.contains(needle))
        {
            "command_not_found"
        } else if [
            "syntax error",
            "parse error",
            "unexpected token",
            "invalid command",
            "unterminated",
            "shell operator",
        ]
        .iter()
        .any(|needle| text.contains(needle))
        {
            "command_parse"
        } else if text.contains("timed out") || text.contains("timeout") {
            "timeout"
        } else if [
            "rate limit",
            "rate_limit",
            "quota",
            "usage limit",
            "capacity",
        ]
        .iter()
        .any(|needle| text.contains(needle))
        {
            "rate_limit"
        } else if [
            "authentication failed",
            "authentication_failed",
            "unauthorized",
            "invalid api key",
        ]
        .iter()
        .any(|needle| text.contains(needle))
        {
            "authentication"
        } else if text.contains("invalid request") || text.contains("invalid_request") {
            "invalid_request"
        } else if ["exit code", "non-zero", "exited with"]
            .iter()
            .any(|needle| text.contains(needle))
        {
            "process_failed"
        } else if text.contains("refusal") || text.contains("refused") {
            "model_refusal"
        } else if ["connection refused", "broken pipe", "socket", "transport"]
            .iter()
            .any(|needle| text.contains(needle))
        {
            "transport"
        } else {
            "unknown_tool_failure"
        }
    }

    #[cfg(unix)]
    fn safe_stop_reason(value: Option<&str>) -> String {
        match value {
            Some(
                value @ ("end_turn" | "max_tokens" | "max_turn_requests" | "max_turns_reached"
                | "refusal" | "cancelled" | "tool_use" | "pause_turn"),
            ) => value.to_string(),
            Some(_) => "other".to_string(),
            None => "unknown".to_string(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_grok_bootstrap_instruction_contains_exact_monitor_command() {
        let command = "/tmp/teak-private-bin/teak-collab listen";
        let prompt = real_grok_monitor_bootstrap_instruction(command, "worker-a");
        assert!(prompt.contains(&format!("command: `{command}`")));
        assert!(prompt.contains("description: `Teak collaboration inbox for worker-a`"));
        assert!(prompt.contains("persistent: `true`"));
        assert!(!prompt.contains("from your collaboration rules"));
    }

    #[cfg(unix)]
    #[test]
    fn real_grok_diagnostics_classify_failure_without_disclosing_stream_data() {
        let root = std::env::temp_dir().join(format!(
            "teak-collab-sanitized-diagnostics-{}",
            Uuid::new_v4().simple()
        ));
        create_private_directory(&root);
        let stream_path = root.join("worker.stdout");
        let stderr_path = root.join("worker.stderr");
        let mut stream = private_output_file(&stream_path);
        let capability = "e2e.private-capability";
        let body = "PRIVATE_REPORT_BODY";
        writeln!(
            stream,
            "{}",
            json!({
                "type": "tool_call",
                "toolCallId": "private-call-id",
                "title": format!("Monitor `/tmp/private/teak-collab listen --lease {capability}`"),
                "kind": "execute",
                "status": "in_progress",
                "toolName": "monitor",
                "rawInput": {"command": format!("/tmp/private/teak-collab listen {body}")}
            })
        )
        .expect("write private tool start");
        writeln!(
            stream,
            "{}",
            json!({
                "type": "tool_call_update",
                "toolCallId": "private-call-id",
                "status": "failed",
                "rawOutput": format!("Permission denied for {capability}: {body}"),
                "content": [{"type": "text", "text": body}]
            })
        )
        .expect("write private failed update");
        writeln!(
            stream,
            "{}",
            json!({
                "type": "end",
                "stopReason": "end_turn",
                "sessionId": "private-session-id"
            })
        )
        .expect("write private end");
        drop(stream);
        let mut stderr = private_output_file(&stderr_path);
        writeln!(stderr, "Error: max turns reached; {capability}; {body}")
            .expect("write private stderr");
        drop(stderr);

        let diagnostics = sanitized_diagnostics(&[&stream_path, &stderr_path], &[capability]);
        assert!(diagnostics
            .contains("tool=monitor,title=monitor,status=failed,category=permission_denied"));
        assert!(diagnostics.contains("end_reasons=[end_turn]"));
        assert!(diagnostics.contains("unparsed_stream_category=max_turns"));
        for private in [
            capability,
            body,
            "private-call-id",
            "private-session-id",
            "/tmp/private/teak-collab",
            "Permission denied",
        ] {
            assert!(
                !diagnostics.contains(private),
                "sanitized diagnostics disclosed private stream data"
            );
        }

        std::fs::remove_dir_all(root).expect("remove private diagnostics fixture");
    }

    #[cfg(unix)]
    struct RealGrokE2eGuard {
        broker: BrokerHandle,
        grok_binary: PathBuf,
        root: PathBuf,
        session_ids: Vec<String>,
        children: Vec<Child>,
        preserve_private_diagnostics: bool,
        finished: bool,
    }

    #[cfg(unix)]
    impl RealGrokE2eGuard {
        fn new(
            broker: BrokerHandle,
            grok_binary: PathBuf,
            root: PathBuf,
            session_ids: Vec<String>,
        ) -> Self {
            write_session_cleanup_manifest(&root, &session_ids);
            Self {
                broker,
                grok_binary,
                root,
                session_ids,
                children: Vec::new(),
                preserve_private_diagnostics: std::env::var_os(
                    "TEAK_COLLAB_E2E_PRESERVE_PRIVATE_DIAGNOSTICS",
                )
                .is_some_and(|value| value == "1"),
                finished: false,
            }
        }

        fn stop_runtime(&mut self) {
            for mut child in self.children.drain(..) {
                let _ = child.kill();
                let _ = wait_for_child_bounded(
                    &mut child,
                    Duration::from_secs(5),
                    "real Grok runtime shutdown",
                );
            }
            self.broker.shutdown();
        }

        fn delete_sessions(&mut self) -> Result<(), String> {
            let pending = std::mem::take(&mut self.session_ids);
            let mut failed = Vec::new();
            for session_id in pending {
                if delete_real_grok_session_bounded(&self.grok_binary, &self.root, &session_id)
                    .is_err()
                {
                    failed.push(session_id);
                }
            }
            self.session_ids = failed;
            if self.session_ids.is_empty() {
                Ok(())
            } else {
                write_session_cleanup_manifest(&self.root, &self.session_ids);
                Err(format!(
                    "could not delete {} generated Grok E2E session(s); private cleanup manifest retained at {}",
                    self.session_ids.len(),
                    self.root.join("session-cleanup.txt").display()
                ))
            }
        }

        fn finish(&mut self) -> Result<(), String> {
            self.stop_runtime();
            self.delete_sessions()?;
            std::fs::remove_dir_all(&self.root)
                .map_err(|error| format!("could not remove private Grok E2E root: {error}"))?;
            self.finished = true;
            Ok(())
        }
    }

    #[cfg(unix)]
    fn write_session_cleanup_manifest(root: &Path, session_ids: &[String]) {
        let path = root.join("session-cleanup.txt");
        let mut file = private_output_file(&path);
        for session_id in session_ids {
            writeln!(file, "{session_id}").expect("write private session cleanup manifest");
        }
    }

    #[cfg(unix)]
    fn retain_only_session_cleanup_manifest(root: &Path) {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name() == "session-cleanup.txt" {
                continue;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    #[cfg(unix)]
    fn delete_real_grok_session_bounded(
        grok_binary: &Path,
        root: &Path,
        session_id: &str,
    ) -> Result<(), String> {
        let mut child = Command::new(grok_binary)
            .args(["sessions", "delete", session_id])
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start generated Grok session cleanup: {error}"))?;
        let status = wait_for_child_bounded(
            &mut child,
            Duration::from_secs(30),
            "generated Grok session cleanup",
        )?;
        if status.success() {
            Ok(())
        } else {
            Err("generated Grok session cleanup exited unsuccessfully".to_string())
        }
    }

    #[cfg(unix)]
    impl Drop for RealGrokE2eGuard {
        fn drop(&mut self) {
            if self.finished {
                return;
            }
            self.stop_runtime();
            if let Err(error) = self.delete_sessions() {
                // Model/tool transcripts can contain short-lived lease tokens.
                // If native-session cleanup fails, retain only the owner-only
                // session ID manifest needed for manual recovery.
                retain_only_session_cleanup_manifest(&self.root);
                eprintln!("[collaboration test] {error}");
                return;
            }
            if self.preserve_private_diagnostics {
                let _ = std::fs::remove_file(self.root.join("session-cleanup.txt"));
                eprintln!(
                    "[collaboration test] preserved owner-only private diagnostics at {}",
                    self.root.display()
                );
            } else {
                let _ = std::fs::remove_dir_all(&self.root);
            }
        }
    }

    #[cfg(unix)]
    fn request(
        alias: &str,
        generation: i64,
        secret: &str,
        request_id: Option<String>,
        operation: ClientOperation,
    ) -> ClientRequest {
        ClientRequest::new(
            request_id,
            RuntimeClaim {
                member_alias: alias.to_string(),
                generation: generation.to_string(),
            },
            AuthProof::Bearer {
                token: secret.to_string(),
            },
            operation,
        )
        .unwrap()
    }

    #[cfg(unix)]
    fn request_frame(endpoint: &Path, request: &ClientRequest) -> ServerFrame {
        let mut stream = UnixStream::connect(endpoint).unwrap();
        stream
            .write_all(&encode_json_line(request).unwrap())
            .unwrap();
        read_frame(&mut BufReader::new(stream))
    }

    #[cfg(unix)]
    fn read_frame(reader: &mut impl BufRead) -> ServerFrame {
        let mut line = Vec::new();
        reader.read_until(b'\n', &mut line).unwrap();
        decode_json_line(&line).unwrap()
    }

    #[cfg(unix)]
    fn response_data(frame: ServerFrame) -> Value {
        let ServerFrame::Response(response) = frame else {
            panic!("expected response")
        };
        assert_eq!(response.status, ResponseStatus::Ok, "{:?}", response.error);
        response.data.unwrap()
    }

    #[cfg(unix)]
    fn assert_access_denied(frame: ServerFrame) {
        let ServerFrame::Response(response) = frame else {
            panic!("expected denial response")
        };
        assert_eq!(response.status, ResponseStatus::Error);
        let error = response.error.expect("wire error");
        assert_eq!(error.code, "access_denied");
        assert_eq!(error.message, "collaboration request was denied");
    }
}
