//! Durable domain model for Grok Build collaboration mode.
//!
//! The IDs deliberately remain opaque strings at this boundary.  The broker
//! creates UUID v4 values, while callers may persist and replay them without
//! depending on UUID's serde feature.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub type TeamId = String;
pub type MemberId = String;
pub type BindingId = String;
pub type RuntimeId = String;
pub type MessageId = String;
pub type TaskId = String;
pub type EventId = String;

pub const PROVIDER_GROK_BUILD: &str = "grok-build";
pub const DEFAULT_MESSAGE_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
pub const DEFAULT_TASK_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub const DEFAULT_LEASE_MS: i64 = 30_000;
pub const MAX_DELIVERY_ATTEMPTS: i64 = 5;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_PENDING_MESSAGES_PER_TEAM: i64 = 500;
pub const MAX_MESSAGES_PER_MEMBER_PER_MINUTE: i64 = 30;

pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(crate) trait DbEnum: Sized {
    fn as_db(&self) -> &'static str;
    fn from_db(value: &str) -> Option<Self>;
}

macro_rules! db_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl DbEnum for $name {
            fn as_db(&self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }

            fn from_db(value: &str) -> Option<Self> {
                match value { $($value => Some(Self::$variant),)+ _ => None }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_db())
            }
        }
    };
}

db_enum!(Role {
    Leader => "leader",
    Worker => "worker",
});

db_enum!(AuthMethod {
    PeerIdentity => "peer_identity",
    InheritedFd => "inherited_fd",
    Sidecar => "sidecar",
    EnvBearer => "env_bearer",
});

db_enum!(ListenerState {
    Offline => "offline",
    Connecting => "connecting",
    Ready => "ready",
    Degraded => "degraded",
});

db_enum!(RuntimeState {
    Unknown => "unknown",
    Idle => "idle",
    Busy => "busy",
    WaitingUser => "waiting_user",
    Exited => "exited",
});

db_enum!(MessageKind {
    Message => "message",
    TaskAssignment => "task_assignment",
    Question => "question",
    Progress => "progress",
    TaskReport => "task_report",
    TaskCancel => "task_cancel",
    TaskCancelAck => "task_cancel_ack",
});

db_enum!(MessageState {
    Queued => "queued",
    Suspended => "suspended",
    Leased => "leased",
    Acknowledged => "acknowledged",
    Blocked => "blocked",
    Expired => "expired",
    DeadLetter => "dead_letter",
    Cancelled => "cancelled",
});

impl MessageState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Acknowledged | Self::Blocked | Self::Expired | Self::DeadLetter | Self::Cancelled
        )
    }
}

db_enum!(TaskState {
    Assigned => "assigned",
    Accepted => "accepted",
    Running => "running",
    ReportedCompleted => "reported_completed",
    ReportedFailed => "reported_failed",
    CancelRequested => "cancel_requested",
    Cancelled => "cancelled",
});

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ReportedCompleted | Self::ReportedFailed | Self::Cancelled
        )
    }
}

db_enum!(AttentionState {
    None => "none",
    ReportRequired => "report_required",
    DeliveryFailed => "delivery_failed",
    CancelUnconfirmed => "cancel_unconfirmed",
    UncertainExecution => "uncertain_execution",
});

db_enum!(ActorType {
    User => "user",
    Member => "member",
    Broker => "broker",
    System => "system",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclAction {
    Message,
    AssignTask,
    Report,
    CancelTask,
    AcknowledgeCancel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub id: TeamId,
    pub name: String,
    pub provider: String,
    pub enabled: bool,
    pub workspace_fingerprint: String,
    pub config_revision: i64,
    pub routing_revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTeam {
    pub name: String,
    pub workspace_fingerprint: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: MemberId,
    pub team_id: TeamId,
    pub alias: String,
    pub display_name: String,
    pub avatar_id: String,
    pub role: Role,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMember {
    pub team_id: TeamId,
    pub alias: String,
    pub display_name: String,
    pub avatar_id: String,
    pub role: Role,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: BindingId,
    pub member_id: MemberId,
    pub provider: String,
    pub grok_session_id: String,
    pub bound_at: i64,
    pub released_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewBinding {
    pub member_id: MemberId,
    pub grok_session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamConfigMemberInput {
    pub id: Option<MemberId>,
    pub alias: String,
    pub display_name: String,
    pub avatar_id: String,
    pub role: Role,
    pub enabled: bool,
    pub grok_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceTeamConfigRequest {
    pub name: String,
    pub workspace_fingerprint: String,
    pub members: Vec<TeamConfigMemberInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeamConfiguration {
    pub team: Team,
    pub members: Vec<Member>,
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedBinding {
    pub team: Team,
    pub member: Member,
    pub binding: Binding,
    pub active_runtime: Option<Runtime>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AclEdge {
    pub team_id: TeamId,
    pub from_member_id: MemberId,
    pub to_member_id: MemberId,
    pub can_message: bool,
    pub can_assign_task: bool,
    pub can_report: bool,
    pub can_cancel_task: bool,
    pub can_ack_cancel: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub id: RuntimeId,
    pub member_id: MemberId,
    pub binding_id: BindingId,
    pub terminal_session_id: String,
    pub terminal_generation: i64,
    pub observed_grok_session_id: String,
    pub process_id: Option<i64>,
    pub routing_revision: i64,
    pub auth_method: AuthMethod,
    pub token_epoch: i64,
    pub attested_provider: String,
    pub attested_workspace_fingerprint: String,
    pub grok_version: String,
    pub helper_protocol_version: String,
    pub capability_probe_result: String,
    pub listener_state: ListenerState,
    pub runtime_state: RuntimeState,
    pub last_heartbeat_at: Option<i64>,
    pub started_at: i64,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewRuntime {
    pub member_id: MemberId,
    pub binding_id: BindingId,
    pub terminal_session_id: String,
    pub terminal_generation: i64,
    pub observed_grok_session_id: String,
    pub process_id: Option<i64>,
    pub auth_method: AuthMethod,
    /// Raw bearer presented only at registration. It is never persisted.
    #[serde(skip_serializing)]
    pub bearer_secret: Option<String>,
    pub token_epoch: i64,
    pub attested_workspace_fingerprint: String,
    pub grok_version: String,
    pub helper_protocol_version: String,
    pub capability_probe_result: String,
    pub listener_state: ListenerState,
    pub runtime_state: RuntimeState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerIdentity {
    pub member_id: MemberId,
    pub terminal_generation: i64,
    pub token_epoch: i64,
    /// Required for bearer-backed runtimes; omitted for peer/FD-authenticated IPC.
    #[serde(skip_serializing)]
    pub bearer_secret: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedScope {
    pub team_id: TeamId,
    pub member_id: MemberId,
    pub member_alias: String,
    pub role: Role,
    pub terminal_generation: i64,
    pub token_epoch: i64,
    pub routing_revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: MessageId,
    pub team_id: TeamId,
    pub sender_member_id: MemberId,
    pub sender_generation: i64,
    pub recipient_member_id: MemberId,
    pub recipient_generation: i64,
    pub routing_revision: i64,
    pub kind: MessageKind,
    pub task_id: Option<TaskId>,
    pub reply_to_message_id: Option<MessageId>,
    pub payload_text: String,
    pub request_id: String,
    pub request_fingerprint: String,
    pub retry_of_message_id: Option<MessageId>,
    pub edge_sequence: i64,
    pub state: MessageState,
    pub lease_epoch: i64,
    pub lease_until: Option<i64>,
    pub acknowledged_lease_epoch: Option<i64>,
    pub attempt_count: i64,
    pub not_before: i64,
    pub expires_at: i64,
    pub paused_at: Option<i64>,
    pub blocked_at: Option<i64>,
    pub blocked_reason: Option<String>,
    pub resolution_policy: Option<String>,
    pub last_error_code: Option<String>,
    pub created_at: i64,
    pub acknowledged_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: TaskId,
    pub team_id: TeamId,
    pub assigner_member_id: MemberId,
    pub assignee_member_id: MemberId,
    pub assignee_generation: i64,
    pub assignment_message_id: MessageId,
    pub title: String,
    pub instructions: String,
    pub optional_scope_json: Option<String>,
    pub state: TaskState,
    pub version: i64,
    pub terminal_report_message_id: Option<MessageId>,
    pub cancel_request_message_id: Option<MessageId>,
    pub cancel_ack_message_id: Option<MessageId>,
    pub attention_state: AttentionState,
    pub attention_reason: Option<String>,
    pub attention_since: Option<i64>,
    pub report_reminder_count: i64,
    pub created_at: i64,
    pub accepted_at: Option<i64>,
    pub started_at: Option<i64>,
    pub terminal_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub sequence: i64,
    pub id: EventId,
    pub team_id: TeamId,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub actor_type: ActorType,
    pub actor_member_id: Option<MemberId>,
    pub redacted_metadata_json: String,
    pub created_at: i64,
}

/// Durable, backend-authored listener notification. It carries only an event
/// ID and task ID; no message body or user-controlled sender identity enters
/// the monitor stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ControlWake {
    pub id: EventId,
    pub task_id: TaskId,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub recipient_alias: String,
    pub kind: MessageKind,
    pub task_id: Option<TaskId>,
    pub reply_to_message_id: Option<MessageId>,
    pub payload_text: String,
    pub request_id: String,
    pub retry_of_message_id: Option<MessageId>,
    pub not_before: Option<i64>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignTaskRequest {
    pub assignee_alias: String,
    pub title: String,
    pub instructions: String,
    pub optional_scope_json: Option<String>,
    pub request_id: String,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRequest {
    pub now: i64,
    pub lease_duration_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeasedMessage {
    pub message: Message,
    pub lease_epoch: i64,
    /// Returned once to the current runtime. Only its SHA-256 verifier is stored.
    pub lease_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckMessageRequest {
    pub message_id: MessageId,
    pub lease_epoch: i64,
    pub lease_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptTaskRequest {
    pub task_id: TaskId,
    pub assignment_message_id: MessageId,
    pub lease_epoch: i64,
    pub lease_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportTaskRequest {
    pub task_id: TaskId,
    pub status: ReportStatus,
    pub payload_text: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportAckRequest {
    pub task_id: TaskId,
    pub report_message_id: MessageId,
    pub lease_epoch: i64,
    pub lease_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelTaskRequest {
    pub task_id: TaskId,
    pub reason: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAckRequest {
    pub task_id: TaskId,
    pub cancel_message_id: MessageId,
    pub lease_epoch: i64,
    pub lease_token: String,
    pub payload_text: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskMessageOutcome {
    pub task: Task,
    pub message: Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelOutcome {
    pub task: Task,
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySummary {
    pub leases_requeued: i64,
    pub messages_expired: i64,
    pub messages_dead_lettered: i64,
    pub messages_blocked: i64,
    pub tasks_needing_attention: i64,
}

#[derive(Debug)]
pub enum CollaborationError {
    Database(rusqlite::Error),
    PoisonedLock,
    NotFound(&'static str),
    InvalidInput(String),
    Unauthorized(&'static str),
    StaleGeneration,
    Suspended,
    Conflict(String),
    InvalidState { entity: &'static str, state: String },
    Capacity(&'static str),
}

impl fmt::Display for CollaborationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(f, "collaboration database error: {error}"),
            Self::PoisonedLock => f.write_str("collaboration database lock was poisoned"),
            Self::NotFound(entity) => write!(f, "{entity} not found"),
            Self::InvalidInput(message) => write!(f, "invalid collaboration request: {message}"),
            Self::Unauthorized(code) => write!(f, "collaboration request denied ({code})"),
            Self::StaleGeneration => f.write_str("runtime generation is stale"),
            Self::Suspended => f.write_str("collaboration is suspended"),
            Self::Conflict(message) => write!(f, "collaboration conflict: {message}"),
            Self::InvalidState { entity, state } => {
                write!(f, "{entity} cannot transition from {state}")
            }
            Self::Capacity(limit) => write!(f, "collaboration limit exceeded ({limit})"),
        }
    }
}

impl std::error::Error for CollaborationError {}

impl From<rusqlite::Error> for CollaborationError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}

pub type CollabResult<T> = Result<T, CollaborationError>;
