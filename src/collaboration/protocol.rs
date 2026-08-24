//! Versioned, local-only wire protocol shared by the Teak collaboration
//! broker and the `teak-collab` helper.
//!
//! The protocol deliberately keeps authentication separate from model-supplied
//! fields. `claim` is only a routing assertion; the broker must derive the
//! authoritative member and generation from `auth` plus the local peer.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

pub const PROTOCOL_NAME: &str = "teak-collab";
pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_FRAME_BYTES: usize = 96 * 1024;
pub const MAX_ALIAS_BYTES: usize = 48;
pub const MAX_CODE_BYTES: usize = 96;
pub const MAX_TOKEN_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    pub code: &'static str,
    pub message: String,
}

impl ProtocolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeClaim {
    pub member_alias: String,
    pub generation: String,
}

/// Authentication material is serialized only onto the protected local
/// transport. Do not log or forward this value to stdout.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthProof {
    /// The broker authenticates the helper from local peer credentials and
    /// process ancestry. The claim remains untrusted until that check passes.
    Peer,
    /// An opaque handle resolved by the Teak-owned sidecar/broker.
    Handle { handle: String },
    /// Narrow, short-lived fallback for runtimes where peer/handle auth cannot
    /// be made reliable. It is intentionally redacted from `Debug`.
    Bearer { token: String },
}

impl fmt::Debug for AuthProof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Peer => f.write_str("AuthProof::Peer"),
            Self::Handle { .. } => f.write_str("AuthProof::Handle([REDACTED])"),
            Self::Bearer { .. } => f.write_str("AuthProof::Bearer([REDACTED])"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutboundMessageKind {
    Message,
    Question,
    Progress,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "operation", content = "payload", rename_all = "snake_case")]
pub enum ClientOperation {
    Listen,
    Allowed,
    Health,
    InboxReceive,
    TasksPending,
    Send {
        to_alias: String,
        kind: OutboundMessageKind,
        task_id: Option<String>,
        text: String,
    },
    InboxAck {
        message_id: String,
        lease_epoch: u64,
        lease_token: String,
    },
    TaskAssign {
        to_alias: String,
        title: String,
        instructions: String,
        scope: Option<Value>,
    },
    TaskAccept {
        task_id: String,
        message_id: String,
        lease_epoch: u64,
        lease_token: String,
    },
    TaskStart {
        task_id: String,
    },
    TaskReport {
        task_id: String,
        status: ReportStatus,
        summary: String,
    },
    TaskReportAck {
        task_id: String,
        message_id: String,
        lease_epoch: u64,
        lease_token: String,
    },
    TaskCancel {
        task_id: String,
        reason: Option<String>,
    },
    TaskCancelAck {
        task_id: String,
        message_id: String,
        lease_epoch: u64,
        lease_token: String,
    },
}

impl ClientOperation {
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self,
            Self::Listen | Self::Allowed | Self::Health | Self::InboxReceive | Self::TasksPending
        )
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Listen
            | Self::Allowed
            | Self::Health
            | Self::InboxReceive
            | Self::TasksPending => Ok(()),
            Self::Send {
                to_alias,
                kind,
                task_id,
                text,
            } => {
                validate_alias(to_alias)?;
                match kind {
                    OutboundMessageKind::Message if task_id.is_some() => {
                        return Err(ProtocolError::new(
                            "invalid_task_scope",
                            "plain messages cannot carry a task id",
                        ));
                    }
                    OutboundMessageKind::Question | OutboundMessageKind::Progress
                        if task_id.is_none() =>
                    {
                        return Err(ProtocolError::new(
                            "missing_task_id",
                            "question/progress requires a task id",
                        ));
                    }
                    _ => {}
                }
                if let Some(task_id) = task_id {
                    validate_uuid("task_id", task_id)?;
                }
                validate_text("text", text, false)
            }
            Self::InboxAck {
                message_id,
                lease_epoch,
                lease_token,
            } => validate_lease(message_id, *lease_epoch, lease_token),
            Self::TaskAssign {
                to_alias,
                title,
                instructions,
                scope,
            } => {
                validate_alias(to_alias)?;
                validate_text("title", title, false)?;
                validate_text("instructions", instructions, false)?;
                if title.len() > 512 {
                    return Err(ProtocolError::new(
                        "title_too_large",
                        "task title exceeds 512 UTF-8 bytes",
                    ));
                }
                if let Some(scope) = scope {
                    let encoded = serde_json::to_vec(scope)
                        .map_err(|error| ProtocolError::new("invalid_scope", error.to_string()))?;
                    if encoded.len() > 16 * 1024 {
                        return Err(ProtocolError::new(
                            "scope_too_large",
                            "task scope exceeds 16 KiB",
                        ));
                    }
                }
                Ok(())
            }
            Self::TaskAccept {
                task_id,
                message_id,
                lease_epoch,
                lease_token,
            }
            | Self::TaskReportAck {
                task_id,
                message_id,
                lease_epoch,
                lease_token,
            }
            | Self::TaskCancelAck {
                task_id,
                message_id,
                lease_epoch,
                lease_token,
            } => {
                validate_uuid("task_id", task_id)?;
                validate_lease(message_id, *lease_epoch, lease_token)
            }
            Self::TaskStart { task_id } => validate_uuid("task_id", task_id),
            Self::TaskReport {
                task_id, summary, ..
            } => {
                validate_uuid("task_id", task_id)?;
                validate_text("summary", summary, false)
            }
            Self::TaskCancel { task_id, reason } => {
                validate_uuid("task_id", task_id)?;
                if let Some(reason) = reason {
                    validate_text("reason", reason, true)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClientRequest {
    pub protocol: String,
    pub version: u16,
    pub request_id: Option<String>,
    pub claim: RuntimeClaim,
    pub auth: AuthProof,
    pub operation: ClientOperation,
}

impl ClientRequest {
    pub fn new(
        request_id: Option<String>,
        claim: RuntimeClaim,
        auth: AuthProof,
        operation: ClientOperation,
    ) -> Result<Self, ProtocolError> {
        let request = Self {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            request_id,
            claim,
            auth,
            operation,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_protocol_header(&self.protocol, self.version)?;
        validate_alias(&self.claim.member_alias)?;
        validate_generation(&self.claim.generation)?;
        validate_auth(&self.auth)?;
        self.operation.validate()?;
        match (&self.request_id, self.operation.is_mutating()) {
            (Some(request_id), _) => validate_uuid("request_id", request_id),
            (None, true) => Err(ProtocolError::new(
                "missing_request_id",
                "mutating operations require a request id",
            )),
            (None, false) => Ok(()),
        }
    }
}

/// Authentication is always redacted when a request is debug-formatted.
impl fmt::Debug for ClientRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientRequest")
            .field("protocol", &self.protocol)
            .field("version", &self.version)
            .field("request_id", &self.request_id)
            .field("claim", &self.claim)
            .field("auth", &self.auth)
            .field("operation", &self.operation)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Ok,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerResponse {
    pub protocol: String,
    pub version: u16,
    pub request_id: Option<String>,
    pub status: ResponseStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

impl ServerResponse {
    pub fn validate_for(&self, expected_request_id: Option<&str>) -> Result<(), ProtocolError> {
        validate_protocol_header(&self.protocol, self.version)?;
        if self.request_id.as_deref() != expected_request_id {
            return Err(ProtocolError::new(
                "request_id_mismatch",
                "broker response request id did not match the request",
            ));
        }
        match (&self.status, &self.error) {
            (ResponseStatus::Ok, None) | (ResponseStatus::Error, Some(_)) => {}
            (ResponseStatus::Ok, Some(_)) => {
                return Err(ProtocolError::new(
                    "invalid_response",
                    "successful response carried an error",
                ));
            }
            (ResponseStatus::Error, None) => {
                return Err(ProtocolError::new(
                    "invalid_response",
                    "error response omitted its error envelope",
                ));
            }
        }
        if let Some(error) = &self.error {
            validate_code("error.code", &error.code)?;
            validate_text("error.message", &error.message, true)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WakeEnvelope {
    pub protocol: String,
    pub version: u16,
    pub message_id: String,
    pub kind: String,
    pub sender_alias: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl WakeEnvelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_protocol_header(&self.protocol, self.version)?;
        validate_uuid("message_id", &self.message_id)?;
        validate_code("kind", &self.kind)?;
        validate_alias(&self.sender_alias)?;
        if let Some(task_id) = &self.task_id {
            validate_uuid("task_id", task_id)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "frame", content = "payload", rename_all = "snake_case")]
pub enum ServerFrame {
    Response(ServerResponse),
    Wake(WakeEnvelope),
    /// Heartbeats stay on the socket and are never forwarded to monitor
    /// stdout, otherwise they would create a model turn every interval.
    Heartbeat {
        unix_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StdioInputEnvelope {
    pub protocol: String,
    pub version: u16,
    pub body: Value,
}

impl StdioInputEnvelope {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_protocol_header(&self.protocol, self.version)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StdioOutputEnvelope {
    pub protocol: String,
    pub version: u16,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireError>,
}

impl StdioOutputEnvelope {
    pub fn from_response(response: ServerResponse) -> Self {
        Self {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            ok: response.status == ResponseStatus::Ok,
            request_id: response.request_id,
            data: response.data,
            error: response.error,
        }
    }
}

pub fn encode_json_line<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    let mut encoded = serde_json::to_vec(value)
        .map_err(|error| ProtocolError::new("json_encode_failed", error.to_string()))?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::new(
            "frame_too_large",
            format!("JSON frame exceeds {MAX_FRAME_BYTES} bytes"),
        ));
    }
    encoded.push(b'\n');
    Ok(encoded)
}

pub fn decode_json_line<T: DeserializeOwned>(line: &[u8]) -> Result<T, ProtocolError> {
    if line.is_empty() {
        return Err(ProtocolError::new("empty_frame", "JSON frame is empty"));
    }
    if line.len() > MAX_FRAME_BYTES + 1 {
        return Err(ProtocolError::new(
            "frame_too_large",
            format!("JSON frame exceeds {MAX_FRAME_BYTES} bytes"),
        ));
    }
    let trimmed = line
        .strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line));
    serde_json::from_slice(trimmed)
        .map_err(|error| ProtocolError::new("invalid_json", error.to_string()))
}

pub fn validate_protocol_header(protocol: &str, version: u16) -> Result<(), ProtocolError> {
    if protocol != PROTOCOL_NAME {
        return Err(ProtocolError::new(
            "unsupported_protocol",
            "unexpected local protocol name",
        ));
    }
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::new(
            "unsupported_version",
            format!("unsupported protocol version {version}"),
        ));
    }
    Ok(())
}

pub fn validate_alias(alias: &str) -> Result<(), ProtocolError> {
    if alias.is_empty() || alias.len() > MAX_ALIAS_BYTES {
        return Err(ProtocolError::new(
            "invalid_alias",
            format!("alias must be 1..={MAX_ALIAS_BYTES} bytes"),
        ));
    }
    let mut chars = alias.bytes();
    let first = chars.next().unwrap_or_default();
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(ProtocolError::new(
            "invalid_alias",
            "alias must start with a lowercase ASCII letter or digit",
        ));
    }
    if !chars.all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
    }) {
        return Err(ProtocolError::new(
            "invalid_alias",
            "alias may contain only lowercase ASCII letters, digits, '-' and '_'",
        ));
    }
    Ok(())
}

pub fn validate_generation(generation: &str) -> Result<(), ProtocolError> {
    if generation.is_empty()
        || generation.starts_with('0')
        || !generation.bytes().all(|byte| byte.is_ascii_digit())
        || generation
            .parse::<i64>()
            .ok()
            .is_none_or(|value| value <= 0)
    {
        return Err(ProtocolError::new(
            "invalid_generation",
            "generation must be a canonical positive i64 decimal string",
        ));
    }
    Ok(())
}

pub fn validate_uuid(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| ProtocolError::new("invalid_uuid", format!("{field} must be a UUID")))
}

fn validate_auth(auth: &AuthProof) -> Result<(), ProtocolError> {
    match auth {
        AuthProof::Peer => Ok(()),
        AuthProof::Handle { handle } => validate_secret("capability handle", handle),
        AuthProof::Bearer { token } => validate_secret("capability token", token),
    }
}

fn validate_secret(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() || value.len() > MAX_TOKEN_BYTES {
        return Err(ProtocolError::new(
            "invalid_credential",
            format!("{field} has an invalid length"),
        ));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'=')
    }) {
        return Err(ProtocolError::new(
            "invalid_credential",
            format!("{field} contains invalid characters"),
        ));
    }
    Ok(())
}

fn validate_lease(
    message_id: &str,
    lease_epoch: u64,
    lease_token: &str,
) -> Result<(), ProtocolError> {
    validate_uuid("message_id", message_id)?;
    if lease_epoch == 0 {
        return Err(ProtocolError::new(
            "invalid_lease_epoch",
            "lease epoch must be greater than zero",
        ));
    }
    validate_secret("lease token", lease_token)
}

fn validate_code(field: &'static str, value: &str) -> Result<(), ProtocolError> {
    if value.is_empty() || value.len() > MAX_CODE_BYTES {
        return Err(ProtocolError::new(
            "invalid_code",
            format!("{field} has an invalid length"),
        ));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
    }) {
        return Err(ProtocolError::new(
            "invalid_code",
            format!("{field} contains invalid characters"),
        ));
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str, allow_empty: bool) -> Result<(), ProtocolError> {
    if (!allow_empty && value.is_empty()) || value.len() > MAX_BODY_BYTES {
        return Err(ProtocolError::new(
            "invalid_body",
            format!("{field} must be within the configured UTF-8 byte limit"),
        ));
    }
    if value.contains('\0') {
        return Err(ProtocolError::new(
            "invalid_body",
            format!("{field} contains a NUL byte"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> RuntimeClaim {
        RuntimeClaim {
            member_alias: "worker-a".to_string(),
            generation: "42".to_string(),
        }
    }

    #[test]
    fn auth_debug_is_redacted() {
        let auth = AuthProof::Bearer {
            token: "super-secret".to_string(),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("REDACTED"));
        assert!(!rendered.contains("super-secret"));
    }

    #[test]
    fn mutation_requires_request_id() {
        let error = ClientRequest::new(
            None,
            claim(),
            AuthProof::Peer,
            ClientOperation::TaskStart {
                task_id: "e63f11fd-904a-4fa2-9c6a-23522843571a".to_string(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "missing_request_id");
    }

    #[test]
    fn question_requires_task_scope() {
        let operation = ClientOperation::Send {
            to_alias: "main".to_string(),
            kind: OutboundMessageKind::Question,
            task_id: None,
            text: "Need a decision".to_string(),
        };
        assert_eq!(operation.validate().unwrap_err().code, "missing_task_id");
    }

    #[test]
    fn wake_contains_no_body_and_round_trips() {
        let wake = WakeEnvelope {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            message_id: "b394cddb-2408-4c24-8a55-d5ba8745681a".to_string(),
            kind: "task_assignment".to_string(),
            sender_alias: "main".to_string(),
            task_id: Some("08317ad7-c92c-451f-a5d1-d9b124d7fd84".to_string()),
        };
        let line = encode_json_line(&wake).unwrap();
        let decoded: WakeEnvelope = decode_json_line(&line).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded, wake);
        assert!(!String::from_utf8(line).unwrap().contains("payload_text"));

        let control = WakeEnvelope {
            protocol: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
            message_id: "4187797a-6c11-4f18-a3be-3802e502926b".to_string(),
            kind: "report_required".to_string(),
            sender_alias: "teak-broker".to_string(),
            task_id: Some("c9a63379-d360-4104-ab04-852cc2eed63a".to_string()),
        };
        let control_line = encode_json_line(&control).unwrap();
        let decoded_control: WakeEnvelope = decode_json_line(&control_line).unwrap();
        decoded_control.validate().unwrap();
        assert_eq!(decoded_control, control);
        let rendered = String::from_utf8(control_line).unwrap();
        assert!(!rendered.contains("payload"));
        assert!(!rendered.contains("instructions"));
    }

    #[test]
    fn rejects_oversized_frames_before_parsing() {
        let line = vec![b'x'; MAX_FRAME_BYTES + 2];
        assert_eq!(
            decode_json_line::<Value>(&line).unwrap_err().code,
            "frame_too_large"
        );
    }
}
