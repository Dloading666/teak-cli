//! SQLite persistence and schema migration for collaboration mode.

use super::model::*;
use rusqlite::types::Type;
use rusqlite::{Connection, OpenFlags, Row, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub(crate) const MESSAGE_COLUMNS: &str = "
    id, team_id, sender_member_id, sender_generation,
    recipient_member_id, recipient_generation, routing_revision, kind,
    task_id, reply_to_message_id, payload_text, request_id,
    request_fingerprint, retry_of_message_id, edge_sequence, state,
    lease_epoch, lease_until, acknowledged_lease_epoch, attempt_count,
    not_before, expires_at, paused_at, blocked_at, blocked_reason,
    resolution_policy, last_error_code, created_at, acknowledged_at, updated_at";

pub(crate) const TASK_COLUMNS: &str = "
    id, team_id, assigner_member_id, assignee_member_id, assignee_generation,
    assignment_message_id, title, instructions, optional_scope_json, state,
    version, terminal_report_message_id, cancel_request_message_id,
    cancel_ack_message_id, attention_state, attention_reason, attention_since,
    report_reminder_count, created_at, accepted_at, started_at, terminal_at,
    updated_at";

pub(crate) const RUNTIME_COLUMNS: &str = "
    id, member_id, binding_id, terminal_session_id, terminal_generation,
    observed_grok_session_id, process_id, routing_revision, auth_method,
    token_epoch, attested_provider, attested_workspace_fingerprint,
    grok_version, helper_protocol_version, capability_probe_result,
    listener_state, runtime_state, last_heartbeat_at, started_at, created_at,
    revoked_at";

pub struct CollaborationStore {
    connection: Mutex<Connection>,
}

impl CollaborationStore {
    pub fn open(path: impl AsRef<Path>) -> CollabResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CollaborationError::InvalidInput(format!(
                    "cannot create collaboration directory: {error}"
                ))
            })?;
            set_private_directory_permissions(parent)?;
        }

        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )?;
        set_private_file_permissions(path)?;
        Self::from_connection(connection, true)
    }

    pub fn in_memory() -> CollabResult<Self> {
        Self::from_connection(Connection::open_in_memory()?, false)
    }

    fn from_connection(mut connection: Connection, file_backed: bool) -> CollabResult<Self> {
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000_i64)?;
        if file_backed {
            connection.pragma_update(None, "journal_mode", "WAL")?;
            connection.pragma_update(None, "synchronous", "FULL")?;
        }
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub(crate) fn lock(&self) -> CollabResult<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| CollaborationError::PoisonedLock)
    }

    pub fn team(&self, id: &str) -> CollabResult<Team> {
        let connection = self.lock()?;
        select_team(&connection, id)
    }

    pub fn member(&self, id: &str) -> CollabResult<Member> {
        let connection = self.lock()?;
        select_member(&connection, id)
    }

    pub fn runtime(&self, id: &str) -> CollabResult<Runtime> {
        let connection = self.lock()?;
        select_runtime(&connection, id)
    }

    pub fn message(&self, id: &str) -> CollabResult<Message> {
        let connection = self.lock()?;
        select_message(&connection, id)
    }

    pub fn task(&self, id: &str) -> CollabResult<Task> {
        let connection = self.lock()?;
        select_task(&connection, id)
    }

    pub fn events_after(&self, sequence: i64, limit: i64) -> CollabResult<Vec<Event>> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT sequence, id, team_id, aggregate_type, aggregate_id,
                    event_type, actor_type, actor_member_id,
                    redacted_metadata_json, created_at
             FROM collab_event
             WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
        )?;
        let rows =
            statement.query_map(rusqlite::params![sequence, limit.clamp(1, 10_000)], |row| {
                Ok(Event {
                    sequence: row.get(0)?,
                    id: row.get(1)?,
                    team_id: row.get(2)?,
                    aggregate_type: row.get(3)?,
                    aggregate_id: row.get(4)?,
                    event_type: row.get(5)?,
                    actor_type: enum_at(row, 6)?,
                    actor_member_id: row.get(7)?,
                    redacted_metadata_json: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

fn migrate(connection: &mut Connection) -> CollabResult<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        0 => {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(SCHEMA_V1)?;
            transaction.pragma_update(None, "user_version", 1_i64)?;
            transaction.commit()?;
        }
        1 => {}
        other => {
            return Err(CollaborationError::InvalidInput(format!(
                "collaboration schema version {other} is newer than supported version 1"
            )))
        }
    }
    Ok(())
}

const SCHEMA_V1: &str = r#"
CREATE TABLE collab_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO collab_meta(key, value) VALUES ('global_enabled', '0');

CREATE TABLE collab_team (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    provider TEXT NOT NULL CHECK(provider = 'grok-build'),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
    workspace_fingerprint TEXT NOT NULL,
    config_revision INTEGER NOT NULL DEFAULT 1,
    routing_revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived_at INTEGER
);

CREATE TABLE collab_member (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES collab_team(id),
    alias TEXT NOT NULL,
    display_name TEXT NOT NULL,
    avatar_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('leader', 'worker')),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(team_id, alias)
);
CREATE UNIQUE INDEX collab_one_enabled_leader
ON collab_member(team_id) WHERE role = 'leader' AND enabled = 1;

CREATE TABLE collab_binding (
    id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES collab_member(id),
    provider TEXT NOT NULL CHECK(provider = 'grok-build'),
    grok_session_id TEXT NOT NULL,
    bound_at INTEGER NOT NULL,
    released_at INTEGER
);
CREATE UNIQUE INDEX collab_one_active_binding_per_member
ON collab_binding(member_id) WHERE released_at IS NULL;
CREATE UNIQUE INDEX collab_one_active_grok_session
ON collab_binding(grok_session_id) WHERE released_at IS NULL;

CREATE TABLE collab_acl (
    team_id TEXT NOT NULL REFERENCES collab_team(id),
    from_member_id TEXT NOT NULL REFERENCES collab_member(id),
    to_member_id TEXT NOT NULL REFERENCES collab_member(id),
    can_message INTEGER NOT NULL CHECK(can_message IN (0, 1)),
    can_assign_task INTEGER NOT NULL CHECK(can_assign_task IN (0, 1)),
    can_report INTEGER NOT NULL CHECK(can_report IN (0, 1)),
    can_cancel_task INTEGER NOT NULL CHECK(can_cancel_task IN (0, 1)),
    can_ack_cancel INTEGER NOT NULL CHECK(can_ack_cancel IN (0, 1)),
    PRIMARY KEY(team_id, from_member_id, to_member_id),
    CHECK(from_member_id <> to_member_id)
);

CREATE TABLE collab_runtime (
    id TEXT PRIMARY KEY,
    member_id TEXT NOT NULL REFERENCES collab_member(id),
    binding_id TEXT NOT NULL REFERENCES collab_binding(id),
    terminal_session_id TEXT NOT NULL,
    terminal_generation INTEGER NOT NULL,
    observed_grok_session_id TEXT NOT NULL,
    process_id INTEGER,
    routing_revision INTEGER NOT NULL,
    auth_method TEXT NOT NULL CHECK(auth_method IN ('peer_identity', 'inherited_fd', 'sidecar', 'env_bearer')),
    token_hash TEXT,
    token_epoch INTEGER NOT NULL,
    attested_provider TEXT NOT NULL CHECK(attested_provider = 'grok-build'),
    attested_workspace_fingerprint TEXT NOT NULL,
    grok_version TEXT NOT NULL,
    helper_protocol_version TEXT NOT NULL,
    capability_probe_result TEXT NOT NULL,
    listener_state TEXT NOT NULL CHECK(listener_state IN ('offline', 'connecting', 'ready', 'degraded')),
    runtime_state TEXT NOT NULL CHECK(runtime_state IN ('unknown', 'idle', 'busy', 'waiting_user', 'exited')),
    last_heartbeat_at INTEGER,
    started_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    UNIQUE(member_id, terminal_generation)
);
CREATE UNIQUE INDEX collab_one_active_runtime_per_member
ON collab_runtime(member_id) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX collab_one_active_terminal_session
ON collab_runtime(terminal_session_id) WHERE revoked_at IS NULL;

CREATE TABLE collab_edge_cursor (
    sender_member_id TEXT NOT NULL REFERENCES collab_member(id),
    recipient_member_id TEXT NOT NULL REFERENCES collab_member(id),
    next_sequence INTEGER NOT NULL,
    PRIMARY KEY(sender_member_id, recipient_member_id),
    CHECK(sender_member_id <> recipient_member_id)
);

CREATE TABLE collab_message (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES collab_team(id),
    sender_member_id TEXT NOT NULL REFERENCES collab_member(id),
    sender_generation INTEGER NOT NULL,
    recipient_member_id TEXT NOT NULL REFERENCES collab_member(id),
    recipient_generation INTEGER NOT NULL,
    routing_revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('message', 'task_assignment', 'question', 'progress', 'task_report', 'task_cancel', 'task_cancel_ack')),
    task_id TEXT REFERENCES collab_task(id) DEFERRABLE INITIALLY DEFERRED,
    reply_to_message_id TEXT REFERENCES collab_message(id),
    payload_text TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    retry_of_message_id TEXT REFERENCES collab_message(id),
    edge_sequence INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('queued', 'suspended', 'leased', 'acknowledged', 'blocked', 'expired', 'dead_letter', 'cancelled')),
    lease_token_hash TEXT,
    lease_epoch INTEGER NOT NULL DEFAULT 0,
    lease_until INTEGER,
    ack_token_hash TEXT,
    acknowledged_lease_epoch INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    not_before INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    paused_at INTEGER,
    blocked_at INTEGER,
    blocked_reason TEXT,
    resolution_policy TEXT CHECK(resolution_policy IS NULL OR resolution_policy IN ('auto_resume', 'user_retry', 'never')),
    last_error_code TEXT,
    created_at INTEGER NOT NULL,
    acknowledged_at INTEGER,
    updated_at INTEGER NOT NULL,
    UNIQUE(team_id, sender_member_id, request_id),
    UNIQUE(sender_member_id, recipient_member_id, edge_sequence),
    CHECK(sender_member_id <> recipient_member_id),
    CHECK(expires_at > not_before),
    CHECK((kind = 'message' AND task_id IS NULL) OR (kind <> 'message' AND task_id IS NOT NULL))
);

CREATE TABLE collab_task (
    id TEXT PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES collab_team(id),
    assigner_member_id TEXT NOT NULL REFERENCES collab_member(id),
    assignee_member_id TEXT NOT NULL REFERENCES collab_member(id),
    assignee_generation INTEGER NOT NULL,
    assignment_message_id TEXT NOT NULL UNIQUE REFERENCES collab_message(id) DEFERRABLE INITIALLY DEFERRED,
    title TEXT NOT NULL,
    instructions TEXT NOT NULL,
    optional_scope_json TEXT,
    state TEXT NOT NULL CHECK(state IN ('assigned', 'accepted', 'running', 'reported_completed', 'reported_failed', 'cancel_requested', 'cancelled')),
    version INTEGER NOT NULL DEFAULT 0,
    terminal_report_message_id TEXT UNIQUE REFERENCES collab_message(id),
    cancel_request_message_id TEXT UNIQUE REFERENCES collab_message(id),
    cancel_ack_message_id TEXT UNIQUE REFERENCES collab_message(id),
    attention_state TEXT NOT NULL CHECK(attention_state IN ('none', 'report_required', 'delivery_failed', 'cancel_unconfirmed', 'uncertain_execution')),
    attention_reason TEXT,
    attention_since INTEGER,
    report_reminder_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    accepted_at INTEGER,
    started_at INTEGER,
    terminal_at INTEGER,
    updated_at INTEGER NOT NULL,
    CHECK(assigner_member_id <> assignee_member_id)
);
CREATE UNIQUE INDEX collab_one_active_task_per_runtime
ON collab_task(assignee_member_id, assignee_generation)
WHERE state IN ('accepted', 'running', 'cancel_requested');

CREATE INDEX collab_message_recipient_queue
ON collab_message(recipient_member_id, recipient_generation, state, not_before, edge_sequence);
CREATE INDEX collab_message_expiry ON collab_message(state, expires_at);
CREATE INDEX collab_task_assignee ON collab_task(assignee_member_id, assignee_generation, state);

CREATE TABLE collab_operation_request (
    team_id TEXT NOT NULL REFERENCES collab_team(id),
    sender_member_id TEXT NOT NULL REFERENCES collab_member(id),
    request_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    operation TEXT NOT NULL CHECK(operation IN ('cancel_task')),
    task_id TEXT NOT NULL REFERENCES collab_task(id),
    result_message_id TEXT REFERENCES collab_message(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(team_id, sender_member_id, request_id)
);

CREATE TABLE collab_event (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    team_id TEXT NOT NULL REFERENCES collab_team(id),
    aggregate_type TEXT NOT NULL CHECK(aggregate_type IN ('team', 'member', 'message', 'task', 'runtime', 'security')),
    aggregate_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    actor_type TEXT NOT NULL CHECK(actor_type IN ('user', 'member', 'broker', 'system')),
    actor_member_id TEXT REFERENCES collab_member(id),
    redacted_metadata_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TRIGGER collab_event_append_only_update
BEFORE UPDATE ON collab_event BEGIN SELECT RAISE(ABORT, 'collab_event is append-only'); END;
CREATE TRIGGER collab_event_append_only_delete
BEFORE DELETE ON collab_event BEGIN SELECT RAISE(ABORT, 'collab_event is append-only'); END;
"#;

pub(crate) fn select_team(connection: &Connection, id: &str) -> CollabResult<Team> {
    connection
        .query_row(
            "SELECT id, name, provider, enabled, workspace_fingerprint,
                    config_revision, routing_revision, created_at, updated_at, archived_at
             FROM collab_team WHERE id = ?1",
            [id],
            |row| {
                Ok(Team {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider: row.get(2)?,
                    enabled: row.get(3)?,
                    workspace_fingerprint: row.get(4)?,
                    config_revision: row.get(5)?,
                    routing_revision: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    archived_at: row.get(9)?,
                })
            },
        )
        .map_err(not_found("team"))
}

pub(crate) fn select_member(connection: &Connection, id: &str) -> CollabResult<Member> {
    connection
        .query_row(
            "SELECT id, team_id, alias, display_name, avatar_id, role, enabled,
                    created_at, updated_at FROM collab_member WHERE id = ?1",
            [id],
            |row| {
                Ok(Member {
                    id: row.get(0)?,
                    team_id: row.get(1)?,
                    alias: row.get(2)?,
                    display_name: row.get(3)?,
                    avatar_id: row.get(4)?,
                    role: enum_at(row, 5)?,
                    enabled: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .map_err(not_found("member"))
}

pub(crate) fn select_runtime(connection: &Connection, id: &str) -> CollabResult<Runtime> {
    connection
        .query_row(
            &format!("SELECT {RUNTIME_COLUMNS} FROM collab_runtime WHERE id = ?1"),
            [id],
            runtime_from_row,
        )
        .map_err(not_found("runtime"))
}

pub(crate) fn select_message(connection: &Connection, id: &str) -> CollabResult<Message> {
    connection
        .query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM collab_message WHERE id = ?1"),
            [id],
            message_from_row,
        )
        .map_err(not_found("message"))
}

pub(crate) fn select_task(connection: &Connection, id: &str) -> CollabResult<Task> {
    connection
        .query_row(
            &format!("SELECT {TASK_COLUMNS} FROM collab_task WHERE id = ?1"),
            [id],
            task_from_row,
        )
        .map_err(not_found("task"))
}

pub(crate) fn runtime_from_row(row: &Row<'_>) -> rusqlite::Result<Runtime> {
    Ok(Runtime {
        id: row.get(0)?,
        member_id: row.get(1)?,
        binding_id: row.get(2)?,
        terminal_session_id: row.get(3)?,
        terminal_generation: row.get(4)?,
        observed_grok_session_id: row.get(5)?,
        process_id: row.get(6)?,
        routing_revision: row.get(7)?,
        auth_method: enum_at(row, 8)?,
        token_epoch: row.get(9)?,
        attested_provider: row.get(10)?,
        attested_workspace_fingerprint: row.get(11)?,
        grok_version: row.get(12)?,
        helper_protocol_version: row.get(13)?,
        capability_probe_result: row.get(14)?,
        listener_state: enum_at(row, 15)?,
        runtime_state: enum_at(row, 16)?,
        last_heartbeat_at: row.get(17)?,
        started_at: row.get(18)?,
        created_at: row.get(19)?,
        revoked_at: row.get(20)?,
    })
}

pub(crate) fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        team_id: row.get(1)?,
        sender_member_id: row.get(2)?,
        sender_generation: row.get(3)?,
        recipient_member_id: row.get(4)?,
        recipient_generation: row.get(5)?,
        routing_revision: row.get(6)?,
        kind: enum_at(row, 7)?,
        task_id: row.get(8)?,
        reply_to_message_id: row.get(9)?,
        payload_text: row.get(10)?,
        request_id: row.get(11)?,
        request_fingerprint: row.get(12)?,
        retry_of_message_id: row.get(13)?,
        edge_sequence: row.get(14)?,
        state: enum_at(row, 15)?,
        lease_epoch: row.get(16)?,
        lease_until: row.get(17)?,
        acknowledged_lease_epoch: row.get(18)?,
        attempt_count: row.get(19)?,
        not_before: row.get(20)?,
        expires_at: row.get(21)?,
        paused_at: row.get(22)?,
        blocked_at: row.get(23)?,
        blocked_reason: row.get(24)?,
        resolution_policy: row.get(25)?,
        last_error_code: row.get(26)?,
        created_at: row.get(27)?,
        acknowledged_at: row.get(28)?,
        updated_at: row.get(29)?,
    })
}

pub(crate) fn task_from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        team_id: row.get(1)?,
        assigner_member_id: row.get(2)?,
        assignee_member_id: row.get(3)?,
        assignee_generation: row.get(4)?,
        assignment_message_id: row.get(5)?,
        title: row.get(6)?,
        instructions: row.get(7)?,
        optional_scope_json: row.get(8)?,
        state: enum_at(row, 9)?,
        version: row.get(10)?,
        terminal_report_message_id: row.get(11)?,
        cancel_request_message_id: row.get(12)?,
        cancel_ack_message_id: row.get(13)?,
        attention_state: enum_at(row, 14)?,
        attention_reason: row.get(15)?,
        attention_since: row.get(16)?,
        report_reminder_count: row.get(17)?,
        created_at: row.get(18)?,
        accepted_at: row.get(19)?,
        started_at: row.get(20)?,
        terminal_at: row.get(21)?,
        updated_at: row.get(22)?,
    })
}

pub(crate) fn enum_at<T: DbEnum>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    T::from_db(&raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown collaboration enum value {raw:?}"),
            )),
        )
    })
}

fn not_found(entity: &'static str) -> impl FnOnce(rusqlite::Error) -> CollaborationError + 'static {
    move |error| match error {
        rusqlite::Error::QueryReturnedNoRows => CollaborationError::NotFound(entity),
        other => CollaborationError::Database(other),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_event(
    transaction: &Transaction<'_>,
    team_id: &str,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    actor_type: ActorType,
    actor_member_id: Option<&str>,
    redacted_metadata_json: &str,
    created_at: i64,
) -> CollabResult<()> {
    transaction.execute(
        "INSERT INTO collab_event(
             id, team_id, aggregate_type, aggregate_id, event_type, actor_type,
             actor_member_id, redacted_metadata_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            new_id(),
            team_id,
            aggregate_type,
            aggregate_id,
            event_type,
            actor_type.as_db(),
            actor_member_id,
            redacted_metadata_json,
            created_at
        ],
    )?;
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> CollabResult<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        CollaborationError::InvalidInput(format!(
            "cannot secure collaboration directory permissions: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> CollabResult<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> CollabResult<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        CollaborationError::InvalidInput(format!(
            "cannot secure collaboration database permissions: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> CollabResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_v1_schema_and_append_only_events() {
        let store = CollaborationStore::in_memory().expect("in-memory store");
        let connection = store.lock().expect("lock");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 1);

        let now = now_ms();
        connection
            .execute(
                "INSERT INTO collab_team(id, name, provider, enabled,
                 workspace_fingerprint, config_revision, routing_revision,
                 created_at, updated_at) VALUES ('t', 'team', 'grok-build', 0,
                 'workspace', 1, 1, ?1, ?1)",
                [now],
            )
            .expect("team");
        connection
            .execute(
                "INSERT INTO collab_event(id, team_id, aggregate_type,
                 aggregate_id, event_type, actor_type, redacted_metadata_json,
                 created_at) VALUES ('e', 't', 'team', 't', 'team_created',
                 'system', '{}', ?1)",
                [now],
            )
            .expect("event");
        assert!(connection
            .execute("UPDATE collab_event SET event_type = 'tampered'", [])
            .is_err());
    }

    #[test]
    fn released_bindings_preserve_history_but_active_binding_is_unique() {
        let store = CollaborationStore::in_memory().expect("store");
        let connection = store.lock().expect("lock");
        let now = now_ms();
        connection.execute_batch(&format!(
            "INSERT INTO collab_team(id,name,provider,enabled,workspace_fingerprint,config_revision,routing_revision,created_at,updated_at)
             VALUES ('t','team','grok-build',0,'w',1,1,{now},{now});
             INSERT INTO collab_member(id,team_id,alias,display_name,avatar_id,role,enabled,created_at,updated_at)
             VALUES ('m','t','main','Main','a','leader',1,{now},{now});
             INSERT INTO collab_binding(id,member_id,provider,grok_session_id,bound_at)
             VALUES ('b1','m','grok-build','g1',{now});"
        )).expect("seed");
        assert!(connection
            .execute(
                "INSERT INTO collab_binding(id,member_id,provider,grok_session_id,bound_at)
             VALUES ('b2','m','grok-build','g2',?1)",
                [now]
            )
            .is_err());
        connection
            .execute(
                "UPDATE collab_binding SET released_at=?1 WHERE id='b1'",
                [now],
            )
            .expect("release");
        connection
            .execute(
                "INSERT INTO collab_binding(id,member_id,provider,grok_session_id,bound_at)
             VALUES ('b2','m','grok-build','g2',?1)",
                [now],
            )
            .expect("new active binding");
    }
}
