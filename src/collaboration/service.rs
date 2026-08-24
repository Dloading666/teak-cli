//! Transactional collaboration broker core.
//!
//! This module owns authorization and domain transitions.  Transport code may
//! wake a Grok session after these methods commit, but must never infer durable
//! state from PTY output.

use super::model::*;
use super::store::{
    enum_at, insert_event, message_from_row, runtime_from_row, select_member, select_message,
    select_task, select_team, task_from_row, CollaborationStore, MESSAGE_COLUMNS, RUNTIME_COLUMNS,
    TASK_COLUMNS,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::json;
use std::path::Path;

pub struct CollaborationService {
    store: CollaborationStore,
}

#[derive(Debug, Clone)]
struct AuthenticatedCaller {
    team: Team,
    member: Member,
    runtime: Runtime,
}

/// Bounded, redacted reasons that may cross from the broker's authorization
/// boundary into the append-only security audit log. Keeping this typed means
/// raw domain errors, aliases, request bodies, lease tokens, and bearer
/// material cannot accidentally become event metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthenticatedRejectionReason {
    TargetPolicy,
    TaskScope,
    ResourceScope,
    Policy,
}

impl AuthenticatedRejectionReason {
    fn metadata_json(self) -> &'static str {
        match self {
            Self::TargetPolicy => r#"{"reasonCode":"target_policy_denied"}"#,
            Self::TaskScope => r#"{"reasonCode":"task_scope_denied"}"#,
            Self::ResourceScope => r#"{"reasonCode":"resource_scope_denied"}"#,
            Self::Policy => r#"{"reasonCode":"policy_denied"}"#,
        }
    }
}

impl CollaborationService {
    pub fn open(path: impl AsRef<Path>) -> CollabResult<Self> {
        Ok(Self {
            store: CollaborationStore::open(path)?,
        })
    }

    pub fn in_memory() -> CollabResult<Self> {
        Ok(Self {
            store: CollaborationStore::in_memory()?,
        })
    }

    pub fn store(&self) -> &CollaborationStore {
        &self.store
    }

    pub fn global_enabled(&self) -> CollabResult<bool> {
        let connection = self.store.lock()?;
        let value: String = connection.query_row(
            "SELECT value FROM collab_meta WHERE key='global_enabled'",
            [],
            |row| row.get(0),
        )?;
        Ok(value == "1")
    }

    /// Turning the master switch off invalidates every capability/runtime and
    /// moves deliverable messages to `suspended`.  Normal PTYs are not touched.
    pub fn set_global_enabled(&self, enabled: bool) -> CollabResult<()> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_ms();
        transaction.execute(
            "UPDATE collab_meta SET value=?1 WHERE key='global_enabled'",
            [if enabled { "1" } else { "0" }],
        )?;
        if !enabled {
            let active_tasks = {
                let mut statement = transaction.prepare(
                    "SELECT id,team_id FROM collab_task
                     WHERE state IN ('accepted','running','cancel_requested')",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            suspend_messages(&transaction, None, now, "global_disabled")?;
            transaction.execute(
                "UPDATE collab_runtime SET revoked_at=?1
                 WHERE revoked_at IS NULL",
                [now],
            )?;
            transaction.execute(
                "UPDATE collab_task SET attention_state='uncertain_execution',
                 attention_reason='global_collaboration_disabled',
                 attention_since=COALESCE(attention_since,?1),updated_at=?1,version=version+1
                 WHERE state IN ('accepted','running','cancel_requested')",
                [now],
            )?;
            for (task_id, team_id) in active_tasks {
                insert_event(
                    &transaction,
                    &team_id,
                    "task",
                    &task_id,
                    "task_needs_attention",
                    ActorType::System,
                    None,
                    r#"{"reason":"global_collaboration_disabled"}"#,
                    now,
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn create_team(&self, request: NewTeam) -> CollabResult<Team> {
        validate_nonempty("team name", &request.name)?;
        validate_nonempty("workspace fingerprint", &request.workspace_fingerprint)?;
        if request.enabled {
            return Err(CollaborationError::InvalidInput(
                "create the roster first, then enable the team".into(),
            ));
        }
        let id = new_id();
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO collab_team(
                id,name,provider,enabled,workspace_fingerprint,config_revision,
                routing_revision,created_at,updated_at
             ) VALUES (?1,?2,'grok-build',0,?3,1,1,?4,?4)",
            params![id, request.name, request.workspace_fingerprint, now],
        )?;
        insert_event(
            &transaction,
            &id,
            "team",
            &id,
            "team_created",
            ActorType::User,
            None,
            "{}",
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.store.team(&id)
    }

    pub fn add_member(&self, request: NewMember) -> CollabResult<Member> {
        validate_alias(&request.alias)?;
        validate_nonempty("display name", &request.display_name)?;
        validate_nonempty("avatar id", &request.avatar_id)?;
        let id = new_id();
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let team = select_team(&transaction, &request.team_id)?;
        ensure_team_mutable(&team)?;
        ensure_team_paused(&team)?;
        ensure_no_nonterminal_tasks(&transaction, &team.id)?;
        let enabled_members: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collab_member WHERE team_id=?1 AND enabled=1",
            [&request.team_id],
            |row| row.get(0),
        )?;
        if request.enabled && enabled_members >= 4 {
            return Err(CollaborationError::Capacity("team_members"));
        }
        transaction.execute(
            "INSERT INTO collab_member(
                id,team_id,alias,display_name,avatar_id,role,enabled,created_at,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![
                id,
                request.team_id,
                request.alias,
                request.display_name,
                request.avatar_id,
                request.role.as_db(),
                request.enabled,
                now
            ],
        )?;
        bump_routing_revision(&transaction, &team.id, now)?;
        insert_event(
            &transaction,
            &team.id,
            "member",
            &id,
            "member_added",
            ActorType::User,
            None,
            &json!({"role": request.role.as_db()}).to_string(),
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.store.member(&id)
    }

    pub fn bind_member(&self, request: NewBinding) -> CollabResult<Binding> {
        validate_nonempty("Grok session id", &request.grok_session_id)?;
        let id = new_id();
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let member = select_member(&transaction, &request.member_id)?;
        let team = select_team(&transaction, &member.team_id)?;
        ensure_team_mutable(&team)?;
        ensure_team_paused(&team)?;
        ensure_no_nonterminal_tasks(&transaction, &team.id)?;
        let active_task_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collab_task
             WHERE assignee_member_id=?1 AND state IN ('accepted','running','cancel_requested')",
            [&member.id],
            |row| row.get(0),
        )?;
        if active_task_count > 0 {
            return Err(CollaborationError::Conflict(
                "member has an active task; binding cannot change".into(),
            ));
        }
        transaction.execute(
            "UPDATE collab_binding SET released_at=?1
             WHERE member_id=?2 AND released_at IS NULL",
            params![now, member.id],
        )?;
        transaction.execute(
            "UPDATE collab_runtime SET revoked_at=?1
             WHERE member_id=?2 AND revoked_at IS NULL",
            params![now, member.id],
        )?;
        transaction.execute(
            "INSERT INTO collab_binding(
                id,member_id,provider,grok_session_id,bound_at
             ) VALUES (?1,?2,'grok-build',?3,?4)",
            params![id, member.id, request.grok_session_id, now],
        )?;
        bump_routing_revision(&transaction, &team.id, now)?;
        insert_event(
            &transaction,
            &team.id,
            "member",
            &member.id,
            "member_bound",
            ActorType::User,
            None,
            "{}",
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        let connection = self.store.lock()?;
        connection
            .query_row(
                "SELECT id,member_id,provider,grok_session_id,bound_at,released_at
                 FROM collab_binding WHERE id=?1",
                [&id],
                |row| {
                    Ok(Binding {
                        id: row.get(0)?,
                        member_id: row.get(1)?,
                        provider: row.get(2)?,
                        grok_session_id: row.get(3)?,
                        bound_at: row.get(4)?,
                        released_at: row.get(5)?,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Rebuilds the deliberately fixed V1 graph from member roles.
    pub fn install_v1_acl(&self, team_id: &str) -> CollabResult<()> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let team = select_team(&transaction, team_id)?;
        ensure_team_mutable(&team)?;
        ensure_team_paused(&team)?;
        ensure_no_nonterminal_tasks(&transaction, &team.id)?;
        let leader_id: String = transaction
            .query_row(
                "SELECT id FROM collab_member
                 WHERE team_id=?1 AND enabled=1 AND role='leader'",
                [team_id],
                |row| row.get(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    CollaborationError::InvalidInput("team requires one enabled leader".into())
                }
                other => CollaborationError::Database(other),
            })?;
        let worker_ids = {
            let mut statement = transaction.prepare(
                "SELECT id FROM collab_member
                 WHERE team_id=?1 AND enabled=1 AND role='worker' ORDER BY id",
            )?;
            let rows = statement
                .query_map([team_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        transaction.execute("DELETE FROM collab_acl WHERE team_id=?1", [team_id])?;
        for worker_id in worker_ids {
            transaction.execute(
                "INSERT INTO collab_acl(
                    team_id,from_member_id,to_member_id,can_message,
                    can_assign_task,can_report,can_cancel_task,can_ack_cancel
                 ) VALUES (?1,?2,?3,1,1,0,1,0)",
                params![team_id, leader_id, worker_id],
            )?;
            transaction.execute(
                "INSERT INTO collab_acl(
                    team_id,from_member_id,to_member_id,can_message,
                    can_assign_task,can_report,can_cancel_task,can_ack_cancel
                 ) VALUES (?1,?2,?3,1,0,1,0,1)",
                params![team_id, worker_id, leader_id],
            )?;
        }
        bump_routing_revision(&transaction, team_id, now)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn set_team_enabled(&self, team_id: &str, enabled: bool) -> CollabResult<Team> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let team = select_team(&transaction, team_id)?;
        ensure_team_mutable(&team)?;
        if team.enabled == enabled {
            transaction.commit()?;
            drop(connection);
            return self.store.team(team_id);
        }
        if enabled {
            ensure_global_enabled(&transaction)?;
            if team.workspace_fingerprint.trim().is_empty() {
                return Err(CollaborationError::InvalidInput(
                    "workspace fingerprint is required".into(),
                ));
            }
            let leaders: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM collab_member
                 WHERE team_id=?1 AND enabled=1 AND role='leader'",
                [team_id],
                |row| row.get(0),
            )?;
            if leaders != 1 {
                return Err(CollaborationError::InvalidInput(
                    "enabled team requires exactly one enabled leader".into(),
                ));
            }
            let workers: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM collab_member
                 WHERE team_id=?1 AND enabled=1 AND role='worker'",
                [team_id],
                |row| row.get(0),
            )?;
            if !(1..=3).contains(&workers) {
                return Err(CollaborationError::InvalidInput(
                    "enabled team requires 1-3 enabled workers".into(),
                ));
            }
            let bindings: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM collab_member m
                 JOIN collab_binding b ON b.member_id=m.id AND b.released_at IS NULL
                 WHERE m.team_id=?1 AND m.enabled=1",
                [team_id],
                |row| row.get(0),
            )?;
            let members: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM collab_member WHERE team_id=?1 AND enabled=1",
                [team_id],
                |row| row.get(0),
            )?;
            if bindings != members {
                return Err(CollaborationError::InvalidInput(
                    "every enabled member requires an active Grok binding".into(),
                ));
            }
            transaction.execute(
                "UPDATE collab_team SET enabled=1,updated_at=?1 WHERE id=?2",
                params![now, team_id],
            )?;
            resume_messages(&transaction, team_id, now)?;
            insert_event(
                &transaction,
                team_id,
                "team",
                team_id,
                "team_enabled",
                ActorType::User,
                None,
                "{}",
                now,
            )?;
        } else {
            transaction.execute(
                "UPDATE collab_team SET enabled=0,updated_at=?1 WHERE id=?2",
                params![now, team_id],
            )?;
            suspend_messages(&transaction, Some(team_id), now, "team_paused")?;
            insert_event(
                &transaction,
                team_id,
                "team",
                team_id,
                "team_paused",
                ActorType::User,
                None,
                "{}",
                now,
            )?;
        }
        transaction.commit()?;
        drop(connection);
        self.store.team(team_id)
    }

    pub fn archive_team(&self, team_id: &str) -> CollabResult<Team> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let team = select_team(&transaction, team_id)?;
        if team.archived_at.is_some() {
            transaction.commit()?;
            drop(connection);
            return self.store.team(team_id);
        }
        ensure_no_nonterminal_tasks(&transaction, team_id)?;
        block_team_messages(&transaction, team_id, now, "team_archived", "never")?;
        transaction.execute(
            "UPDATE collab_runtime
             SET revoked_at=?1,listener_state='offline',runtime_state='exited'
             WHERE member_id IN
             (SELECT id FROM collab_member WHERE team_id=?2) AND revoked_at IS NULL",
            params![now, team_id],
        )?;
        transaction.execute(
            "UPDATE collab_binding SET released_at=?1 WHERE member_id IN
             (SELECT id FROM collab_member WHERE team_id=?2) AND released_at IS NULL",
            params![now, team_id],
        )?;
        transaction.execute(
            "UPDATE collab_team SET enabled=0,archived_at=?1,updated_at=?1 WHERE id=?2",
            params![now, team_id],
        )?;
        insert_event(
            &transaction,
            team_id,
            "team",
            team_id,
            "team_archived",
            ActorType::User,
            None,
            "{}",
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.store.team(team_id)
    }

    pub fn register_runtime(&self, request: NewRuntime) -> CollabResult<Runtime> {
        let now = now_ms();
        let runtime_id = new_id();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_global_enabled(&transaction)?;
        let member = select_member(&transaction, &request.member_id)?;
        let team = select_team(&transaction, &member.team_id)?;
        ensure_enabled_team_and_member(&team, &member)?;
        let (binding_member_id, grok_session_id): (String, String) = transaction
            .query_row(
                "SELECT member_id,grok_session_id FROM collab_binding
                 WHERE id=?1 AND released_at IS NULL",
                [&request.binding_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => CollaborationError::NotFound("binding"),
                other => CollaborationError::Database(other),
            })?;
        if binding_member_id != member.id
            || grok_session_id != request.observed_grok_session_id
            || request.attested_workspace_fingerprint != team.workspace_fingerprint
        {
            return Err(CollaborationError::Unauthorized("runtime_attestation"));
        }
        if request.token_epoch < 1 {
            return Err(CollaborationError::InvalidInput(
                "token epoch must be positive".into(),
            ));
        }
        if request.terminal_generation < 1 {
            return Err(CollaborationError::InvalidInput(
                "terminal generation must be positive".into(),
            ));
        }
        if request.auth_method == AuthMethod::EnvBearer && request.bearer_secret.is_none() {
            return Err(CollaborationError::InvalidInput(
                "env bearer runtime requires a secret".into(),
            ));
        }
        let token_hash = request.bearer_secret.as_deref().map(hash_secret);

        let old_generations = {
            let mut statement = transaction.prepare(
                "SELECT terminal_generation FROM collab_runtime
                 WHERE member_id=?1 AND revoked_at IS NULL",
            )?;
            let rows = statement
                .query_map([&member.id], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        transaction.execute(
            "UPDATE collab_runtime SET revoked_at=?1
             WHERE member_id=?2 AND revoked_at IS NULL",
            params![now, member.id],
        )?;
        for generation in old_generations {
            block_generation(
                &transaction,
                &team.id,
                &member.id,
                generation,
                now,
                "stale_target",
            )?;
            let affected_tasks = {
                let mut statement = transaction.prepare(
                    "SELECT id FROM collab_task
                     WHERE assignee_member_id=?1 AND assignee_generation=?2
                       AND state IN ('accepted','running','cancel_requested')",
                )?;
                let rows = statement
                    .query_map(params![member.id, generation], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            };
            transaction.execute(
                "UPDATE collab_task
                 SET attention_state='uncertain_execution',
                     attention_reason='runtime_generation_changed',
                     attention_since=COALESCE(attention_since,?1),updated_at=?1,version=version+1
                 WHERE assignee_member_id=?2 AND assignee_generation=?3
                   AND state IN ('accepted','running','cancel_requested')",
                params![now, member.id, generation],
            )?;
            for task_id in affected_tasks {
                insert_event(
                    &transaction,
                    &team.id,
                    "task",
                    &task_id,
                    "task_needs_attention",
                    ActorType::Broker,
                    None,
                    r#"{"reason":"runtime_generation_changed"}"#,
                    now,
                )?;
            }
        }
        transaction.execute(
            "INSERT INTO collab_runtime(
                id,member_id,binding_id,terminal_session_id,terminal_generation,
                observed_grok_session_id,process_id,routing_revision,auth_method,
                token_hash,token_epoch,attested_provider,
                attested_workspace_fingerprint,grok_version,helper_protocol_version,
                capability_probe_result,listener_state,runtime_state,last_heartbeat_at,
                started_at,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'grok-build',?12,
                       ?13,?14,?15,?16,?17,?18,?19,?19)",
            params![
                runtime_id,
                member.id,
                request.binding_id,
                request.terminal_session_id,
                request.terminal_generation,
                request.observed_grok_session_id,
                request.process_id,
                team.routing_revision,
                request.auth_method.as_db(),
                token_hash,
                request.token_epoch,
                request.attested_workspace_fingerprint,
                request.grok_version,
                request.helper_protocol_version,
                request.capability_probe_result,
                request.listener_state.as_db(),
                request.runtime_state.as_db(),
                if request.listener_state == ListenerState::Ready {
                    Some(now)
                } else {
                    None
                },
                now
            ],
        )?;
        insert_event(
            &transaction,
            &team.id,
            "runtime",
            &runtime_id,
            if request.listener_state == ListenerState::Ready {
                "member_ready"
            } else {
                "member_connecting"
            },
            ActorType::Broker,
            Some(&member.id),
            &json!({"generation": request.terminal_generation}).to_string(),
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.store.runtime(&runtime_id)
    }

    pub fn update_runtime_state(
        &self,
        caller: &CallerIdentity,
        listener_state: ListenerState,
        runtime_state: RuntimeState,
    ) -> CollabResult<Runtime> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        transaction.execute(
            "UPDATE collab_runtime
             SET listener_state=?1,runtime_state=?2,last_heartbeat_at=?3
             WHERE id=?4",
            params![
                listener_state.as_db(),
                runtime_state.as_db(),
                now,
                authenticated.runtime.id
            ],
        )?;
        insert_event(
            &transaction,
            &authenticated.team.id,
            "runtime",
            &authenticated.runtime.id,
            match (listener_state, runtime_state) {
                (ListenerState::Ready, RuntimeState::WaitingUser) => "member_waiting_user",
                (ListenerState::Ready, _) => "member_ready",
                _ => "member_offline",
            },
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        transaction.commit()?;
        drop(connection);
        self.store.runtime(&authenticated.runtime.id)
    }

    /// Applies a Teak-observed terminal activity edge to the exact active
    /// collaboration runtime. The caller is the backend runtime registry, not
    /// an untrusted helper or frontend-provided member/generation. Listener
    /// readiness remains authoritative: connecting/offline generations are
    /// never made busy/idle by terminal UI observations.
    pub fn observe_ready_runtime_state(
        &self,
        terminal_session_id: &str,
        terminal_generation: i64,
        runtime_state: RuntimeState,
    ) -> CollabResult<bool> {
        validate_nonempty("terminal session id", terminal_session_id)?;
        if !matches!(
            runtime_state,
            RuntimeState::Idle | RuntimeState::Busy | RuntimeState::WaitingUser
        ) {
            return Err(CollaborationError::InvalidInput(
                "terminal activity must be idle, busy, or waiting_user".into(),
            ));
        }
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_global_enabled(&transaction)?;
        let observed = transaction
            .query_row(
                "SELECT r.id,r.member_id,m.team_id,r.runtime_state
                 FROM collab_runtime r
                 JOIN collab_member m ON m.id=r.member_id
                 JOIN collab_team t ON t.id=m.team_id
                 WHERE r.terminal_session_id=?1 AND r.terminal_generation=?2
                   AND r.revoked_at IS NULL AND r.listener_state='ready'
                   AND m.enabled=1 AND t.enabled=1 AND t.archived_at IS NULL",
                params![terminal_session_id, terminal_generation],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        enum_at::<RuntimeState>(row, 3)?,
                    ))
                },
            )
            .optional()?;
        let Some((runtime_id, member_id, team_id, previous_state)) = observed else {
            transaction.commit()?;
            return Ok(false);
        };
        if previous_state == runtime_state {
            transaction.commit()?;
            return Ok(false);
        }
        let changed = transaction.execute(
            "UPDATE collab_runtime SET runtime_state=?1,last_heartbeat_at=?2
             WHERE id=?3 AND revoked_at IS NULL AND listener_state='ready'
               AND runtime_state=?4",
            params![
                runtime_state.as_db(),
                now,
                runtime_id,
                previous_state.as_db()
            ],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict(
                "terminal activity edge lost compare-and-swap".into(),
            ));
        }
        let event_type = match runtime_state {
            RuntimeState::Idle => "member_ready",
            // Busy can also mean a normal user turn or bootstrap work. Keep
            // this runtime-scoped edge distinct from task-scoped starts.
            RuntimeState::Busy => "member_busy",
            RuntimeState::WaitingUser => "member_waiting_user",
            RuntimeState::Unknown | RuntimeState::Exited => unreachable!(),
        };
        insert_event(
            &transaction,
            &team_id,
            "runtime",
            &runtime_id,
            event_type,
            ActorType::Broker,
            Some(&member_id),
            r#"{"source":"terminal_activity"}"#,
            now,
        )?;
        if previous_state == RuntimeState::Busy && runtime_state == RuntimeState::Idle {
            record_report_required_edge(
                &transaction,
                &team_id,
                &member_id,
                terminal_generation,
                now,
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Returns the latest durable backend-only reminder for this exact
    /// runtime generation. Reconnecting a listener returns the same event ID,
    /// while a later Busy -> Idle edge creates the next (and final) reminder.
    pub fn peek_next_control_wake(
        &self,
        caller: &CallerIdentity,
    ) -> CollabResult<Option<ControlWake>> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let wake = transaction
            .query_row(
                "SELECT e.id,t.id
                 FROM collab_task t
                 JOIN collab_event e ON e.team_id=t.team_id
                   AND e.aggregate_type='task' AND e.aggregate_id=t.id
                   AND e.event_type='task_report_required'
                 WHERE t.team_id=?1 AND t.assignee_member_id=?2
                   AND t.assignee_generation=?3
                   AND t.state IN ('accepted','running')
                   AND t.attention_state='report_required'
                   AND t.attention_reason='missing_explicit_report'
                 ORDER BY e.sequence DESC LIMIT 1",
                params![
                    authenticated.team.id,
                    authenticated.member.id,
                    authenticated.runtime.terminal_generation
                ],
                |row| {
                    Ok(ControlWake {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        kind: "report_required".into(),
                    })
                },
            )
            .optional()?;
        transaction.commit()?;
        Ok(wake)
    }
}

impl CollaborationService {
    pub fn allowed(&self, caller: &CallerIdentity) -> CollabResult<AuthenticatedScope> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let scope = AuthenticatedScope {
            team_id: authenticated.team.id,
            member_id: authenticated.member.id,
            member_alias: authenticated.member.alias,
            role: authenticated.member.role,
            terminal_generation: authenticated.runtime.terminal_generation,
            token_epoch: authenticated.runtime.token_epoch,
            routing_revision: authenticated.team.routing_revision,
        };
        transaction.commit()?;
        Ok(scope)
    }

    pub fn tasks_pending(&self, caller: &CallerIdentity) -> CollabResult<Vec<Task>> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let (where_clause, generation) = match authenticated.member.role {
            Role::Worker => (
                "assignee_member_id=?1 AND assignee_generation=?2",
                authenticated.runtime.terminal_generation,
            ),
            Role::Leader => ("assigner_member_id=?1 AND ?2=?2", 0),
        };
        let tasks = {
            let mut statement = transaction.prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM collab_task
                 WHERE team_id=?3 AND {where_clause}
                   AND state IN ('assigned','accepted','running','cancel_requested')
                 ORDER BY created_at ASC,id ASC"
            ))?;
            let rows = statement
                .query_map(
                    params![authenticated.member.id, generation, authenticated.team.id],
                    task_from_row,
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        transaction.commit()?;
        Ok(tasks)
    }

    /// Resolves a helper bearer to its DB-owned identity. Alias is never used
    /// alone: generation and the stored verifier must select exactly one
    /// active runtime, and `authenticate` then rechecks the full scope.
    pub fn authenticate_claim(
        &self,
        member_alias: &str,
        terminal_generation: i64,
        bearer_secret: &str,
    ) -> CollabResult<CallerIdentity> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_global_enabled(&transaction)?;
        let candidates = {
            let mut statement = transaction.prepare(
                "SELECT m.id,r.token_epoch,r.token_hash
                 FROM collab_member m
                 JOIN collab_team t ON t.id=m.team_id
                 JOIN collab_runtime r ON r.member_id=m.id AND r.revoked_at IS NULL
                 WHERE m.alias=?1 AND m.enabled=1 AND t.enabled=1 AND t.archived_at IS NULL
                   AND r.terminal_generation=?2 AND r.token_hash IS NOT NULL",
            )?;
            let rows = statement
                .query_map(params![member_alias, terminal_generation], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let actual_hash = hash_secret(bearer_secret);
        let mut winner: Option<(String, i64)> = None;
        for (member_id, token_epoch, expected_hash) in candidates {
            if constant_time_eq(actual_hash.as_bytes(), expected_hash.as_bytes()) {
                if winner.is_some() {
                    return Err(CollaborationError::Unauthorized("ambiguous_claim"));
                }
                winner = Some((member_id, token_epoch));
            }
        }
        let (member_id, token_epoch) =
            winner.ok_or(CollaborationError::Unauthorized("invalid_claim"))?;
        let identity = CallerIdentity {
            member_id,
            terminal_generation,
            token_epoch,
            bearer_secret: Some(bearer_secret.to_owned()),
        };
        authenticate(&transaction, &identity)?;
        transaction.commit()?;
        Ok(identity)
    }

    /// Records a policy rejection only after re-authenticating the exact
    /// caller capability. The audit row intentionally contains no request id,
    /// target, payload, lease, or credential data; the actor is derived solely
    /// from the DB-owned authenticated scope rather than any wire claim.
    pub(crate) fn record_request_rejected(
        &self,
        caller: &CallerIdentity,
        reason: AuthenticatedRejectionReason,
    ) -> CollabResult<()> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let now = now_ms();
        insert_event(
            &transaction,
            &authenticated.team.id,
            "security",
            &authenticated.member.id,
            "request_rejected",
            ActorType::Member,
            Some(&authenticated.member.id),
            reason.metadata_json(),
            now,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn send_message(
        &self,
        caller: &CallerIdentity,
        request: SendMessageRequest,
    ) -> CollabResult<Message> {
        if !matches!(
            request.kind,
            MessageKind::Message | MessageKind::Question | MessageKind::Progress
        ) {
            return Err(CollaborationError::InvalidInput(
                "domain messages must use their atomic task operation".into(),
            ));
        }
        validate_request_id(&request.request_id)?;
        validate_payload(&request.payload_text)?;
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let recipient = member_by_alias(
            &transaction,
            &authenticated.team.id,
            &request.recipient_alias,
        )?;
        let fingerprint = operation_fingerprint(&json!({
            "operation": "send_message",
            "kind": request.kind.as_db(),
            "recipientMemberId": recipient.id,
            "taskId": request.task_id,
            "replyToMessageId": request.reply_to_message_id,
            "payloadText": request.payload_text,
            "retryOfMessageId": request.retry_of_message_id,
            "notBefore": request.not_before,
            "expiresAt": request.expires_at,
        }))?;
        if let Some(existing) = idempotent_message(
            &transaction,
            &authenticated.team.id,
            &authenticated.member.id,
            &request.request_id,
            &fingerprint,
        )? {
            transaction.commit()?;
            return Ok(existing);
        }
        let action = if request.kind == MessageKind::Message {
            AclAction::Message
        } else {
            AclAction::Report
        };
        authorize_acl(
            &transaction,
            &authenticated.team.id,
            &authenticated.member,
            &recipient,
            action,
        )?;
        if request.kind != MessageKind::Message {
            let task_id = request
                .task_id
                .as_deref()
                .ok_or_else(|| CollaborationError::InvalidInput("task id is required".into()))?;
            let task = select_task(&transaction, task_id)?;
            ensure_task_assignee(&task, &authenticated)?;
            if task.assigner_member_id != recipient.id {
                return Err(CollaborationError::Unauthorized("task_recipient"));
            }
        } else if request.task_id.is_some() {
            return Err(CollaborationError::InvalidInput(
                "generic message cannot be task-scoped".into(),
            ));
        }
        validate_reply(
            &transaction,
            &authenticated.team.id,
            &authenticated.member.id,
            &recipient.id,
            request.reply_to_message_id.as_deref(),
            request.task_id.as_deref(),
        )?;
        validate_retry_of(
            &transaction,
            &authenticated.team.id,
            &authenticated.member.id,
            &recipient.id,
            request.kind,
            request.task_id.as_deref(),
            request.retry_of_message_id.as_deref(),
        )?;
        let recipient_runtime = active_runtime(&transaction, &authenticated.team, &recipient)?;
        enforce_send_limits(
            &transaction,
            &authenticated.team.id,
            &authenticated.member.id,
            now,
        )?;
        let expires_at = request.expires_at.unwrap_or(now + DEFAULT_MESSAGE_TTL_MS);
        validate_delivery_window(request.not_before.unwrap_or(now), expires_at, now)?;
        let message = insert_message_record(
            &transaction,
            MessageInsert {
                id: new_id(),
                team: &authenticated.team,
                sender: &authenticated,
                recipient: &recipient,
                recipient_runtime: &recipient_runtime,
                kind: request.kind,
                task_id: request.task_id.as_deref(),
                reply_to: request.reply_to_message_id.as_deref(),
                payload_text: &request.payload_text,
                request_id: &request.request_id,
                request_fingerprint: &fingerprint,
                retry_of: request.retry_of_message_id.as_deref(),
                not_before: request.not_before.unwrap_or(now),
                expires_at,
                now,
            },
        )?;
        insert_event(
            &transaction,
            &authenticated.team.id,
            "message",
            &message.id,
            match request.kind {
                MessageKind::Question => "question_submitted",
                MessageKind::Progress => "progress_reported",
                _ => "message_queued",
            },
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        transaction.commit()?;
        Ok(message)
    }

    pub fn assign_task(
        &self,
        caller: &CallerIdentity,
        request: AssignTaskRequest,
    ) -> CollabResult<TaskMessageOutcome> {
        validate_request_id(&request.request_id)?;
        validate_nonempty("task title", &request.title)?;
        validate_payload(&request.instructions)?;
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        if authenticated.member.role != Role::Leader {
            return Err(CollaborationError::Unauthorized("assigner_role"));
        }
        let assignee = member_by_alias(
            &transaction,
            &authenticated.team.id,
            &request.assignee_alias,
        )?;
        if assignee.role != Role::Worker {
            return Err(CollaborationError::Unauthorized("assignee_role"));
        }
        let fingerprint = operation_fingerprint(&json!({
            "operation": "assign_task",
            "assigneeMemberId": assignee.id,
            "title": request.title,
            "instructions": request.instructions,
            "scope": request.optional_scope_json,
            "expiresAt": request.expires_at,
        }))?;
        if let Some(existing) = idempotent_message(
            &transaction,
            &authenticated.team.id,
            &authenticated.member.id,
            &request.request_id,
            &fingerprint,
        )? {
            let task_id = existing.task_id.clone().ok_or_else(|| {
                CollaborationError::Conflict("idempotent record is not a task".into())
            })?;
            let task = select_task(&transaction, &task_id)?;
            transaction.commit()?;
            return Ok(TaskMessageOutcome {
                task,
                message: existing,
            });
        }
        authorize_acl(
            &transaction,
            &authenticated.team.id,
            &authenticated.member,
            &assignee,
            AclAction::AssignTask,
        )?;
        let assignee_runtime = active_runtime(&transaction, &authenticated.team, &assignee)?;
        enforce_send_limits(
            &transaction,
            &authenticated.team.id,
            &authenticated.member.id,
            now,
        )?;
        let expires_at = request.expires_at.unwrap_or(now + DEFAULT_TASK_TTL_MS);
        validate_delivery_window(now, expires_at, now)?;
        let task_id = new_id();
        let assignment_message_id = new_id();
        let payload = json!({
            "taskId": task_id,
            "title": request.title,
            "instructions": request.instructions,
            "scope": request.optional_scope_json,
        })
        .to_string();
        validate_payload(&payload)?;
        let message = insert_message_record(
            &transaction,
            MessageInsert {
                id: assignment_message_id.clone(),
                team: &authenticated.team,
                sender: &authenticated,
                recipient: &assignee,
                recipient_runtime: &assignee_runtime,
                kind: MessageKind::TaskAssignment,
                task_id: Some(&task_id),
                reply_to: None,
                payload_text: &payload,
                request_id: &request.request_id,
                request_fingerprint: &fingerprint,
                retry_of: None,
                not_before: now,
                expires_at,
                now,
            },
        )?;
        transaction.execute(
            "INSERT INTO collab_task(
                id,team_id,assigner_member_id,assignee_member_id,assignee_generation,
                assignment_message_id,title,instructions,optional_scope_json,state,
                version,attention_state,report_reminder_count,created_at,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'assigned',0,'none',0,?10,?10)",
            params![
                task_id,
                authenticated.team.id,
                authenticated.member.id,
                assignee.id,
                assignee_runtime.terminal_generation,
                assignment_message_id,
                request.title,
                request.instructions,
                request.optional_scope_json,
                now
            ],
        )?;
        insert_event(
            &transaction,
            &authenticated.team.id,
            "message",
            &message.id,
            "message_queued",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        insert_event(
            &transaction,
            &authenticated.team.id,
            "task",
            &task_id,
            "task_assigned",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        let task = select_task(&transaction, &task_id)?;
        transaction.commit()?;
        Ok(TaskMessageOutcome { task, message })
    }

    pub fn peek_next_pending(&self, caller: &CallerIdentity) -> CollabResult<Option<Message>> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let result = next_pending_message(&transaction, &authenticated, now_ms())?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn pending_count(&self, caller: &CallerIdentity) -> CollabResult<i64> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let now = now_ms();
        let count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collab_message m
             WHERE m.recipient_member_id=?1 AND m.recipient_generation=?2
               AND m.team_id=?3 AND m.routing_revision=?4 AND m.state='queued'
               AND m.not_before<=?5 AND m.expires_at>?5",
            params![
                authenticated.member.id,
                authenticated.runtime.terminal_generation,
                authenticated.team.id,
                authenticated.team.routing_revision,
                now
            ],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(count)
    }

    pub fn lease_next(
        &self,
        caller: &CallerIdentity,
        request: LeaseRequest,
    ) -> CollabResult<Option<LeasedMessage>> {
        if request.lease_duration_ms <= 0 || request.lease_duration_ms > 5 * 60_000 {
            return Err(CollaborationError::InvalidInput(
                "lease duration must be between 1ms and 5 minutes".into(),
            ));
        }
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        expire_and_requeue(&transaction, request.now)?;
        let Some(candidate) = next_pending_message(&transaction, &authenticated, request.now)?
        else {
            transaction.commit()?;
            return Ok(None);
        };
        let token = format!("{}{}", new_id(), new_id());
        let token_hash = hash_secret(&token);
        let changed = transaction.execute(
            "UPDATE collab_message
             SET state='leased',lease_token_hash=?1,lease_epoch=lease_epoch+1,
                 lease_until=?2,attempt_count=attempt_count+1,updated_at=?3
             WHERE id=?4 AND state='queued'",
            params![
                token_hash,
                request.now + request.lease_duration_ms,
                request.now,
                candidate.id
            ],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict(
                "message was concurrently leased".into(),
            ));
        }
        let message = select_message(&transaction, &candidate.id)?;
        insert_event(
            &transaction,
            &authenticated.team.id,
            "message",
            &message.id,
            "message_leased",
            ActorType::Member,
            Some(&authenticated.member.id),
            &json!({"leaseEpoch": message.lease_epoch}).to_string(),
            request.now,
        )?;
        transaction.commit()?;
        Ok(Some(LeasedMessage {
            lease_epoch: message.lease_epoch,
            message,
            lease_token: token,
        }))
    }

    pub fn ack_message(
        &self,
        caller: &CallerIdentity,
        request: AckMessageRequest,
    ) -> CollabResult<Message> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let message = select_message(&transaction, &request.message_id)?;
        if !matches!(
            message.kind,
            MessageKind::Message
                | MessageKind::Question
                | MessageKind::Progress
                | MessageKind::TaskCancelAck
        ) {
            return Err(CollaborationError::InvalidInput(
                "message kind requires its atomic domain ACK".into(),
            ));
        }
        ensure_message_recipient(&message, &authenticated)?;
        let changed = acknowledge_leased_message(
            &transaction,
            &message,
            request.lease_epoch,
            &request.lease_token,
            now,
        )?;
        if changed {
            insert_event(
                &transaction,
                &message.team_id,
                "message",
                &message.id,
                "message_acknowledged",
                ActorType::Member,
                Some(&authenticated.member.id),
                "{}",
                now,
            )?;
        }
        let acknowledged = select_message(&transaction, &message.id)?;
        transaction.commit()?;
        Ok(acknowledged)
    }
}

impl CollaborationService {
    pub fn accept_task(
        &self,
        caller: &CallerIdentity,
        request: AcceptTaskRequest,
    ) -> CollabResult<Task> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let task = select_task(&transaction, &request.task_id)?;
        ensure_task_assignee(&task, &authenticated)?;
        let assignment = select_message(&transaction, &request.assignment_message_id)?;
        ensure_message_recipient(&assignment, &authenticated)?;
        if assignment.kind != MessageKind::TaskAssignment
            || assignment.task_id.as_deref() != Some(task.id.as_str())
            || task.assignment_message_id != assignment.id
        {
            return Err(CollaborationError::Unauthorized("assignment_scope"));
        }
        let ack_changed = acknowledge_leased_message(
            &transaction,
            &assignment,
            request.lease_epoch,
            &request.lease_token,
            now,
        )?;
        if task.state != TaskState::Assigned {
            if ack_changed {
                return Err(CollaborationError::Conflict(
                    "assignment ACK and task state diverged".into(),
                ));
            }
            transaction.commit()?;
            return Ok(task);
        }
        let active_tasks: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collab_task
             WHERE assignee_member_id=?1 AND assignee_generation=?2
               AND id<>?3 AND state IN ('accepted','running','cancel_requested')",
            params![task.assignee_member_id, task.assignee_generation, task.id],
            |row| row.get(0),
        )?;
        if active_tasks > 0 {
            return Err(CollaborationError::Conflict(
                "runtime already has an active task".into(),
            ));
        }
        let changed = transaction.execute(
            "UPDATE collab_task SET state='accepted',version=version+1,
             accepted_at=?1,updated_at=?1
             WHERE id=?2 AND state='assigned' AND version=?3",
            params![now, task.id, task.version],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict("task accept lost CAS".into()));
        }
        if ack_changed {
            insert_event(
                &transaction,
                &task.team_id,
                "message",
                &assignment.id,
                "message_acknowledged",
                ActorType::Member,
                Some(&authenticated.member.id),
                "{}",
                now,
            )?;
        }
        insert_event(
            &transaction,
            &task.team_id,
            "task",
            &task.id,
            "task_accepted",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        let accepted = select_task(&transaction, &task.id)?;
        transaction.commit()?;
        Ok(accepted)
    }

    pub fn start_task(&self, caller: &CallerIdentity, task_id: &str) -> CollabResult<Task> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let task = select_task(&transaction, task_id)?;
        ensure_task_assignee(&task, &authenticated)?;
        if task.state == TaskState::Running {
            transaction.commit()?;
            return Ok(task);
        }
        if task.state != TaskState::Accepted {
            return Err(CollaborationError::InvalidState {
                entity: "task",
                state: task.state.to_string(),
            });
        }
        let changed = transaction.execute(
            "UPDATE collab_task SET state='running',version=version+1,
             started_at=?1,updated_at=?1
             WHERE id=?2 AND state='accepted' AND version=?3",
            params![now, task.id, task.version],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict("task start lost CAS".into()));
        }
        insert_event(
            &transaction,
            &task.team_id,
            "task",
            &task.id,
            "member_started",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        let running = select_task(&transaction, &task.id)?;
        transaction.commit()?;
        Ok(running)
    }

    pub fn report_task(
        &self,
        caller: &CallerIdentity,
        request: ReportTaskRequest,
    ) -> CollabResult<TaskMessageOutcome> {
        validate_request_id(&request.request_id)?;
        validate_payload(&request.payload_text)?;
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let task = select_task(&transaction, &request.task_id)?;
        ensure_task_assignee(&task, &authenticated)?;
        let assigner = select_member(&transaction, &task.assigner_member_id)?;
        authorize_acl(
            &transaction,
            &task.team_id,
            &authenticated.member,
            &assigner,
            AclAction::Report,
        )?;
        let fingerprint = operation_fingerprint(&json!({
            "operation": "report_task",
            "taskId": task.id,
            "status": match request.status { ReportStatus::Completed => "completed", ReportStatus::Failed => "failed" },
            "payloadText": request.payload_text,
        }))?;
        if let Some(existing) = idempotent_message(
            &transaction,
            &task.team_id,
            &authenticated.member.id,
            &request.request_id,
            &fingerprint,
        )? {
            let current = select_task(&transaction, &task.id)?;
            transaction.commit()?;
            return Ok(TaskMessageOutcome {
                task: current,
                message: existing,
            });
        }
        let target_state = match request.status {
            ReportStatus::Completed => TaskState::ReportedCompleted,
            ReportStatus::Failed => TaskState::ReportedFailed,
        };
        if task.state.is_terminal() {
            if task.state == target_state {
                if let Some(message_id) = task.terminal_report_message_id.as_deref() {
                    let existing = select_message(&transaction, message_id)?;
                    if existing.payload_text == request.payload_text {
                        transaction.commit()?;
                        return Ok(TaskMessageOutcome {
                            task,
                            message: existing,
                        });
                    }
                }
            }
            return Err(CollaborationError::Conflict(
                "task already has a terminal result".into(),
            ));
        }
        if !matches!(task.state, TaskState::Running | TaskState::CancelRequested) {
            return Err(CollaborationError::InvalidState {
                entity: "task",
                state: task.state.to_string(),
            });
        }
        let assigner_runtime = active_runtime(&transaction, &authenticated.team, &assigner)?;
        enforce_send_limits(&transaction, &task.team_id, &authenticated.member.id, now)?;
        let report_message_id = new_id();
        let message = insert_message_record(
            &transaction,
            MessageInsert {
                id: report_message_id.clone(),
                team: &authenticated.team,
                sender: &authenticated,
                recipient: &assigner,
                recipient_runtime: &assigner_runtime,
                kind: MessageKind::TaskReport,
                task_id: Some(&task.id),
                reply_to: Some(&task.assignment_message_id),
                payload_text: &request.payload_text,
                request_id: &request.request_id,
                request_fingerprint: &fingerprint,
                retry_of: None,
                not_before: now,
                expires_at: now + DEFAULT_TASK_TTL_MS,
                now,
            },
        )?;
        let changed = transaction.execute(
            "UPDATE collab_task SET state=?1,version=version+1,
             terminal_report_message_id=?2,terminal_at=?3,updated_at=?3,
             attention_state='none',attention_reason=NULL,attention_since=NULL
             WHERE id=?4 AND version=?5 AND state IN ('running','cancel_requested')",
            params![
                target_state.as_db(),
                report_message_id,
                now,
                task.id,
                task.version
            ],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict(
                "terminal report lost CAS".into(),
            ));
        }
        insert_event(
            &transaction,
            &task.team_id,
            "message",
            &message.id,
            "message_queued",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        insert_event(
            &transaction,
            &task.team_id,
            "task",
            &task.id,
            "report_submitted",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        insert_event(
            &transaction,
            &task.team_id,
            "task",
            &task.id,
            match request.status {
                ReportStatus::Completed => "task_reported_completed",
                ReportStatus::Failed => "task_reported_failed",
            },
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        let reported = select_task(&transaction, &task.id)?;
        transaction.commit()?;
        Ok(TaskMessageOutcome {
            task: reported,
            message,
        })
    }

    pub fn ack_report(
        &self,
        caller: &CallerIdentity,
        request: ReportAckRequest,
    ) -> CollabResult<Task> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let task = select_task(&transaction, &request.task_id)?;
        if task.team_id != authenticated.team.id
            || task.assigner_member_id != authenticated.member.id
            || authenticated.member.role != Role::Leader
        {
            return Err(CollaborationError::Unauthorized("report_recipient"));
        }
        let report = select_message(&transaction, &request.report_message_id)?;
        ensure_message_recipient(&report, &authenticated)?;
        if report.kind != MessageKind::TaskReport
            || report.task_id.as_deref() != Some(task.id.as_str())
            || task.terminal_report_message_id.as_deref() != Some(report.id.as_str())
        {
            return Err(CollaborationError::Unauthorized("report_scope"));
        }
        let changed = acknowledge_leased_message(
            &transaction,
            &report,
            request.lease_epoch,
            &request.lease_token,
            now,
        )?;
        if changed {
            transaction.execute(
                "UPDATE collab_task SET attention_state='none',attention_reason=NULL,
                 attention_since=NULL,updated_at=?1 WHERE id=?2",
                params![now, task.id],
            )?;
            insert_event(
                &transaction,
                &task.team_id,
                "message",
                &report.id,
                "message_acknowledged",
                ActorType::Member,
                Some(&authenticated.member.id),
                "{}",
                now,
            )?;
            insert_event(
                &transaction,
                &task.team_id,
                "task",
                &task.id,
                "report_received",
                ActorType::Member,
                Some(&authenticated.member.id),
                "{}",
                now,
            )?;
        }
        let current = select_task(&transaction, &task.id)?;
        transaction.commit()?;
        Ok(current)
    }
}

impl CollaborationService {
    /// Heartbeats are control-plane liveness only and intentionally do not
    /// append events. State edge changes use `update_runtime_state`.
    pub fn touch_runtime_heartbeat(&self, caller: &CallerIdentity) -> CollabResult<Runtime> {
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        transaction.execute(
            "UPDATE collab_runtime SET last_heartbeat_at=?1 WHERE id=?2",
            params![now, authenticated.runtime.id],
        )?;
        transaction.commit()?;
        drop(connection);
        self.store.runtime(&authenticated.runtime.id)
    }

    pub fn cancel_task(
        &self,
        caller: &CallerIdentity,
        request: CancelTaskRequest,
    ) -> CollabResult<CancelOutcome> {
        validate_request_id(&request.request_id)?;
        validate_payload(&request.reason)?;
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let task = select_task(&transaction, &request.task_id)?;
        if task.team_id != authenticated.team.id
            || task.assigner_member_id != authenticated.member.id
            || authenticated.member.role != Role::Leader
        {
            return Err(CollaborationError::Unauthorized("task_assigner"));
        }
        let assignee = select_member(&transaction, &task.assignee_member_id)?;
        authorize_acl(
            &transaction,
            &task.team_id,
            &authenticated.member,
            &assignee,
            AclAction::CancelTask,
        )?;
        let fingerprint = operation_fingerprint(&json!({
            "operation": "cancel_task",
            "taskId": task.id,
            "reason": request.reason,
        }))?;
        if let Some(existing) = idempotent_operation_request(
            &transaction,
            &task.team_id,
            &authenticated.member.id,
            &request.request_id,
            &fingerprint,
            "cancel_task",
        )? {
            let current = select_task(&transaction, &existing.task_id)?;
            let message = existing
                .result_message_id
                .as_deref()
                .map(|message_id| select_message(&transaction, message_id))
                .transpose()?;
            transaction.commit()?;
            return Ok(CancelOutcome {
                task: current,
                message,
            });
        }
        if let Some(existing) = idempotent_message(
            &transaction,
            &task.team_id,
            &authenticated.member.id,
            &request.request_id,
            &fingerprint,
        )? {
            let current = select_task(&transaction, &task.id)?;
            transaction.commit()?;
            return Ok(CancelOutcome {
                task: current,
                message: Some(existing),
            });
        }
        match task.state {
            TaskState::Assigned => {
                let changed = transaction.execute(
                    "UPDATE collab_message
                     SET state='cancelled',lease_token_hash=NULL,lease_until=NULL,
                         lease_epoch=lease_epoch+1,updated_at=?1
                     WHERE id=?2 AND state IN ('queued','suspended','leased')",
                    params![now, task.assignment_message_id],
                )?;
                if changed != 1 {
                    return Err(CollaborationError::Conflict(
                        "assignment was accepted while cancellation raced".into(),
                    ));
                }
                let task_changed = transaction.execute(
                    "UPDATE collab_task SET state='cancelled',version=version+1,
                     terminal_at=?1,updated_at=?1 WHERE id=?2 AND state='assigned' AND version=?3",
                    params![now, task.id, task.version],
                )?;
                if task_changed != 1 {
                    return Err(CollaborationError::Conflict("task cancel lost CAS".into()));
                }
                insert_event(
                    &transaction,
                    &task.team_id,
                    "message",
                    &task.assignment_message_id,
                    "message_cancelled",
                    ActorType::Member,
                    Some(&authenticated.member.id),
                    "{}",
                    now,
                )?;
                insert_event(
                    &transaction,
                    &task.team_id,
                    "task",
                    &task.id,
                    "task_cancelled",
                    ActorType::Member,
                    Some(&authenticated.member.id),
                    "{}",
                    now,
                )?;
                insert_operation_request(
                    &transaction,
                    &task.team_id,
                    &authenticated.member.id,
                    &request.request_id,
                    &fingerprint,
                    "cancel_task",
                    &task.id,
                    None,
                    now,
                )?;
                let cancelled = select_task(&transaction, &task.id)?;
                transaction.commit()?;
                Ok(CancelOutcome {
                    task: cancelled,
                    message: None,
                })
            }
            TaskState::Accepted | TaskState::Running => {
                let assignee_runtime =
                    active_runtime(&transaction, &authenticated.team, &assignee)?;
                if assignee_runtime.terminal_generation != task.assignee_generation {
                    return Err(CollaborationError::StaleGeneration);
                }
                enforce_send_limits(&transaction, &task.team_id, &authenticated.member.id, now)?;
                let cancel_message_id = new_id();
                let message = insert_message_record(
                    &transaction,
                    MessageInsert {
                        id: cancel_message_id.clone(),
                        team: &authenticated.team,
                        sender: &authenticated,
                        recipient: &assignee,
                        recipient_runtime: &assignee_runtime,
                        kind: MessageKind::TaskCancel,
                        task_id: Some(&task.id),
                        reply_to: Some(&task.assignment_message_id),
                        payload_text: &request.reason,
                        request_id: &request.request_id,
                        request_fingerprint: &fingerprint,
                        retry_of: None,
                        not_before: now,
                        expires_at: now + DEFAULT_TASK_TTL_MS,
                        now,
                    },
                )?;
                let changed = transaction.execute(
                    "UPDATE collab_task SET state='cancel_requested',version=version+1,
                     cancel_request_message_id=?1,attention_state='cancel_unconfirmed',
                     attention_reason='cancel_waiting_for_worker',attention_since=?2,updated_at=?2
                     WHERE id=?3 AND version=?4 AND state IN ('accepted','running')",
                    params![cancel_message_id, now, task.id, task.version],
                )?;
                if changed != 1 {
                    return Err(CollaborationError::Conflict("task cancel lost CAS".into()));
                }
                insert_event(
                    &transaction,
                    &task.team_id,
                    "message",
                    &message.id,
                    "message_queued",
                    ActorType::Member,
                    Some(&authenticated.member.id),
                    "{}",
                    now,
                )?;
                insert_event(
                    &transaction,
                    &task.team_id,
                    "task",
                    &task.id,
                    "task_cancel_requested",
                    ActorType::Member,
                    Some(&authenticated.member.id),
                    "{}",
                    now,
                )?;
                insert_operation_request(
                    &transaction,
                    &task.team_id,
                    &authenticated.member.id,
                    &request.request_id,
                    &fingerprint,
                    "cancel_task",
                    &task.id,
                    Some(&message.id),
                    now,
                )?;
                let current = select_task(&transaction, &task.id)?;
                transaction.commit()?;
                Ok(CancelOutcome {
                    task: current,
                    message: Some(message),
                })
            }
            TaskState::CancelRequested => Err(CollaborationError::Conflict(
                "task cancellation is already pending".into(),
            )),
            state if state.is_terminal() => {
                if state == TaskState::Cancelled {
                    transaction.commit()?;
                    Ok(CancelOutcome {
                        task,
                        message: None,
                    })
                } else {
                    Err(CollaborationError::Conflict(
                        "task already has a terminal report".into(),
                    ))
                }
            }
            _ => Err(CollaborationError::InvalidState {
                entity: "task",
                state: task.state.to_string(),
            }),
        }
    }

    pub fn ack_cancel(
        &self,
        caller: &CallerIdentity,
        request: CancelAckRequest,
    ) -> CollabResult<TaskMessageOutcome> {
        validate_request_id(&request.request_id)?;
        validate_payload(&request.payload_text)?;
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let authenticated = authenticate(&transaction, caller)?;
        let task = select_task(&transaction, &request.task_id)?;
        ensure_task_assignee(&task, &authenticated)?;
        let leader = select_member(&transaction, &task.assigner_member_id)?;
        authorize_acl(
            &transaction,
            &task.team_id,
            &authenticated.member,
            &leader,
            AclAction::AcknowledgeCancel,
        )?;
        let fingerprint = operation_fingerprint(&json!({
            "operation": "ack_cancel",
            "taskId": task.id,
            "cancelMessageId": request.cancel_message_id,
            "payloadText": request.payload_text,
        }))?;
        if let Some(existing) = idempotent_message(
            &transaction,
            &task.team_id,
            &authenticated.member.id,
            &request.request_id,
            &fingerprint,
        )? {
            let current = select_task(&transaction, &task.id)?;
            transaction.commit()?;
            return Ok(TaskMessageOutcome {
                task: current,
                message: existing,
            });
        }
        if task.state != TaskState::CancelRequested {
            return Err(if task.state.is_terminal() {
                CollaborationError::Conflict("terminal transition already won".into())
            } else {
                CollaborationError::InvalidState {
                    entity: "task",
                    state: task.state.to_string(),
                }
            });
        }
        let cancel_message = select_message(&transaction, &request.cancel_message_id)?;
        ensure_message_recipient(&cancel_message, &authenticated)?;
        if cancel_message.kind != MessageKind::TaskCancel
            || cancel_message.task_id.as_deref() != Some(task.id.as_str())
            || task.cancel_request_message_id.as_deref() != Some(cancel_message.id.as_str())
        {
            return Err(CollaborationError::Unauthorized("cancel_scope"));
        }
        let leader_runtime = active_runtime(&transaction, &authenticated.team, &leader)?;
        enforce_send_limits(&transaction, &task.team_id, &authenticated.member.id, now)?;
        let ack_changed = acknowledge_leased_message(
            &transaction,
            &cancel_message,
            request.lease_epoch,
            &request.lease_token,
            now,
        )?;
        let ack_message_id = new_id();
        let ack_message = insert_message_record(
            &transaction,
            MessageInsert {
                id: ack_message_id.clone(),
                team: &authenticated.team,
                sender: &authenticated,
                recipient: &leader,
                recipient_runtime: &leader_runtime,
                kind: MessageKind::TaskCancelAck,
                task_id: Some(&task.id),
                reply_to: Some(&cancel_message.id),
                payload_text: &request.payload_text,
                request_id: &request.request_id,
                request_fingerprint: &fingerprint,
                retry_of: None,
                not_before: now,
                expires_at: now + DEFAULT_TASK_TTL_MS,
                now,
            },
        )?;
        let changed = transaction.execute(
            "UPDATE collab_task SET state='cancelled',version=version+1,
             cancel_ack_message_id=?1,terminal_at=?2,updated_at=?2,
             attention_state='none',attention_reason=NULL,attention_since=NULL
             WHERE id=?3 AND state='cancel_requested' AND version=?4",
            params![ack_message_id, now, task.id, task.version],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict(
                "cancel acknowledgement lost terminal CAS".into(),
            ));
        }
        if ack_changed {
            insert_event(
                &transaction,
                &task.team_id,
                "message",
                &cancel_message.id,
                "message_acknowledged",
                ActorType::Member,
                Some(&authenticated.member.id),
                "{}",
                now,
            )?;
        }
        insert_event(
            &transaction,
            &task.team_id,
            "message",
            &ack_message.id,
            "message_queued",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        insert_event(
            &transaction,
            &task.team_id,
            "task",
            &task.id,
            "task_cancel_acknowledged",
            ActorType::Member,
            Some(&authenticated.member.id),
            "{}",
            now,
        )?;
        let cancelled = select_task(&transaction, &task.id)?;
        transaction.commit()?;
        Ok(TaskMessageOutcome {
            task: cancelled,
            message: ack_message,
        })
    }

    pub fn recover(&self, now: i64) -> CollabResult<RecoverySummary> {
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut summary = expire_and_requeue(&transaction, now)?;
        let global: String = transaction.query_row(
            "SELECT value FROM collab_meta WHERE key='global_enabled'",
            [],
            |row| row.get(0),
        )?;
        if global != "1" {
            suspend_messages(&transaction, None, now, "global_disabled")?;
            transaction.commit()?;
            return Ok(summary);
        }
        let stale_ids = query_message_ids(
            &transaction,
            "SELECT m.id FROM collab_message m
             JOIN collab_team t ON t.id=m.team_id
             LEFT JOIN collab_runtime r ON r.member_id=m.recipient_member_id
              AND r.terminal_generation=m.recipient_generation AND r.revoked_at IS NULL
             WHERE m.state IN ('queued','leased','suspended')
               AND t.enabled=1 AND (r.id IS NULL OR r.routing_revision<>t.routing_revision
                 OR m.routing_revision<>t.routing_revision)",
            [],
        )?;
        for id in stale_ids {
            let message = select_message(&transaction, &id)?;
            transaction.execute(
                "UPDATE collab_message SET state='blocked',blocked_at=?1,
                 blocked_reason='stale_target',resolution_policy='user_retry',
                 lease_token_hash=NULL,lease_until=NULL,lease_epoch=lease_epoch+1,updated_at=?1
                 WHERE id=?2 AND state IN ('queued','leased','suspended')",
                params![now, id],
            )?;
            insert_event(
                &transaction,
                &message.team_id,
                "message",
                &id,
                "message_blocked",
                ActorType::System,
                None,
                r#"{"reason":"stale_target"}"#,
                now,
            )?;
            summary.messages_blocked += 1;
        }
        let uncertain_tasks = {
            let mut statement = transaction.prepare(
                "SELECT task.id,task.team_id FROM collab_task task
                 LEFT JOIN collab_runtime r ON r.member_id=task.assignee_member_id
                  AND r.terminal_generation=task.assignee_generation AND r.revoked_at IS NULL
                 WHERE task.state IN ('accepted','running','cancel_requested')
                   AND r.id IS NULL AND task.attention_state<>'uncertain_execution'",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (task_id, team_id) in uncertain_tasks {
            transaction.execute(
                "UPDATE collab_task SET attention_state='uncertain_execution',
                 attention_reason='runtime_missing_after_recovery',attention_since=?1,
                 updated_at=?1,version=version+1 WHERE id=?2",
                params![now, task_id],
            )?;
            insert_event(
                &transaction,
                &team_id,
                "task",
                &task_id,
                "task_needs_attention",
                ActorType::System,
                None,
                r#"{"reason":"uncertain_execution"}"#,
                now,
            )?;
            summary.tasks_needing_attention += 1;
        }
        transaction.commit()?;
        Ok(summary)
    }
}

impl CollaborationService {
    pub fn team_configuration(&self, team_id: &str) -> CollabResult<TeamConfiguration> {
        let connection = self.store.lock()?;
        load_team_configuration(&connection, team_id)
    }

    /// Replaces the entire editable roster in one transaction. The team must
    /// be paused and task-free, so a failed save can never leave a half-new
    /// routing graph. Display-only changes do not invalidate runtimes.
    pub fn replace_team_config(
        &self,
        team_id: &str,
        request: ReplaceTeamConfigRequest,
    ) -> CollabResult<TeamConfiguration> {
        validate_nonempty("team name", &request.name)?;
        validate_nonempty("workspace fingerprint", &request.workspace_fingerprint)?;
        let enabled_count = request
            .members
            .iter()
            .filter(|member| member.enabled)
            .count();
        if request.members.len() > 4 || !(2..=4).contains(&enabled_count) {
            return Err(CollaborationError::Capacity("team_members"));
        }
        if request
            .members
            .iter()
            .filter(|member| member.enabled && member.role == Role::Leader)
            .count()
            != 1
        {
            return Err(CollaborationError::InvalidInput(
                "roster requires exactly one enabled leader".into(),
            ));
        }
        let worker_count = request
            .members
            .iter()
            .filter(|member| member.enabled && member.role == Role::Worker)
            .count();
        if !(1..=3).contains(&worker_count) {
            return Err(CollaborationError::InvalidInput(
                "roster requires 1-3 enabled workers".into(),
            ));
        }
        let mut aliases = std::collections::BTreeSet::new();
        let mut sessions = std::collections::BTreeSet::new();
        for member in &request.members {
            validate_alias(&member.alias)?;
            validate_nonempty("display name", &member.display_name)?;
            validate_nonempty("avatar id", &member.avatar_id)?;
            if !aliases.insert(member.alias.clone()) {
                return Err(CollaborationError::InvalidInput(
                    "member aliases must be unique".into(),
                ));
            }
            if member.enabled {
                if let Some(session) = member.grok_session_id.as_deref() {
                    validate_nonempty("Grok session id", session)?;
                    if !sessions.insert(session.to_owned()) {
                        return Err(CollaborationError::InvalidInput(
                            "Grok sessions must be unique".into(),
                        ));
                    }
                }
            }
        }

        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let team = select_team(&transaction, team_id)?;
        ensure_team_mutable(&team)?;
        if team.enabled {
            return Err(CollaborationError::InvalidState {
                entity: "team",
                state: "enabled".into(),
            });
        }
        let active_tasks: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM collab_task WHERE team_id=?1
             AND state NOT IN ('reported_completed','reported_failed','cancelled')",
            [team_id],
            |row| row.get(0),
        )?;
        if active_tasks > 0 {
            return Err(CollaborationError::Conflict(
                "team has non-terminal tasks".into(),
            ));
        }

        let existing = load_team_configuration(&transaction, team_id)?;
        let existing_by_id: std::collections::BTreeMap<_, _> = existing
            .members
            .iter()
            .map(|member| (member.id.clone(), member))
            .collect();
        let binding_by_member: std::collections::BTreeMap<_, _> = existing
            .bindings
            .iter()
            .filter(|binding| binding.released_at.is_none())
            .map(|binding| (binding.member_id.clone(), binding.grok_session_id.clone()))
            .collect();
        for member in &request.members {
            if let Some(id) = member.id.as_deref() {
                let current = existing_by_id
                    .get(id)
                    .ok_or_else(|| CollaborationError::Unauthorized("member_not_in_team"))?;
                if current.team_id != team_id {
                    return Err(CollaborationError::Unauthorized("member_not_in_team"));
                }
            }
        }
        let requested_enabled_ids: std::collections::BTreeSet<_> = request
            .members
            .iter()
            .filter(|member| member.enabled)
            .filter_map(|member| member.id.clone())
            .collect();
        let current_enabled_ids: std::collections::BTreeSet<_> = existing
            .members
            .iter()
            .filter(|member| member.enabled)
            .map(|member| member.id.clone())
            .collect();
        let mut routing_changed = team.workspace_fingerprint != request.workspace_fingerprint
            || requested_enabled_ids != current_enabled_ids
            || request
                .members
                .iter()
                .any(|member| member.enabled && member.id.is_none());
        for requested in request.members.iter().filter(|member| member.enabled) {
            if let Some(id) = requested.id.as_deref() {
                let current = existing_by_id[id];
                if current.alias != requested.alias
                    || current.role != requested.role
                    || binding_by_member.get(id).map(String::as_str)
                        != requested.grok_session_id.as_deref()
                {
                    routing_changed = true;
                }
            }
        }

        if routing_changed {
            transaction.execute(
                "UPDATE collab_member SET enabled=0,updated_at=?1 WHERE team_id=?2",
                params![now, team_id],
            )?;
            transaction.execute(
                "UPDATE collab_binding SET released_at=?1 WHERE released_at IS NULL
                 AND member_id IN (SELECT id FROM collab_member WHERE team_id=?2)",
                params![now, team_id],
            )?;
            transaction.execute(
                "UPDATE collab_runtime SET revoked_at=?1 WHERE revoked_at IS NULL
                 AND member_id IN (SELECT id FROM collab_member WHERE team_id=?2)",
                params![now, team_id],
            )?;
            for requested in &request.members {
                if let Some(id) = requested.id.as_deref() {
                    transaction.execute(
                        "UPDATE collab_member SET alias=?1 WHERE id=?2 AND team_id=?3",
                        params![format!("__teak_edit_{id}"), id, team_id],
                    )?;
                }
            }
        }

        let mut resolved_ids = Vec::with_capacity(request.members.len());
        for requested in &request.members {
            let id = requested.id.clone().unwrap_or_else(new_id);
            if requested.id.is_some() {
                transaction.execute(
                    "UPDATE collab_member SET alias=?1,display_name=?2,avatar_id=?3,
                     role=?4,enabled=?5,updated_at=?6 WHERE id=?7 AND team_id=?8",
                    params![
                        requested.alias,
                        requested.display_name,
                        requested.avatar_id,
                        requested.role.as_db(),
                        requested.enabled,
                        now,
                        id,
                        team_id
                    ],
                )?;
            } else {
                transaction.execute(
                    "INSERT INTO collab_member(
                       id,team_id,alias,display_name,avatar_id,role,enabled,created_at,updated_at
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
                    params![
                        id,
                        team_id,
                        requested.alias,
                        requested.display_name,
                        requested.avatar_id,
                        requested.role.as_db(),
                        requested.enabled,
                        now
                    ],
                )?;
            }
            if routing_changed && requested.enabled {
                if let Some(grok_session_id) = requested.grok_session_id.as_deref() {
                    transaction.execute(
                        "INSERT INTO collab_binding(
                           id,member_id,provider,grok_session_id,bound_at
                         ) VALUES (?1,?2,'grok-build',?3,?4)",
                        params![new_id(), id, grok_session_id, now],
                    )?;
                }
            }
            resolved_ids.push(id);
        }
        if routing_changed {
            transaction.execute("DELETE FROM collab_acl WHERE team_id=?1", [team_id])?;
            let leader_id = request
                .members
                .iter()
                .zip(&resolved_ids)
                .find(|(member, _)| member.enabled && member.role == Role::Leader)
                .map(|(_, id)| id)
                .expect("validated leader");
            for (member, worker_id) in request.members.iter().zip(&resolved_ids) {
                if !member.enabled || member.role != Role::Worker {
                    continue;
                }
                transaction.execute(
                    "INSERT INTO collab_acl(
                      team_id,from_member_id,to_member_id,can_message,can_assign_task,
                      can_report,can_cancel_task,can_ack_cancel
                     ) VALUES (?1,?2,?3,1,1,0,1,0)",
                    params![team_id, leader_id, worker_id],
                )?;
                transaction.execute(
                    "INSERT INTO collab_acl(
                      team_id,from_member_id,to_member_id,can_message,can_assign_task,
                      can_report,can_cancel_task,can_ack_cancel
                     ) VALUES (?1,?2,?3,1,0,1,0,1)",
                    params![team_id, worker_id, leader_id],
                )?;
            }
            transaction.execute(
                "UPDATE collab_team SET name=?1,workspace_fingerprint=?2,
                 config_revision=config_revision+1,routing_revision=routing_revision+1,
                 updated_at=?3 WHERE id=?4",
                params![request.name, request.workspace_fingerprint, now, team_id],
            )?;
            block_routing_revision(&transaction, team_id, now)?;
        } else {
            transaction.execute(
                "UPDATE collab_team SET name=?1,config_revision=config_revision+1,
                 updated_at=?2 WHERE id=?3",
                params![request.name, now, team_id],
            )?;
        }
        insert_event(
            &transaction,
            team_id,
            "team",
            team_id,
            "team_config_replaced",
            ActorType::User,
            None,
            &json!({"routingChanged": routing_changed}).to_string(),
            now,
        )?;
        let result = load_team_configuration(&transaction, team_id)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn list_team_configurations(&self) -> CollabResult<Vec<TeamConfiguration>> {
        let connection = self.store.lock()?;
        let team_ids = {
            let mut statement = connection.prepare(
                "SELECT id FROM collab_team WHERE archived_at IS NULL ORDER BY created_at,id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        team_ids
            .iter()
            .map(|team_id| load_team_configuration(&connection, team_id))
            .collect()
    }

    /// Trusted broker lookup used during launch/attach. Both native session
    /// and the canonical workspace fingerprint must match an enabled member.
    pub fn resolve_enabled_binding(
        &self,
        grok_session_id: &str,
        workspace_fingerprint: &str,
    ) -> CollabResult<Option<ResolvedBinding>> {
        let connection = self.store.lock()?;
        let ids: Option<(String, String, String, Option<String>)> = connection
            .query_row(
                "SELECT t.id,m.id,b.id,r.id
                 FROM collab_team t
                 JOIN collab_member m ON m.team_id=t.id AND m.enabled=1
                 JOIN collab_binding b ON b.member_id=m.id AND b.released_at IS NULL
                 LEFT JOIN collab_runtime r ON r.member_id=m.id AND r.revoked_at IS NULL
                 WHERE t.enabled=1 AND t.archived_at IS NULL
                   AND t.provider='grok-build' AND t.workspace_fingerprint=?1
                   AND b.provider='grok-build' AND b.grok_session_id=?2
                   AND (SELECT value FROM collab_meta WHERE key='global_enabled')='1'",
                params![workspace_fingerprint, grok_session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((team_id, member_id, binding_id, runtime_id)) = ids else {
            return Ok(None);
        };
        let team = select_team(&connection, &team_id)?;
        let member = select_member(&connection, &member_id)?;
        let binding = connection.query_row(
            "SELECT id,member_id,provider,grok_session_id,bound_at,released_at
             FROM collab_binding WHERE id=?1",
            [&binding_id],
            |row| {
                Ok(Binding {
                    id: row.get(0)?,
                    member_id: row.get(1)?,
                    provider: row.get(2)?,
                    grok_session_id: row.get(3)?,
                    bound_at: row.get(4)?,
                    released_at: row.get(5)?,
                })
            },
        )?;
        let active_runtime = runtime_id
            .as_deref()
            .map(|runtime_id| super::store::select_runtime(&connection, runtime_id))
            .transpose()?;
        Ok(Some(ResolvedBinding {
            team,
            member,
            binding,
            active_runtime,
        }))
    }

    /// Detects a live attach collision even while the owning team is paused.
    pub fn active_runtime_for_grok_session(
        &self,
        grok_session_id: &str,
    ) -> CollabResult<Option<Runtime>> {
        let connection = self.store.lock()?;
        connection
            .query_row(
                &format!(
                    "SELECT {RUNTIME_COLUMNS} FROM collab_runtime
                     WHERE id=(SELECT r.id FROM collab_runtime r
                       JOIN collab_binding b ON b.id=r.binding_id
                       WHERE b.grok_session_id=?1 AND b.released_at IS NULL
                         AND r.revoked_at IS NULL)"
                ),
                [grok_session_id],
                runtime_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Revokes only the exact observed terminal generation. Requiring the
    /// generation prevents a late process-exit callback from revoking a newly
    /// attached runtime that reused the terminal session ID.
    pub fn revoke_terminal_runtime(
        &self,
        terminal_session_id: &str,
        terminal_generation: i64,
        reason_code: &str,
    ) -> CollabResult<bool> {
        validate_nonempty("terminal session id", terminal_session_id)?;
        validate_nonempty("runtime revocation reason", reason_code)?;
        let now = now_ms();
        let mut connection = self.store.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let runtime = transaction
            .query_row(
                &format!(
                    "SELECT {RUNTIME_COLUMNS} FROM collab_runtime
                     WHERE terminal_session_id=?1 AND terminal_generation=?2
                       AND revoked_at IS NULL"
                ),
                params![terminal_session_id, terminal_generation],
                runtime_from_row,
            )
            .optional()?;
        let Some(runtime) = runtime else {
            transaction.commit()?;
            return Ok(false);
        };
        let member = select_member(&transaction, &runtime.member_id)?;
        transaction.execute(
            "UPDATE collab_runtime SET revoked_at=?1,listener_state='offline',runtime_state='exited'
             WHERE id=?2 AND revoked_at IS NULL",
            params![now, runtime.id],
        )?;
        block_generation(
            &transaction,
            &member.team_id,
            &member.id,
            runtime.terminal_generation,
            now,
            "stale_target",
        )?;
        let tasks = {
            let mut statement = transaction.prepare(
                "SELECT id FROM collab_task WHERE assignee_member_id=?1
                 AND assignee_generation=?2
                 AND state IN ('accepted','running','cancel_requested')",
            )?;
            let rows = statement
                .query_map(params![member.id, runtime.terminal_generation], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        transaction.execute(
            "UPDATE collab_task SET attention_state='uncertain_execution',
             attention_reason=?1,attention_since=COALESCE(attention_since,?2),
             updated_at=?2,version=version+1
             WHERE assignee_member_id=?3 AND assignee_generation=?4
               AND state IN ('accepted','running','cancel_requested')",
            params![reason_code, now, member.id, runtime.terminal_generation],
        )?;
        for task_id in tasks {
            insert_event(
                &transaction,
                &member.team_id,
                "task",
                &task_id,
                "task_needs_attention",
                ActorType::Broker,
                None,
                &json!({"reason": reason_code}).to_string(),
                now,
            )?;
        }
        insert_event(
            &transaction,
            &member.team_id,
            "runtime",
            &runtime.id,
            "member_offline",
            ActorType::Broker,
            Some(&member.id),
            &json!({"reason": reason_code, "generation": terminal_generation}).to_string(),
            now,
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// A GUI broker process owns every active collaboration runtime. After a
    /// crash/restart none of the persisted bearer generations can still have
    /// a legitimate listener, so retire them before accepting new launches.
    /// The exact-generation revoke path preserves the same stale-target and
    /// uncertain-execution accounting as an observed PTY exit.
    pub fn revoke_all_active_runtimes(&self, reason_code: &str) -> CollabResult<i64> {
        validate_nonempty("runtime revocation reason", reason_code)?;
        let runtimes = {
            let connection = self.store.lock()?;
            let mut statement = connection.prepare(
                "SELECT terminal_session_id,terminal_generation FROM collab_runtime
                 WHERE revoked_at IS NULL ORDER BY created_at,id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut revoked = 0;
        for (terminal_session_id, generation) in runtimes {
            if self.revoke_terminal_runtime(&terminal_session_id, generation, reason_code)? {
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    /// Retires every active generation owned by one team without touching its
    /// PTYs. The exact-generation revoke path also blocks stale delivery and
    /// preserves the existing uncertain-execution accounting.
    pub fn revoke_team_active_runtimes(
        &self,
        team_id: &str,
        reason_code: &str,
    ) -> CollabResult<i64> {
        validate_nonempty("team id", team_id)?;
        validate_nonempty("runtime revocation reason", reason_code)?;
        let runtimes = {
            let connection = self.store.lock()?;
            // Resolve the team first so a typo cannot silently look like a
            // successful lifecycle reconciliation.
            let _ = select_team(&connection, team_id)?;
            let mut statement = connection.prepare(
                "SELECT r.terminal_session_id,r.terminal_generation
                 FROM collab_runtime r
                 JOIN collab_member m ON m.id=r.member_id
                 WHERE m.team_id=?1 AND r.revoked_at IS NULL
                 ORDER BY r.created_at,r.id",
            )?;
            let rows = statement
                .query_map([team_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut revoked = 0;
        for (terminal_session_id, generation) in runtimes {
            if self.revoke_terminal_runtime(&terminal_session_id, generation, reason_code)? {
                revoked += 1;
            }
        }
        Ok(revoked)
    }
}

struct MessageInsert<'a> {
    id: String,
    team: &'a Team,
    sender: &'a AuthenticatedCaller,
    recipient: &'a Member,
    recipient_runtime: &'a Runtime,
    kind: MessageKind,
    task_id: Option<&'a str>,
    reply_to: Option<&'a str>,
    payload_text: &'a str,
    request_id: &'a str,
    request_fingerprint: &'a str,
    retry_of: Option<&'a str>,
    not_before: i64,
    expires_at: i64,
    now: i64,
}

fn member_by_alias(
    transaction: &Transaction<'_>,
    team_id: &str,
    alias: &str,
) -> CollabResult<Member> {
    transaction
        .query_row(
            "SELECT id,team_id,alias,display_name,avatar_id,role,enabled,created_at,updated_at
             FROM collab_member WHERE team_id=?1 AND alias=?2 AND enabled=1",
            params![team_id, alias],
            |row| {
                Ok(Member {
                    id: row.get(0)?,
                    team_id: row.get(1)?,
                    alias: row.get(2)?,
                    display_name: row.get(3)?,
                    avatar_id: row.get(4)?,
                    role: super::store::enum_at(row, 5)?,
                    enabled: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            },
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                CollaborationError::Unauthorized("target_not_allowed")
            }
            other => CollaborationError::Database(other),
        })
}

fn load_team_configuration(
    connection: &Connection,
    team_id: &str,
) -> CollabResult<TeamConfiguration> {
    let team = select_team(connection, team_id)?;
    let members = {
        let mut statement = connection.prepare(
            "SELECT id,team_id,alias,display_name,avatar_id,role,enabled,created_at,updated_at
             FROM collab_member WHERE team_id=?1 ORDER BY created_at,id",
        )?;
        let rows = statement
            .query_map([team_id], |row| {
                Ok(Member {
                    id: row.get(0)?,
                    team_id: row.get(1)?,
                    alias: row.get(2)?,
                    display_name: row.get(3)?,
                    avatar_id: row.get(4)?,
                    role: super::store::enum_at(row, 5)?,
                    enabled: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let bindings = {
        let mut statement = connection.prepare(
            "SELECT b.id,b.member_id,b.provider,b.grok_session_id,b.bound_at,b.released_at
             FROM collab_binding b JOIN collab_member m ON m.id=b.member_id
             WHERE m.team_id=?1 ORDER BY b.bound_at,b.id",
        )?;
        let rows = statement
            .query_map([team_id], |row| {
                Ok(Binding {
                    id: row.get(0)?,
                    member_id: row.get(1)?,
                    provider: row.get(2)?,
                    grok_session_id: row.get(3)?,
                    bound_at: row.get(4)?,
                    released_at: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    Ok(TeamConfiguration {
        team,
        members,
        bindings,
    })
}

fn active_runtime(
    transaction: &Transaction<'_>,
    team: &Team,
    member: &Member,
) -> CollabResult<Runtime> {
    let runtime = transaction
        .query_row(
            &format!(
                "SELECT {RUNTIME_COLUMNS} FROM collab_runtime
                 WHERE member_id=?1 AND revoked_at IS NULL"
            ),
            [&member.id],
            runtime_from_row,
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                CollaborationError::Unauthorized("target_runtime_unavailable")
            }
            other => CollaborationError::Database(other),
        })?;
    if runtime.routing_revision != team.routing_revision
        || runtime.attested_provider != PROVIDER_GROK_BUILD
        || runtime.attested_workspace_fingerprint != team.workspace_fingerprint
    {
        return Err(CollaborationError::Unauthorized("target_runtime_scope"));
    }
    Ok(runtime)
}

fn authorize_acl(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender: &Member,
    recipient: &Member,
    action: AclAction,
) -> CollabResult<()> {
    if sender.team_id != team_id || recipient.team_id != team_id || sender.id == recipient.id {
        return Err(CollaborationError::Unauthorized("target_not_allowed"));
    }
    let column = match action {
        AclAction::Message => "can_message",
        AclAction::AssignTask => "can_assign_task",
        AclAction::Report => "can_report",
        AclAction::CancelTask => "can_cancel_task",
        AclAction::AcknowledgeCancel => "can_ack_cancel",
    };
    let allowed: Option<bool> = transaction
        .query_row(
            &format!(
                "SELECT {column} FROM collab_acl
                 WHERE team_id=?1 AND from_member_id=?2 AND to_member_id=?3"
            ),
            params![team_id, sender.id, recipient.id],
            |row| row.get(0),
        )
        .optional()?;
    if allowed != Some(true) {
        return Err(CollaborationError::Unauthorized("acl"));
    }
    Ok(())
}

fn validate_request_id(request_id: &str) -> CollabResult<()> {
    if request_id.trim().is_empty() || request_id.len() > 200 {
        return Err(CollaborationError::InvalidInput(
            "request id must be 1-200 bytes".into(),
        ));
    }
    Ok(())
}

fn validate_payload(payload: &str) -> CollabResult<()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(CollaborationError::Capacity("payload_64_kib"));
    }
    if payload.as_bytes().contains(&0) {
        return Err(CollaborationError::InvalidInput(
            "payload cannot contain NUL".into(),
        ));
    }
    Ok(())
}

fn validate_delivery_window(not_before: i64, expires_at: i64, now: i64) -> CollabResult<()> {
    if expires_at <= not_before || expires_at <= now {
        return Err(CollaborationError::InvalidInput(
            "message expiry must be after delivery time".into(),
        ));
    }
    Ok(())
}

fn operation_fingerprint(value: &serde_json::Value) -> CollabResult<String> {
    let canonical = serde_json::to_vec(value).map_err(|error| {
        CollaborationError::InvalidInput(format!("cannot canonicalize request: {error}"))
    })?;
    Ok(sha256_hex(&canonical))
}

fn idempotent_message(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender_member_id: &str,
    request_id: &str,
    fingerprint: &str,
) -> CollabResult<Option<Message>> {
    let existing = transaction
        .query_row(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM collab_message
                 WHERE team_id=?1 AND sender_member_id=?2 AND request_id=?3"
            ),
            params![team_id, sender_member_id, request_id],
            message_from_row,
        )
        .optional()?;
    if let Some(message) = existing {
        if message.request_fingerprint != fingerprint {
            return Err(CollaborationError::Conflict(
                "request id was already used with different semantics".into(),
            ));
        }
        return Ok(Some(message));
    }
    let operation_exists = transaction
        .query_row(
            "SELECT 1 FROM collab_operation_request
             WHERE team_id=?1 AND sender_member_id=?2 AND request_id=?3",
            params![team_id, sender_member_id, request_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if operation_exists {
        return Err(CollaborationError::Conflict(
            "request id was already used with different semantics".into(),
        ));
    }
    Ok(None)
}

struct OperationRequestRecord {
    task_id: String,
    result_message_id: Option<String>,
}

fn idempotent_operation_request(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender_member_id: &str,
    request_id: &str,
    fingerprint: &str,
    operation: &str,
) -> CollabResult<Option<OperationRequestRecord>> {
    let existing = transaction
        .query_row(
            "SELECT request_fingerprint,operation,task_id,result_message_id
             FROM collab_operation_request
             WHERE team_id=?1 AND sender_member_id=?2 AND request_id=?3",
            params![team_id, sender_member_id, request_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((existing_fingerprint, existing_operation, task_id, result_message_id)) = existing
    else {
        return Ok(None);
    };
    if existing_fingerprint != fingerprint || existing_operation != operation {
        return Err(CollaborationError::Conflict(
            "request id was already used with different semantics".into(),
        ));
    }
    Ok(Some(OperationRequestRecord {
        task_id,
        result_message_id,
    }))
}

#[allow(clippy::too_many_arguments)]
fn insert_operation_request(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender_member_id: &str,
    request_id: &str,
    fingerprint: &str,
    operation: &str,
    task_id: &str,
    result_message_id: Option<&str>,
    now: i64,
) -> CollabResult<()> {
    transaction.execute(
        "INSERT INTO collab_operation_request(
             team_id,sender_member_id,request_id,request_fingerprint,
             operation,task_id,result_message_id,created_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            team_id,
            sender_member_id,
            request_id,
            fingerprint,
            operation,
            task_id,
            result_message_id,
            now
        ],
    )?;
    Ok(())
}

fn enforce_send_limits(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender_member_id: &str,
    now: i64,
) -> CollabResult<()> {
    let pending: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM collab_message
         WHERE team_id=?1 AND state IN ('queued','suspended','leased')",
        [team_id],
        |row| row.get(0),
    )?;
    if pending >= MAX_PENDING_MESSAGES_PER_TEAM {
        return Err(CollaborationError::Capacity("team_pending_messages"));
    }
    let recent: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM collab_message
         WHERE sender_member_id=?1 AND created_at>=?2",
        params![sender_member_id, now - 60_000],
        |row| row.get(0),
    )?;
    if recent >= MAX_MESSAGES_PER_MEMBER_PER_MINUTE {
        return Err(CollaborationError::Capacity("member_rate_per_minute"));
    }
    Ok(())
}

fn allocate_edge_sequence(
    transaction: &Transaction<'_>,
    sender_member_id: &str,
    recipient_member_id: &str,
) -> CollabResult<i64> {
    transaction.execute(
        "INSERT OR IGNORE INTO collab_edge_cursor(
            sender_member_id,recipient_member_id,next_sequence
         ) VALUES (?1,?2,1)",
        params![sender_member_id, recipient_member_id],
    )?;
    let sequence: i64 = transaction.query_row(
        "UPDATE collab_edge_cursor SET next_sequence=next_sequence+1
         WHERE sender_member_id=?1 AND recipient_member_id=?2
         RETURNING next_sequence-1",
        params![sender_member_id, recipient_member_id],
        |row| row.get(0),
    )?;
    Ok(sequence)
}

fn insert_message_record(
    transaction: &Transaction<'_>,
    input: MessageInsert<'_>,
) -> CollabResult<Message> {
    let edge_sequence =
        allocate_edge_sequence(transaction, &input.sender.member.id, &input.recipient.id)?;
    transaction.execute(
        "INSERT INTO collab_message(
            id,team_id,sender_member_id,sender_generation,recipient_member_id,
            recipient_generation,routing_revision,kind,task_id,reply_to_message_id,
            payload_text,request_id,request_fingerprint,retry_of_message_id,
            edge_sequence,state,lease_epoch,attempt_count,not_before,expires_at,
            created_at,updated_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,
                   ?15,'queued',0,0,?16,?17,?18,?18)",
        params![
            input.id,
            input.team.id,
            input.sender.member.id,
            input.sender.runtime.terminal_generation,
            input.recipient.id,
            input.recipient_runtime.terminal_generation,
            input.team.routing_revision,
            input.kind.as_db(),
            input.task_id,
            input.reply_to,
            input.payload_text,
            input.request_id,
            input.request_fingerprint,
            input.retry_of,
            edge_sequence,
            input.not_before,
            input.expires_at,
            input.now,
        ],
    )?;
    select_message(transaction, &input.id)
}

fn validate_reply(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender_member_id: &str,
    recipient_member_id: &str,
    reply_to: Option<&str>,
    expected_task_id: Option<&str>,
) -> CollabResult<()> {
    let Some(reply_to) = reply_to else {
        return Ok(());
    };
    let message = select_message(transaction, reply_to)?;
    if message.team_id != team_id
        || !((message.sender_member_id == sender_member_id
            && message.recipient_member_id == recipient_member_id)
            || (message.sender_member_id == recipient_member_id
                && message.recipient_member_id == sender_member_id))
    {
        return Err(CollaborationError::Unauthorized("reply_scope"));
    }
    if let Some(task_id) = expected_task_id {
        if message.task_id.as_deref() != Some(task_id) {
            return Err(CollaborationError::Unauthorized("reply_task_scope"));
        }
    }
    Ok(())
}

fn validate_retry_of(
    transaction: &Transaction<'_>,
    team_id: &str,
    sender_member_id: &str,
    recipient_member_id: &str,
    kind: MessageKind,
    task_id: Option<&str>,
    retry_of: Option<&str>,
) -> CollabResult<()> {
    let Some(retry_of) = retry_of else {
        return Ok(());
    };
    let previous = select_message(transaction, retry_of)?;
    if previous.team_id != team_id
        || previous.sender_member_id != sender_member_id
        || previous.recipient_member_id != recipient_member_id
        || previous.kind != kind
        || previous.task_id.as_deref() != task_id
        || !matches!(
            previous.state,
            MessageState::Blocked | MessageState::Expired | MessageState::DeadLetter
        )
    {
        return Err(CollaborationError::Unauthorized("retry_scope"));
    }
    Ok(())
}

fn ensure_task_assignee(task: &Task, caller: &AuthenticatedCaller) -> CollabResult<()> {
    if task.team_id != caller.team.id || task.assignee_member_id != caller.member.id {
        return Err(CollaborationError::Unauthorized("task_owner"));
    }
    if task.assignee_generation != caller.runtime.terminal_generation {
        return Err(CollaborationError::StaleGeneration);
    }
    Ok(())
}

fn ensure_message_recipient(message: &Message, caller: &AuthenticatedCaller) -> CollabResult<()> {
    if message.team_id != caller.team.id || message.recipient_member_id != caller.member.id {
        return Err(CollaborationError::Unauthorized("message_recipient"));
    }
    if message.recipient_generation != caller.runtime.terminal_generation
        || message.routing_revision != caller.team.routing_revision
    {
        return Err(CollaborationError::StaleGeneration);
    }
    Ok(())
}

fn next_pending_message(
    transaction: &Transaction<'_>,
    caller: &AuthenticatedCaller,
    now: i64,
) -> CollabResult<Option<Message>> {
    transaction
        .query_row(
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM collab_message m
                 WHERE m.recipient_member_id=?1 AND m.recipient_generation=?2
                   AND m.team_id=?3 AND m.routing_revision=?4
                   AND m.state='queued' AND m.not_before<=?5 AND m.expires_at>?5
                   AND NOT EXISTS (
                     SELECT 1 FROM collab_message earlier
                     WHERE earlier.sender_member_id=m.sender_member_id
                       AND earlier.recipient_member_id=m.recipient_member_id
                       AND earlier.edge_sequence<m.edge_sequence
                       AND earlier.state IN ('queued','suspended','leased')
                   )
                 ORDER BY m.created_at ASC,m.edge_sequence ASC LIMIT 1"
            ),
            params![
                caller.member.id,
                caller.runtime.terminal_generation,
                caller.team.id,
                caller.team.routing_revision,
                now
            ],
            message_from_row,
        )
        .optional()
        .map_err(Into::into)
}

/// Returns true only for the first successful ACK. Replaying the exact winning
/// epoch/token is a successful no-op; every other late token is rejected.
fn acknowledge_leased_message(
    transaction: &Transaction<'_>,
    message: &Message,
    lease_epoch: i64,
    lease_token: &str,
    now: i64,
) -> CollabResult<bool> {
    let (state, current_epoch, lease_hash, ack_hash, acknowledged_epoch): (
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = transaction.query_row(
        "SELECT state,lease_epoch,lease_token_hash,ack_token_hash,
                acknowledged_lease_epoch FROM collab_message WHERE id=?1",
        [&message.id],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        },
    )?;
    let provided_hash = hash_secret(lease_token);
    if state == MessageState::Acknowledged.as_db() {
        if acknowledged_epoch == Some(lease_epoch)
            && ack_hash.as_deref().is_some_and(|expected| {
                constant_time_eq(expected.as_bytes(), provided_hash.as_bytes())
            })
        {
            return Ok(false);
        }
        return Err(CollaborationError::Conflict("stale or losing ACK".into()));
    }
    if state != MessageState::Leased.as_db() || current_epoch != lease_epoch {
        return Err(CollaborationError::Conflict("stale lease epoch".into()));
    }
    let expected_hash = lease_hash.ok_or_else(|| {
        CollaborationError::Conflict("leased message has no token verifier".into())
    })?;
    if !constant_time_eq(expected_hash.as_bytes(), provided_hash.as_bytes()) {
        return Err(CollaborationError::Unauthorized("lease_token"));
    }
    let changed = transaction.execute(
        "UPDATE collab_message
         SET state='acknowledged',ack_token_hash=lease_token_hash,
             acknowledged_lease_epoch=lease_epoch,acknowledged_at=?1,
             lease_token_hash=NULL,lease_until=NULL,updated_at=?1
         WHERE id=?2 AND state='leased' AND lease_epoch=?3",
        params![now, message.id, lease_epoch],
    )?;
    if changed != 1 {
        return Err(CollaborationError::Conflict(
            "ACK lost compare-and-swap".into(),
        ));
    }
    Ok(true)
}

fn expire_and_requeue(transaction: &Transaction<'_>, now: i64) -> CollabResult<RecoverySummary> {
    let mut summary = RecoverySummary::default();
    let expiring = query_message_ids(
        transaction,
        "SELECT id FROM collab_message
         WHERE state IN ('queued','leased','suspended') AND expires_at<=?1",
        [now],
    )?;
    for id in expiring {
        let message = select_message(transaction, &id)?;
        transaction.execute(
            "UPDATE collab_message
             SET state='expired',lease_token_hash=NULL,lease_until=NULL,
                 last_error_code='ttl_expired',updated_at=?1 WHERE id=?2",
            params![now, id],
        )?;
        insert_event(
            transaction,
            &message.team_id,
            "message",
            &id,
            "message_expired",
            ActorType::System,
            None,
            "{}",
            now,
        )?;
        mark_delivery_attention(transaction, &message, now, "ttl_expired")?;
        summary.messages_expired += 1;
    }
    let expired_leases = query_message_ids(
        transaction,
        "SELECT id FROM collab_message
         WHERE state='leased' AND lease_until<=?1",
        [now],
    )?;
    for id in expired_leases {
        let message = select_message(transaction, &id)?;
        if message.attempt_count >= MAX_DELIVERY_ATTEMPTS {
            transaction.execute(
                "UPDATE collab_message SET state='dead_letter',lease_token_hash=NULL,
                 lease_until=NULL,last_error_code='max_attempts',updated_at=?1 WHERE id=?2",
                params![now, id],
            )?;
            insert_event(
                transaction,
                &message.team_id,
                "message",
                &id,
                "message_dead_lettered",
                ActorType::System,
                None,
                "{}",
                now,
            )?;
            mark_delivery_attention(transaction, &message, now, "max_attempts")?;
            summary.messages_dead_lettered += 1;
        } else {
            let backoff_ms = (1_000_i64
                .saturating_mul(1_i64 << message.attempt_count.saturating_sub(1).min(6)))
            .min(60_000);
            transaction.execute(
                "UPDATE collab_message SET state='queued',lease_token_hash=NULL,
                 lease_until=NULL,not_before=?1,updated_at=?2 WHERE id=?3",
                params![now + backoff_ms, now, id],
            )?;
            insert_event(
                transaction,
                &message.team_id,
                "message",
                &id,
                "message_queued",
                ActorType::System,
                None,
                &json!({"reason":"lease_expired","backoffMs":backoff_ms}).to_string(),
                now,
            )?;
            summary.leases_requeued += 1;
        }
    }
    Ok(summary)
}

fn record_report_required_edge(
    transaction: &Transaction<'_>,
    team_id: &str,
    member_id: &str,
    terminal_generation: i64,
    now: i64,
) -> CollabResult<()> {
    let task = transaction
        .query_row(
            &format!(
                "SELECT {TASK_COLUMNS} FROM collab_task
                 WHERE team_id=?1 AND assignee_member_id=?2 AND assignee_generation=?3
                   AND state IN ('accepted','running')
                 ORDER BY created_at,id LIMIT 1"
            ),
            params![team_id, member_id, terminal_generation],
            task_from_row,
        )
        .optional()?;
    let Some(task) = task else {
        return Ok(());
    };

    if task.report_reminder_count < 2 {
        let reminder = task.report_reminder_count + 1;
        let changed = transaction.execute(
            "UPDATE collab_task SET attention_state='report_required',
             attention_reason='missing_explicit_report',
             attention_since=COALESCE(attention_since,?1),
             report_reminder_count=?2,updated_at=?1,version=version+1
             WHERE id=?3 AND version=?4 AND state IN ('accepted','running')",
            params![now, reminder, task.id, task.version],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict(
                "report reminder lost compare-and-swap".into(),
            ));
        }
        insert_event(
            transaction,
            team_id,
            "task",
            &task.id,
            "task_report_required",
            ActorType::Broker,
            Some(member_id),
            &json!({"reminder": reminder}).to_string(),
            now,
        )?;
    } else if task.attention_reason.as_deref() != Some("report_reminders_exhausted") {
        let changed = transaction.execute(
            "UPDATE collab_task SET attention_state='report_required',
             attention_reason='report_reminders_exhausted',
             attention_since=COALESCE(attention_since,?1),updated_at=?1,version=version+1
             WHERE id=?2 AND version=?3 AND state IN ('accepted','running')",
            params![now, task.id, task.version],
        )?;
        if changed != 1 {
            return Err(CollaborationError::Conflict(
                "report attention escalation lost compare-and-swap".into(),
            ));
        }
        insert_event(
            transaction,
            team_id,
            "task",
            &task.id,
            "task_needs_attention",
            ActorType::Broker,
            Some(member_id),
            r#"{"reason":"report_reminders_exhausted","reminders":2}"#,
            now,
        )?;
    }
    Ok(())
}

fn mark_delivery_attention(
    transaction: &Transaction<'_>,
    message: &Message,
    now: i64,
    reason: &str,
) -> CollabResult<()> {
    let Some(task_id) = message.task_id.as_deref() else {
        return Ok(());
    };
    let attention = match message.kind {
        MessageKind::TaskAssignment | MessageKind::TaskReport => "delivery_failed",
        MessageKind::TaskCancel => "cancel_unconfirmed",
        _ => return Ok(()),
    };
    let changed = transaction.execute(
        "UPDATE collab_task SET attention_state=?1,attention_reason=?2,
         attention_since=COALESCE(attention_since,?3),updated_at=?3,version=version+1
         WHERE id=?4",
        params![attention, reason, now, task_id],
    )?;
    if changed == 1 {
        insert_event(
            transaction,
            &message.team_id,
            "task",
            task_id,
            "task_needs_attention",
            ActorType::System,
            None,
            &json!({"reason": reason}).to_string(),
            now,
        )?;
    }
    Ok(())
}

fn validate_nonempty(field: &str, value: &str) -> CollabResult<()> {
    if value.trim().is_empty() {
        return Err(CollaborationError::InvalidInput(format!(
            "{field} is required"
        )));
    }
    Ok(())
}

fn validate_alias(alias: &str) -> CollabResult<()> {
    let mut bytes = alias.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if alias.len() > 48
        || !valid_first
        || !bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        return Err(CollaborationError::InvalidInput(
            "alias must be 1-48 lowercase ASCII characters, start with a letter/digit, and contain only letters, digits, '-' or '_'".into(),
        ));
    }
    Ok(())
}

fn ensure_team_mutable(team: &Team) -> CollabResult<()> {
    if team.archived_at.is_some() {
        return Err(CollaborationError::InvalidState {
            entity: "team",
            state: "archived".into(),
        });
    }
    Ok(())
}

fn ensure_team_paused(team: &Team) -> CollabResult<()> {
    if team.enabled {
        return Err(CollaborationError::InvalidState {
            entity: "team",
            state: "enabled".into(),
        });
    }
    Ok(())
}

fn ensure_no_nonterminal_tasks(transaction: &Transaction<'_>, team_id: &str) -> CollabResult<()> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM collab_task WHERE team_id=?1
         AND state NOT IN ('reported_completed','reported_failed','cancelled')",
        [team_id],
        |row| row.get(0),
    )?;
    if count > 0 {
        return Err(CollaborationError::Conflict(
            "team has non-terminal tasks".into(),
        ));
    }
    Ok(())
}

fn ensure_enabled_team_and_member(team: &Team, member: &Member) -> CollabResult<()> {
    ensure_team_mutable(team)?;
    if !team.enabled || !member.enabled {
        return Err(CollaborationError::Suspended);
    }
    if team.provider != PROVIDER_GROK_BUILD {
        return Err(CollaborationError::Unauthorized("provider"));
    }
    Ok(())
}

fn ensure_global_enabled(transaction: &Transaction<'_>) -> CollabResult<()> {
    let value: String = transaction.query_row(
        "SELECT value FROM collab_meta WHERE key='global_enabled'",
        [],
        |row| row.get(0),
    )?;
    if value != "1" {
        return Err(CollaborationError::Suspended);
    }
    Ok(())
}

fn authenticate(
    transaction: &Transaction<'_>,
    caller: &CallerIdentity,
) -> CollabResult<AuthenticatedCaller> {
    ensure_global_enabled(transaction)?;
    let (runtime, token_hash): (Runtime, Option<String>) = transaction
        .query_row(
            &format!(
                "SELECT {RUNTIME_COLUMNS},token_hash FROM collab_runtime
                 WHERE member_id=?1 AND revoked_at IS NULL"
            ),
            [&caller.member_id],
            |row| Ok((runtime_from_row(row)?, row.get(21)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => CollaborationError::StaleGeneration,
            other => CollaborationError::Database(other),
        })?;
    if runtime.terminal_generation != caller.terminal_generation
        || runtime.token_epoch != caller.token_epoch
    {
        return Err(CollaborationError::StaleGeneration);
    }
    match token_hash {
        Some(expected) => {
            let actual = caller
                .bearer_secret
                .as_deref()
                .map(hash_secret)
                .ok_or(CollaborationError::Unauthorized("capability"))?;
            if !constant_time_eq(expected.as_bytes(), actual.as_bytes()) {
                return Err(CollaborationError::Unauthorized("capability"));
            }
        }
        None if caller.bearer_secret.is_some() => {
            return Err(CollaborationError::Unauthorized("auth_method"));
        }
        None => {}
    }
    let member = select_member(transaction, &caller.member_id)?;
    let team = select_team(transaction, &member.team_id)?;
    ensure_enabled_team_and_member(&team, &member)?;
    if runtime.routing_revision != team.routing_revision
        || runtime.attested_provider != PROVIDER_GROK_BUILD
        || runtime.attested_workspace_fingerprint != team.workspace_fingerprint
    {
        return Err(CollaborationError::Unauthorized("runtime_scope"));
    }
    Ok(AuthenticatedCaller {
        team,
        member,
        runtime,
    })
}

fn bump_routing_revision(
    transaction: &Transaction<'_>,
    team_id: &str,
    now: i64,
) -> CollabResult<()> {
    transaction.execute(
        "UPDATE collab_team SET routing_revision=routing_revision+1,updated_at=?1 WHERE id=?2",
        params![now, team_id],
    )?;
    // Any active runtime is scoped to the previous graph and must re-attest.
    transaction.execute(
        "UPDATE collab_runtime SET revoked_at=?1 WHERE member_id IN
         (SELECT id FROM collab_member WHERE team_id=?2) AND revoked_at IS NULL",
        params![now, team_id],
    )?;
    block_routing_revision(transaction, team_id, now)?;
    Ok(())
}

fn block_routing_revision(
    transaction: &Transaction<'_>,
    team_id: &str,
    now: i64,
) -> CollabResult<()> {
    let ids = query_message_ids(
        transaction,
        "SELECT id FROM collab_message
         WHERE team_id=?1 AND state IN ('queued','leased','suspended')",
        [team_id],
    )?;
    transaction.execute(
        "UPDATE collab_message
         SET state='blocked',blocked_at=?1,blocked_reason='stale_routing',
             resolution_policy='user_retry',lease_token_hash=NULL,lease_until=NULL,
             lease_epoch=lease_epoch+1,updated_at=?1
         WHERE team_id=?2 AND state IN ('queued','leased','suspended')",
        params![now, team_id],
    )?;
    for id in ids {
        insert_event(
            transaction,
            team_id,
            "message",
            &id,
            "message_blocked",
            ActorType::Broker,
            None,
            r#"{"reason":"stale_routing"}"#,
            now,
        )?;
    }
    Ok(())
}

fn block_team_messages(
    transaction: &Transaction<'_>,
    team_id: &str,
    now: i64,
    reason: &str,
    resolution_policy: &str,
) -> CollabResult<()> {
    let ids = query_message_ids(
        transaction,
        "SELECT id FROM collab_message
         WHERE team_id=?1 AND state IN ('queued','leased','suspended')",
        [team_id],
    )?;
    transaction.execute(
        "UPDATE collab_message SET state='blocked',blocked_at=?1,blocked_reason=?2,
         resolution_policy=?3,lease_token_hash=NULL,lease_until=NULL,
         lease_epoch=lease_epoch+1,updated_at=?1
         WHERE team_id=?4 AND state IN ('queued','leased','suspended')",
        params![now, reason, resolution_policy, team_id],
    )?;
    for id in ids {
        insert_event(
            transaction,
            team_id,
            "message",
            &id,
            "message_blocked",
            ActorType::Broker,
            None,
            &json!({"reason": reason}).to_string(),
            now,
        )?;
    }
    Ok(())
}

fn suspend_messages(
    transaction: &Transaction<'_>,
    team_id: Option<&str>,
    now: i64,
    reason: &str,
) -> CollabResult<()> {
    let ids = if let Some(team_id) = team_id {
        query_message_ids(
            transaction,
            "SELECT id FROM collab_message WHERE team_id=?1 AND state IN ('queued','leased')",
            [team_id],
        )?
    } else {
        query_message_ids(
            transaction,
            "SELECT id FROM collab_message WHERE state IN ('queued','leased')",
            [],
        )?
    };
    if let Some(team_id) = team_id {
        transaction.execute(
            "UPDATE collab_message
             SET state='suspended',paused_at=?1,lease_token_hash=NULL,lease_until=NULL,
                 lease_epoch=lease_epoch+1,updated_at=?1
             WHERE team_id=?2 AND state IN ('queued','leased')",
            params![now, team_id],
        )?;
        for id in ids {
            insert_event(
                transaction,
                team_id,
                "message",
                &id,
                "message_suspended",
                ActorType::Broker,
                None,
                &json!({"reason": reason}).to_string(),
                now,
            )?;
        }
    } else {
        transaction.execute(
            "UPDATE collab_message
             SET state='suspended',paused_at=?1,lease_token_hash=NULL,lease_until=NULL,
                 lease_epoch=lease_epoch+1,updated_at=?1
             WHERE state IN ('queued','leased')",
            [now],
        )?;
        for id in ids {
            let message = select_message(transaction, &id)?;
            insert_event(
                transaction,
                &message.team_id,
                "message",
                &id,
                "message_suspended",
                ActorType::Broker,
                None,
                &json!({"reason": reason}).to_string(),
                now,
            )?;
        }
    }
    Ok(())
}

fn resume_messages(transaction: &Transaction<'_>, team_id: &str, now: i64) -> CollabResult<()> {
    let ids = query_message_ids(
        transaction,
        "SELECT m.id FROM collab_message m
         JOIN collab_runtime r ON r.member_id=m.recipient_member_id
          AND r.terminal_generation=m.recipient_generation AND r.revoked_at IS NULL
         JOIN collab_team t ON t.id=m.team_id
         WHERE m.team_id=?1 AND m.state='suspended'
           AND m.routing_revision=t.routing_revision
           AND r.routing_revision=t.routing_revision",
        [team_id],
    )?;
    transaction.execute(
        "UPDATE collab_message SET state='queued',paused_at=NULL,updated_at=?1
         WHERE id IN (
           SELECT m.id FROM collab_message m
           JOIN collab_runtime r ON r.member_id=m.recipient_member_id
            AND r.terminal_generation=m.recipient_generation AND r.revoked_at IS NULL
           JOIN collab_team t ON t.id=m.team_id
           WHERE m.team_id=?2 AND m.state='suspended'
             AND m.routing_revision=t.routing_revision
             AND r.routing_revision=t.routing_revision
         )",
        params![now, team_id],
    )?;
    for id in ids {
        insert_event(
            transaction,
            team_id,
            "message",
            &id,
            "message_queued",
            ActorType::Broker,
            None,
            r#"{"resumed":true}"#,
            now,
        )?;
    }
    let blocked = query_message_ids(
        transaction,
        "SELECT id FROM collab_message WHERE team_id=?1 AND state='suspended'",
        [team_id],
    )?;
    transaction.execute(
        "UPDATE collab_message
         SET state='blocked',blocked_at=?1,blocked_reason='resume_revalidation_failed',
             resolution_policy='user_retry',updated_at=?1
         WHERE team_id=?2 AND state='suspended'",
        params![now, team_id],
    )?;
    for id in blocked {
        insert_event(
            transaction,
            team_id,
            "message",
            &id,
            "message_blocked",
            ActorType::Broker,
            None,
            r#"{"reason":"resume_revalidation_failed"}"#,
            now,
        )?;
    }
    Ok(())
}

fn block_generation(
    transaction: &Transaction<'_>,
    team_id: &str,
    member_id: &str,
    generation: i64,
    now: i64,
    reason: &str,
) -> CollabResult<()> {
    let ids = {
        let mut statement = transaction.prepare(
            "SELECT id FROM collab_message
             WHERE recipient_member_id=?1 AND recipient_generation=?2
               AND state IN ('queued','leased','suspended')",
        )?;
        let rows = statement
            .query_map(params![member_id, generation], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    transaction.execute(
        "UPDATE collab_message SET state='blocked',blocked_at=?1,blocked_reason=?2,
             resolution_policy='user_retry',lease_token_hash=NULL,lease_until=NULL,
             lease_epoch=lease_epoch+1,updated_at=?1
         WHERE recipient_member_id=?3 AND recipient_generation=?4
           AND state IN ('queued','leased','suspended')",
        params![now, reason, member_id, generation],
    )?;
    for id in ids {
        insert_event(
            transaction,
            team_id,
            "message",
            &id,
            "message_blocked",
            ActorType::Broker,
            None,
            &json!({"reason": reason}).to_string(),
            now,
        )?;
    }
    Ok(())
}

fn query_message_ids<P: rusqlite::Params>(
    transaction: &Transaction<'_>,
    sql: &str,
    params: P,
) -> CollabResult<Vec<String>> {
    let mut statement = transaction.prepare(sql)?;
    let rows = statement.query_map(params, |row| row.get::<_, String>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

pub(crate) fn hash_secret(value: &str) -> String {
    sha256_hex(value.as_bytes())
}

// Compact dependency-free SHA-256. Keeping the verifier here avoids storing
// raw lease/capability secrets and avoids widening Cargo dependencies.
#[allow(clippy::chunks_exact_to_as_chunks)]
fn sha256_hex(input: &[u8]) -> String {
    const INITIAL: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
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
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let mut state = INITIAL;
    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes(
                chunk[index * 4..index * 4 + 4]
                    .try_into()
                    .expect("four bytes"),
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
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
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
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        service: CollaborationService,
        team: Team,
        leader: Member,
        worker_a: Member,
        worker_b: Member,
        worker_c: Member,
        worker_a_binding: Binding,
        leader_caller: CallerIdentity,
        worker_a_caller: CallerIdentity,
        worker_b_caller: CallerIdentity,
        worker_c_caller: CallerIdentity,
    }

    fn setup() -> Harness {
        let service = CollaborationService::in_memory().expect("service");
        service.set_global_enabled(true).expect("global enable");
        let team = service
            .create_team(NewTeam {
                name: "Core team".into(),
                workspace_fingerprint: "workspace-a".into(),
                enabled: false,
            })
            .expect("team");
        let leader = service
            .add_member(NewMember {
                team_id: team.id.clone(),
                alias: "main".into(),
                display_name: "Main".into(),
                avatar_id: "leader-1".into(),
                role: Role::Leader,
                enabled: true,
            })
            .expect("leader");
        let worker_a = service
            .add_member(NewMember {
                team_id: team.id.clone(),
                alias: "worker-a".into(),
                display_name: "Worker A".into(),
                avatar_id: "worker-1".into(),
                role: Role::Worker,
                enabled: true,
            })
            .expect("worker a");
        let worker_b = service
            .add_member(NewMember {
                team_id: team.id.clone(),
                alias: "worker-b".into(),
                display_name: "Worker B".into(),
                avatar_id: "worker-2".into(),
                role: Role::Worker,
                enabled: true,
            })
            .expect("worker b");
        let worker_c = service
            .add_member(NewMember {
                team_id: team.id.clone(),
                alias: "worker-c".into(),
                display_name: "Worker C".into(),
                avatar_id: "worker-3".into(),
                role: Role::Worker,
                enabled: true,
            })
            .expect("worker c");
        let leader_binding = service
            .bind_member(NewBinding {
                member_id: leader.id.clone(),
                grok_session_id: "grok-main".into(),
            })
            .expect("leader binding");
        let worker_a_binding = service
            .bind_member(NewBinding {
                member_id: worker_a.id.clone(),
                grok_session_id: "grok-worker-a".into(),
            })
            .expect("worker a binding");
        let worker_b_binding = service
            .bind_member(NewBinding {
                member_id: worker_b.id.clone(),
                grok_session_id: "grok-worker-b".into(),
            })
            .expect("worker b binding");
        let worker_c_binding = service
            .bind_member(NewBinding {
                member_id: worker_c.id.clone(),
                grok_session_id: "grok-worker-c".into(),
            })
            .expect("worker c binding");
        service.install_v1_acl(&team.id).expect("ACL");
        service
            .set_team_enabled(&team.id, true)
            .expect("team enable");
        register(&service, &leader, &leader_binding, "leader-secret", 1);
        register(&service, &worker_a, &worker_a_binding, "worker-a-secret", 1);
        register(&service, &worker_b, &worker_b_binding, "worker-b-secret", 1);
        register(&service, &worker_c, &worker_c_binding, "worker-c-secret", 1);
        Harness {
            team: service.store.team(&team.id).expect("current team"),
            service,
            leader: leader.clone(),
            worker_a: worker_a.clone(),
            worker_b: worker_b.clone(),
            worker_c: worker_c.clone(),
            worker_a_binding,
            leader_caller: caller(&leader, "leader-secret", 1),
            worker_a_caller: caller(&worker_a, "worker-a-secret", 1),
            worker_b_caller: caller(&worker_b, "worker-b-secret", 1),
            worker_c_caller: caller(&worker_c, "worker-c-secret", 1),
        }
    }

    fn register(
        service: &CollaborationService,
        member: &Member,
        binding: &Binding,
        secret: &str,
        generation: i64,
    ) {
        service
            .register_runtime(NewRuntime {
                member_id: member.id.clone(),
                binding_id: binding.id.clone(),
                terminal_session_id: format!("terminal-{}", member.alias),
                terminal_generation: generation,
                observed_grok_session_id: binding.grok_session_id.clone(),
                process_id: Some(1234),
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(secret.into()),
                token_epoch: 1,
                attested_workspace_fingerprint: "workspace-a".into(),
                grok_version: "test".into(),
                helper_protocol_version: "1".into(),
                capability_probe_result: "ok".into(),
                listener_state: ListenerState::Ready,
                runtime_state: RuntimeState::Idle,
            })
            .expect("runtime");
    }

    fn caller(member: &Member, secret: &str, generation: i64) -> CallerIdentity {
        CallerIdentity {
            member_id: member.id.clone(),
            terminal_generation: generation,
            token_epoch: 1,
            bearer_secret: Some(secret.into()),
        }
    }

    fn message_request(request_id: &str, payload: &str) -> SendMessageRequest {
        SendMessageRequest {
            recipient_alias: "worker-a".into(),
            kind: MessageKind::Message,
            task_id: None,
            reply_to_message_id: None,
            payload_text: payload.into(),
            request_id: request_id.into(),
            retry_of_message_id: None,
            not_before: None,
            expires_at: None,
        }
    }

    fn assign_request(request_id: &str) -> AssignTaskRequest {
        assign_request_to("worker-a", request_id)
    }

    fn assign_request_to(assignee_alias: &str, request_id: &str) -> AssignTaskRequest {
        AssignTaskRequest {
            assignee_alias: assignee_alias.into(),
            title: format!("Task {request_id}"),
            instructions: "Inspect the repository and report explicitly.".into(),
            optional_scope_json: Some(r#"{"paths":["src"]}"#.into()),
            request_id: request_id.into(),
            expires_at: None,
        }
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn stable_idempotency_claim_and_fixed_acl_are_enforced() {
        let harness = setup();
        let first = harness
            .service
            .send_message(&harness.leader_caller, message_request("send-1", "hello"))
            .expect("first send");
        let repeated = harness
            .service
            .send_message(&harness.leader_caller, message_request("send-1", "hello"))
            .expect("idempotent retry");
        assert_eq!(first.id, repeated.id);
        assert_eq!(
            harness
                .service
                .store()
                .events_after(0, 10_000)
                .expect("idempotency events")
                .into_iter()
                .filter(|event| {
                    event.aggregate_type == "message"
                        && event.aggregate_id == first.id
                        && event.event_type == "message_queued"
                })
                .count(),
            1,
            "an idempotent retry must not repeat the domain transition"
        );
        assert!(matches!(
            harness.service.send_message(
                &harness.leader_caller,
                message_request("send-1", "different")
            ),
            Err(CollaborationError::Conflict(_))
        ));

        let denied = SendMessageRequest {
            recipient_alias: "worker-b".into(),
            kind: MessageKind::Message,
            task_id: None,
            reply_to_message_id: None,
            payload_text: "peer message".into(),
            request_id: "worker-peer".into(),
            retry_of_message_id: None,
            not_before: None,
            expires_at: None,
        };
        assert!(matches!(
            harness
                .service
                .send_message(&harness.worker_a_caller, denied),
            Err(CollaborationError::Unauthorized("acl"))
        ));
        let claim = harness
            .service
            .authenticate_claim("worker-a", 1, "worker-a-secret")
            .expect("claim");
        assert_eq!(claim.member_id, harness.worker_a.id);
        assert!(matches!(
            harness
                .service
                .authenticate_claim("worker-a", 1, "wrong-secret"),
            Err(CollaborationError::Unauthorized("invalid_claim"))
        ));
        assert!(matches!(
            harness
                .service
                .authenticate_claim("worker-b", 1, "worker-a-secret"),
            Err(CollaborationError::Unauthorized("invalid_claim"))
        ));
        assert!(harness.service.touch_runtime_heartbeat(&claim).is_ok());
        let scope = harness
            .service
            .allowed(&harness.worker_b_caller)
            .expect("allowed scope");
        assert_eq!(scope.role, Role::Worker);
        assert!(harness
            .service
            .tasks_pending(&harness.worker_b_caller)
            .expect("pending tasks")
            .is_empty());
        assert_eq!(
            harness
                .service
                .list_team_configurations()
                .expect("team list")
                .len(),
            1
        );
        let resolved = harness
            .service
            .resolve_enabled_binding("grok-worker-a", "workspace-a")
            .expect("binding lookup")
            .expect("binding");
        assert_eq!(resolved.member.id, harness.worker_a.id);
        assert!(resolved.active_runtime.is_some());
        assert!(harness
            .service
            .active_runtime_for_grok_session("grok-worker-b")
            .expect("collision lookup")
            .is_some());
        assert!(harness
            .service
            .revoke_terminal_runtime("terminal-worker-b", 1, "test_exit")
            .expect("revoke"));
        assert!(!harness
            .service
            .revoke_terminal_runtime("terminal-worker-b", 1, "late_duplicate")
            .expect("idempotent revoke"));
    }

    #[test]
    fn busy_waiting_user_and_offline_targets_only_queue_without_changing_execution_state() {
        let harness = setup();
        harness
            .service
            .update_runtime_state(
                &harness.worker_a_caller,
                ListenerState::Ready,
                RuntimeState::Busy,
            )
            .expect("worker busy");
        let busy_task = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("queued-while-busy"))
            .expect("queue task while busy");
        assert_eq!(busy_task.task.state, TaskState::Assigned);
        assert_eq!(busy_task.message.state, MessageState::Queued);
        assert_eq!(
            harness
                .service
                .active_runtime_for_grok_session("grok-worker-a")
                .expect("busy runtime")
                .expect("active busy runtime")
                .runtime_state,
            RuntimeState::Busy
        );

        harness
            .service
            .update_runtime_state(
                &harness.worker_a_caller,
                ListenerState::Ready,
                RuntimeState::WaitingUser,
            )
            .expect("worker waiting for user");
        let waiting_task = harness
            .service
            .assign_task(
                &harness.leader_caller,
                assign_request("queued-while-waiting-user"),
            )
            .expect("queue task while waiting for user");
        assert_eq!(waiting_task.task.state, TaskState::Assigned);
        assert_eq!(waiting_task.message.state, MessageState::Queued);
        assert_eq!(
            harness
                .service
                .active_runtime_for_grok_session("grok-worker-a")
                .expect("waiting runtime")
                .expect("active waiting runtime")
                .runtime_state,
            RuntimeState::WaitingUser
        );

        harness
            .service
            .update_runtime_state(
                &harness.worker_a_caller,
                ListenerState::Offline,
                RuntimeState::Unknown,
            )
            .expect("listener offline while generation remains active");
        let offline_message = harness
            .service
            .send_message(
                &harness.leader_caller,
                message_request("queued-while-offline", "durable inbox only"),
            )
            .expect("queue while listener offline");
        assert_eq!(offline_message.state, MessageState::Queued);
        let offline_runtime = harness
            .service
            .active_runtime_for_grok_session("grok-worker-a")
            .expect("offline runtime query")
            .expect("offline listener still has active generation");
        assert_eq!(offline_runtime.listener_state, ListenerState::Offline);
        assert_eq!(offline_runtime.runtime_state, RuntimeState::Unknown);
        assert_eq!(
            harness
                .service
                .tasks_pending(&harness.worker_a_caller)
                .expect("assigned tasks remain pending")
                .len(),
            2
        );
        assert_eq!(
            harness
                .service
                .pending_count(&harness.worker_a_caller)
                .expect("all queued envelopes"),
            3
        );
    }

    #[test]
    fn busy_to_idle_emits_two_durable_report_controls_then_needs_attention_until_report() {
        let harness = setup();
        let assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("report-reminders"))
            .expect("assign reminder task");
        let lease = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("lease reminder task")
            .expect("assignment envelope");
        harness
            .service
            .accept_task(
                &harness.worker_a_caller,
                AcceptTaskRequest {
                    task_id: assignment.task.id.clone(),
                    assignment_message_id: lease.message.id,
                    lease_epoch: lease.lease_epoch,
                    lease_token: lease.lease_token,
                },
            )
            .expect("accept reminder task");
        harness
            .service
            .start_task(&harness.worker_a_caller, &assignment.task.id)
            .expect("start reminder task");

        let observe = |state| {
            harness
                .service
                .observe_ready_runtime_state("terminal-worker-a", 1, state)
                .expect("observe worker activity")
        };
        assert!(observe(RuntimeState::Busy));
        assert!(observe(RuntimeState::Idle));
        let first_task = harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("first reminder task");
        assert_eq!(first_task.state, TaskState::Running);
        assert_eq!(first_task.attention_state, AttentionState::ReportRequired);
        assert_eq!(first_task.report_reminder_count, 1);
        assert_eq!(
            first_task.attention_reason.as_deref(),
            Some("missing_explicit_report")
        );
        let first_wake = harness
            .service
            .peek_next_control_wake(&harness.worker_a_caller)
            .expect("first control query")
            .expect("first report control");
        assert_eq!(first_wake.task_id, assignment.task.id);
        assert_eq!(first_wake.kind, "report_required");
        assert_eq!(
            harness
                .service
                .peek_next_control_wake(&harness.worker_a_caller)
                .expect("restart-safe control query")
                .expect("same durable control")
                .id,
            first_wake.id
        );

        assert!(!observe(RuntimeState::Idle));
        assert_eq!(
            harness
                .service
                .store()
                .task(&assignment.task.id)
                .expect("duplicate idle task")
                .report_reminder_count,
            1
        );

        assert!(observe(RuntimeState::Busy));
        assert!(observe(RuntimeState::Idle));
        let second_task = harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("second reminder task");
        assert_eq!(second_task.state, TaskState::Running);
        assert_eq!(second_task.report_reminder_count, 2);
        let second_wake = harness
            .service
            .peek_next_control_wake(&harness.worker_a_caller)
            .expect("second control query")
            .expect("second report control");
        assert_ne!(second_wake.id, first_wake.id);
        assert_eq!(second_wake.task_id, assignment.task.id);

        assert!(observe(RuntimeState::Busy));
        assert!(observe(RuntimeState::Idle));
        let exhausted = harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("needs-attention task");
        assert_eq!(exhausted.state, TaskState::Running);
        assert_eq!(exhausted.attention_state, AttentionState::ReportRequired);
        assert_eq!(exhausted.report_reminder_count, 2);
        assert_eq!(
            exhausted.attention_reason.as_deref(),
            Some("report_reminders_exhausted")
        );
        assert!(harness
            .service
            .peek_next_control_wake(&harness.worker_a_caller)
            .expect("exhausted control query")
            .is_none());
        assert!(!observe(RuntimeState::Idle));

        let events = harness
            .service
            .store()
            .events_after(0, 10_000)
            .expect("reminder events");
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.aggregate_id == assignment.task.id
                        && event.event_type == "task_report_required"
                })
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.aggregate_id == assignment.task.id
                        && event.event_type == "task_needs_attention"
                        && event.actor_type == ActorType::Broker
                })
                .count(),
            1
        );

        let reported = harness
            .service
            .report_task(
                &harness.worker_a_caller,
                ReportTaskRequest {
                    task_id: assignment.task.id.clone(),
                    status: ReportStatus::Completed,
                    payload_text: "explicit report after reminders".into(),
                    request_id: "report-after-reminders".into(),
                },
            )
            .expect("explicit report");
        assert_eq!(reported.task.state, TaskState::ReportedCompleted);
        assert_eq!(reported.task.attention_state, AttentionState::None);
        assert!(harness
            .service
            .peek_next_control_wake(&harness.worker_a_caller)
            .expect("reported control query")
            .is_none());
        assert!(observe(RuntimeState::Busy));
        assert!(observe(RuntimeState::Idle));
        assert!(harness
            .service
            .peek_next_control_wake(&harness.worker_a_caller)
            .expect("terminal task control query")
            .is_none());
    }

    #[test]
    fn payload_round_trip_preserves_unicode_markdown_and_ansi_and_rejects_oversize() {
        let harness = setup();
        let payload = "中文第一行\n'quoted' \"double\" **Markdown** `code`\n\u{1b}[31mred\u{1b}[0m";
        let message = harness
            .service
            .send_message(
                &harness.leader_caller,
                message_request("payload-round-trip", payload),
            )
            .expect("structured payload");
        assert_eq!(message.payload_text, payload);
        let leased = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("payload lease")
            .expect("payload message");
        assert_eq!(leased.message.payload_text, payload);
        harness
            .service
            .ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: leased.message.id,
                    lease_epoch: leased.lease_epoch,
                    lease_token: leased.lease_token,
                },
            )
            .expect("payload ACK");

        let oversized = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        assert!(matches!(
            harness.service.send_message(
                &harness.leader_caller,
                message_request("payload-too-large", &oversized),
            ),
            Err(CollaborationError::Capacity("payload_64_kib"))
        ));
        assert!(matches!(
            harness.service.send_message(
                &harness.leader_caller,
                message_request("payload-nul", "prefix\0suffix"),
            ),
            Err(CollaborationError::InvalidInput(_))
        ));
        assert_eq!(
            harness
                .service
                .pending_count(&harness.worker_a_caller)
                .expect("rejected payloads create no message"),
            0
        );
    }

    #[test]
    fn lease_epoch_winning_ack_and_atomic_task_accept_hold() {
        let harness = setup();
        harness
            .service
            .send_message(&harness.leader_caller, message_request("lease-1", "one"))
            .expect("send");
        let lease = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: 10,
                },
            )
            .expect("lease")
            .expect("message");
        assert!(matches!(
            harness.service.ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: lease.message.id.clone(),
                    lease_epoch: lease.lease_epoch,
                    lease_token: "wrong".into(),
                }
            ),
            Err(CollaborationError::Unauthorized("lease_token"))
        ));
        let ack = AckMessageRequest {
            message_id: lease.message.id.clone(),
            lease_epoch: lease.lease_epoch,
            lease_token: lease.lease_token.clone(),
        };
        harness
            .service
            .ack_message(&harness.worker_a_caller, ack.clone())
            .expect("ack");
        harness
            .service
            .ack_message(&harness.worker_a_caller, ack)
            .expect("winning ACK retry");

        harness
            .service
            .send_message(&harness.leader_caller, message_request("lease-2", "two"))
            .expect("send two");
        let base = now_ms();
        let old = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: base,
                    lease_duration_ms: 10,
                },
            )
            .expect("old lease")
            .expect("old message");
        harness.service.recover(base + 11).expect("recover");
        let renewed = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: base + 1_012,
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("renew lease")
            .expect("renewed message");
        assert!(renewed.lease_epoch > old.lease_epoch);
        assert!(matches!(
            harness.service.ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: old.message.id,
                    lease_epoch: old.lease_epoch,
                    lease_token: old.lease_token,
                }
            ),
            Err(CollaborationError::Conflict(_))
        ));
        harness
            .service
            .ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: renewed.message.id,
                    lease_epoch: renewed.lease_epoch,
                    lease_token: renewed.lease_token,
                },
            )
            .expect("renewed ACK");

        let assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("task-accept"))
            .expect("assignment");
        let task_lease = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("assignment lease")
            .expect("assignment message");
        assert_eq!(task_lease.message.id, assignment.message.id);
        assert!(matches!(
            harness.service.ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: task_lease.message.id.clone(),
                    lease_epoch: task_lease.lease_epoch,
                    lease_token: task_lease.lease_token.clone(),
                }
            ),
            Err(CollaborationError::InvalidInput(_))
        ));
        let accepted = harness
            .service
            .accept_task(
                &harness.worker_a_caller,
                AcceptTaskRequest {
                    task_id: assignment.task.id,
                    assignment_message_id: task_lease.message.id.clone(),
                    lease_epoch: task_lease.lease_epoch,
                    lease_token: task_lease.lease_token,
                },
            )
            .expect("atomic accept");
        assert_eq!(accepted.state, TaskState::Accepted);
        assert_eq!(
            harness
                .service
                .store()
                .message(&task_lease.message.id)
                .expect("assignment state")
                .state,
            MessageState::Acknowledged
        );
    }

    #[test]
    fn recovery_requeues_an_unacked_delivery_with_the_same_id_and_rejects_the_stale_ack() {
        let harness = setup();
        let sent = harness
            .service
            .send_message(
                &harness.leader_caller,
                message_request("receive-before-ack-crash", "present at least once"),
            )
            .expect("send recovery message");
        let first_now = now_ms();
        let first = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: first_now,
                    lease_duration_ms: 10,
                },
            )
            .expect("first lease")
            .expect("first presentation");
        assert_eq!(first.message.id, sent.id);

        let recovery = harness
            .service
            .recover(first_now + 11)
            .expect("recover lost ACK window");
        assert_eq!(recovery.leases_requeued, 1);
        assert_eq!(
            harness
                .service
                .store()
                .message(&sent.id)
                .expect("requeued message")
                .state,
            MessageState::Queued
        );
        assert!(matches!(
            harness.service.ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: sent.id.clone(),
                    lease_epoch: first.lease_epoch,
                    lease_token: first.lease_token,
                },
            ),
            Err(CollaborationError::Conflict(_))
        ));

        let second = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: first_now + 100_000,
                    lease_duration_ms: 10,
                },
            )
            .expect("second lease")
            .expect("same message is presented again");
        assert_eq!(second.message.id, sent.id);
        assert!(second.lease_epoch > first.lease_epoch);
        harness
            .service
            .ack_message(
                &harness.worker_a_caller,
                AckMessageRequest {
                    message_id: sent.id,
                    lease_epoch: second.lease_epoch,
                    lease_token: second.lease_token,
                },
            )
            .expect("winning recovery ACK");
    }

    #[test]
    fn recovery_dead_letters_after_bounded_attempts_without_falsifying_task_result() {
        let harness = setup();
        let assignment = harness
            .service
            .assign_task(
                &harness.leader_caller,
                assign_request("bounded-delivery-attempts"),
            )
            .expect("assign delivery-failure task");
        let mut clock = now_ms();
        for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
            let lease = harness
                .service
                .lease_next(
                    &harness.worker_a_caller,
                    LeaseRequest {
                        now: clock,
                        lease_duration_ms: 1,
                    },
                )
                .expect("delivery attempt")
                .expect("assignment presentation");
            assert_eq!(lease.message.id, assignment.message.id);
            assert_eq!(lease.message.attempt_count, attempt);
            let summary = harness
                .service
                .recover(clock + 2)
                .expect("recover expired lease");
            if attempt < MAX_DELIVERY_ATTEMPTS {
                assert_eq!(summary.leases_requeued, 1);
                assert_eq!(summary.messages_dead_lettered, 0);
                clock += 100_000;
            } else {
                assert_eq!(summary.leases_requeued, 0);
                assert_eq!(summary.messages_dead_lettered, 1);
            }
        }

        let dead_letter = harness
            .service
            .store()
            .message(&assignment.message.id)
            .expect("dead-letter assignment");
        assert_eq!(dead_letter.state, MessageState::DeadLetter);
        assert_eq!(dead_letter.last_error_code.as_deref(), Some("max_attempts"));
        let task = harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("task after delivery failure");
        assert_eq!(task.state, TaskState::Assigned);
        assert_eq!(task.attention_state, AttentionState::DeliveryFailed);
        assert_eq!(task.attention_reason.as_deref(), Some("max_attempts"));
        assert!(task.terminal_report_message_id.is_none());
    }

    #[test]
    fn report_ack_and_cooperative_cancel_use_domain_transactions() {
        let harness = setup();
        let assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("task-report"))
            .expect("assign");
        let leased = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("lease")
            .expect("message");
        harness
            .service
            .accept_task(
                &harness.worker_a_caller,
                AcceptTaskRequest {
                    task_id: assignment.task.id.clone(),
                    assignment_message_id: leased.message.id,
                    lease_epoch: leased.lease_epoch,
                    lease_token: leased.lease_token,
                },
            )
            .expect("accept");
        harness
            .service
            .start_task(&harness.worker_a_caller, &assignment.task.id)
            .expect("start");
        let report = harness
            .service
            .report_task(
                &harness.worker_a_caller,
                ReportTaskRequest {
                    task_id: assignment.task.id.clone(),
                    status: ReportStatus::Completed,
                    payload_text: "done with evidence".into(),
                    request_id: "report-1".into(),
                },
            )
            .expect("report");
        assert_eq!(report.task.state, TaskState::ReportedCompleted);
        let report_lease = harness
            .service
            .lease_next(
                &harness.leader_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("leader lease")
            .expect("report message");
        assert!(matches!(
            harness.service.ack_message(
                &harness.leader_caller,
                AckMessageRequest {
                    message_id: report_lease.message.id.clone(),
                    lease_epoch: report_lease.lease_epoch,
                    lease_token: report_lease.lease_token.clone(),
                }
            ),
            Err(CollaborationError::InvalidInput(_))
        ));
        harness
            .service
            .ack_report(
                &harness.leader_caller,
                ReportAckRequest {
                    task_id: assignment.task.id,
                    report_message_id: report_lease.message.id,
                    lease_epoch: report_lease.lease_epoch,
                    lease_token: report_lease.lease_token,
                },
            )
            .expect("report ACK");

        let cancel_assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("task-cancel"))
            .expect("cancel assignment");
        let assignment_lease = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("assignment lease")
            .expect("assignment");
        harness
            .service
            .accept_task(
                &harness.worker_a_caller,
                AcceptTaskRequest {
                    task_id: cancel_assignment.task.id.clone(),
                    assignment_message_id: assignment_lease.message.id,
                    lease_epoch: assignment_lease.lease_epoch,
                    lease_token: assignment_lease.lease_token,
                },
            )
            .expect("accept cancel task");
        harness
            .service
            .start_task(&harness.worker_a_caller, &cancel_assignment.task.id)
            .expect("start cancel task");
        harness
            .service
            .cancel_task(
                &harness.leader_caller,
                CancelTaskRequest {
                    task_id: cancel_assignment.task.id.clone(),
                    reason: "stop safely".into(),
                    request_id: "cancel-1".into(),
                },
            )
            .expect("cancel request");
        let cancel_lease = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("cancel lease")
            .expect("cancel message");
        let cancelled = harness
            .service
            .ack_cancel(
                &harness.worker_a_caller,
                CancelAckRequest {
                    task_id: cancel_assignment.task.id,
                    cancel_message_id: cancel_lease.message.id,
                    lease_epoch: cancel_lease.lease_epoch,
                    lease_token: cancel_lease.lease_token,
                    payload_text: "stopped".into(),
                    request_id: "cancel-ack-1".into(),
                },
            )
            .expect("cancel ACK");
        assert_eq!(cancelled.task.state, TaskState::Cancelled);
        assert_eq!(cancelled.message.kind, MessageKind::TaskCancelAck);
    }

    #[test]
    fn one_leader_three_workers_preserve_concurrent_reports_and_task_correlation() {
        let harness = setup();
        let workers = [
            ("worker-a", &harness.worker_a_caller),
            ("worker-b", &harness.worker_b_caller),
            ("worker-c", &harness.worker_c_caller),
        ];
        let mut running = Vec::new();
        for (index, (alias, worker)) in workers.iter().enumerate() {
            let assignment = harness
                .service
                .assign_task(
                    &harness.leader_caller,
                    assign_request_to(alias, &format!("concurrent-assign-{index}")),
                )
                .expect("assign concurrent worker");
            let lease = harness
                .service
                .lease_next(
                    worker,
                    LeaseRequest {
                        now: now_ms(),
                        lease_duration_ms: DEFAULT_LEASE_MS,
                    },
                )
                .expect("lease concurrent assignment")
                .expect("assignment message");
            harness
                .service
                .accept_task(
                    worker,
                    AcceptTaskRequest {
                        task_id: assignment.task.id.clone(),
                        assignment_message_id: lease.message.id,
                        lease_epoch: lease.lease_epoch,
                        lease_token: lease.lease_token,
                    },
                )
                .expect("accept concurrent assignment");
            harness
                .service
                .start_task(worker, &assignment.task.id)
                .expect("start concurrent task");
            running.push((index, (*worker).clone(), assignment.task.id));
        }

        let reports = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (index, caller, task_id) in running {
                let service = &harness.service;
                handles.push(scope.spawn(move || {
                    service.report_task(
                        &caller,
                        ReportTaskRequest {
                            task_id,
                            status: ReportStatus::Completed,
                            payload_text: format!("worker {index} completed with evidence"),
                            request_id: format!("concurrent-report-{index}"),
                        },
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .expect("report thread")
                        .expect("concurrent report")
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(reports.len(), 3);
        let mut expected = reports
            .iter()
            .map(|outcome| (outcome.task.id.clone(), outcome.message.id.clone()))
            .collect::<Vec<_>>();
        expected.sort();
        expected.dedup();
        assert_eq!(expected.len(), 3);
        assert!(reports.iter().all(|outcome| {
            outcome.task.state == TaskState::ReportedCompleted
                && outcome.message.kind == MessageKind::TaskReport
                && outcome.message.task_id.as_deref() == Some(outcome.task.id.as_str())
        }));

        let mut received = Vec::new();
        for _ in 0..3 {
            let lease = harness
                .service
                .lease_next(
                    &harness.leader_caller,
                    LeaseRequest {
                        now: now_ms(),
                        lease_duration_ms: DEFAULT_LEASE_MS,
                    },
                )
                .expect("lease report")
                .expect("three reports must remain queued");
            let task_id = lease.message.task_id.clone().expect("report task ID");
            harness
                .service
                .ack_report(
                    &harness.leader_caller,
                    ReportAckRequest {
                        task_id: task_id.clone(),
                        report_message_id: lease.message.id.clone(),
                        lease_epoch: lease.lease_epoch,
                        lease_token: lease.lease_token,
                    },
                )
                .expect("ack correlated report");
            received.push((task_id, lease.message.id));
        }
        received.sort();
        assert_eq!(received, expected);
        assert!(harness
            .service
            .lease_next(
                &harness.leader_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("empty report inbox")
            .is_none());
    }

    #[test]
    fn running_task_runtime_exit_is_uncertain_and_new_generation_cannot_take_it_over() {
        let harness = setup();
        let assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("crash-running-task"))
            .expect("assign crash task");
        let lease = harness
            .service
            .lease_next(
                &harness.worker_a_caller,
                LeaseRequest {
                    now: now_ms(),
                    lease_duration_ms: DEFAULT_LEASE_MS,
                },
            )
            .expect("lease crash task")
            .expect("assignment envelope");
        harness
            .service
            .accept_task(
                &harness.worker_a_caller,
                AcceptTaskRequest {
                    task_id: assignment.task.id.clone(),
                    assignment_message_id: lease.message.id,
                    lease_epoch: lease.lease_epoch,
                    lease_token: lease.lease_token,
                },
            )
            .expect("accept crash task");
        harness
            .service
            .start_task(&harness.worker_a_caller, &assignment.task.id)
            .expect("start crash task");
        let old_target_message = harness
            .service
            .send_message(
                &harness.leader_caller,
                message_request(
                    "old-generation-message",
                    "must remain bound to generation one",
                ),
            )
            .expect("queue old-generation message");
        assert_eq!(old_target_message.recipient_generation, 1);

        assert!(harness
            .service
            .revoke_terminal_runtime("terminal-worker-a", 1, "simulated_worker_crash")
            .expect("revoke crashed generation"));
        let uncertain = harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("uncertain task");
        assert_eq!(uncertain.state, TaskState::Running);
        assert_eq!(
            uncertain.attention_state,
            AttentionState::UncertainExecution
        );
        assert_eq!(
            uncertain.attention_reason.as_deref(),
            Some("simulated_worker_crash")
        );
        let blocked_old_target = harness
            .service
            .store()
            .message(&old_target_message.id)
            .expect("blocked old target");
        assert_eq!(blocked_old_target.state, MessageState::Blocked);
        assert_eq!(
            blocked_old_target.blocked_reason.as_deref(),
            Some("stale_target")
        );
        assert_eq!(
            blocked_old_target.resolution_policy.as_deref(),
            Some("user_retry")
        );
        assert_eq!(blocked_old_target.recipient_generation, 1);

        register(
            &harness.service,
            &harness.worker_a,
            &harness.worker_a_binding,
            "worker-a-secret-generation-2",
            2,
        );
        let replacement = caller(&harness.worker_a, "worker-a-secret-generation-2", 2);
        assert!(matches!(
            harness
                .service
                .start_task(&replacement, &assignment.task.id),
            Err(CollaborationError::StaleGeneration)
        ));
        assert!(matches!(
            harness.service.report_task(
                &replacement,
                ReportTaskRequest {
                    task_id: assignment.task.id.clone(),
                    status: ReportStatus::Completed,
                    payload_text: "new process must not guess completion".into(),
                    request_id: "crash-task-forbidden-report".into(),
                },
            ),
            Err(CollaborationError::StaleGeneration)
        ));
        assert!(harness
            .service
            .tasks_pending(&replacement)
            .expect("replacement pending tasks")
            .is_empty());
        let still_uncertain = harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("task remains uncertain");
        assert_eq!(still_uncertain.state, TaskState::Running);
        assert_eq!(
            still_uncertain.attention_state,
            AttentionState::UncertainExecution
        );
        assert!(still_uncertain.terminal_report_message_id.is_none());
    }

    #[test]
    fn task_owner_and_cross_team_targets_are_rejected_without_side_effects_or_existence_leak() {
        let harness = setup();
        let assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("owned-by-worker-a"))
            .expect("assign worker-a task");
        assert!(matches!(
            harness.service.send_message(
                &harness.worker_b_caller,
                SendMessageRequest {
                    recipient_alias: harness.leader.alias.clone(),
                    kind: MessageKind::Progress,
                    task_id: Some(assignment.task.id.clone()),
                    reply_to_message_id: None,
                    payload_text: "worker-b must not spoof worker-a progress".into(),
                    request_id: "spoof-progress".into(),
                    retry_of_message_id: None,
                    not_before: None,
                    expires_at: None,
                },
            ),
            Err(CollaborationError::Unauthorized("task_owner"))
        ));
        assert!(matches!(
            harness.service.report_task(
                &harness.worker_b_caller,
                ReportTaskRequest {
                    task_id: assignment.task.id.clone(),
                    status: ReportStatus::Completed,
                    payload_text: "worker-b must not spoof worker-a report".into(),
                    request_id: "spoof-report".into(),
                },
            ),
            Err(CollaborationError::Unauthorized("task_owner"))
        ));
        assert_eq!(
            harness
                .service
                .pending_count(&harness.leader_caller)
                .expect("leader inbox unchanged"),
            0
        );
        assert!(harness
            .service
            .store()
            .task(&assignment.task.id)
            .expect("owned task unchanged")
            .terminal_report_message_id
            .is_none());

        let other_team = harness
            .service
            .create_team(NewTeam {
                name: "Other team".into(),
                workspace_fingerprint: "workspace-other".into(),
                enabled: false,
            })
            .expect("other team");
        harness
            .service
            .add_member(NewMember {
                team_id: other_team.id,
                alias: "cross-team-only".into(),
                display_name: "Cross Team Only".into(),
                avatar_id: "other-1".into(),
                role: Role::Leader,
                enabled: true,
            })
            .expect("cross-team member");
        for (request_id, alias) in [
            ("cross-team-target", "cross-team-only"),
            ("unknown-target", "does-not-exist"),
        ] {
            assert!(matches!(
                harness.service.send_message(
                    &harness.leader_caller,
                    SendMessageRequest {
                        recipient_alias: alias.into(),
                        kind: MessageKind::Message,
                        task_id: None,
                        reply_to_message_id: None,
                        payload_text: "must not disclose target existence".into(),
                        request_id: request_id.into(),
                        retry_of_message_id: None,
                        not_before: None,
                        expires_at: None,
                    },
                ),
                Err(CollaborationError::Unauthorized("target_not_allowed"))
            ));
        }
        assert_eq!(
            harness
                .service
                .pending_count(&harness.worker_a_caller)
                .expect("only original assignment remains"),
            1
        );
    }

    #[test]
    fn direct_cancel_records_request_idempotency_and_rejects_semantic_reuse() {
        let harness = setup();
        let assignment = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("direct-cancel-1"))
            .expect("assign first task");
        let request = CancelTaskRequest {
            task_id: assignment.task.id.clone(),
            reason: "no longer needed".into(),
            request_id: "direct-cancel-request".into(),
        };

        let first = harness
            .service
            .cancel_task(&harness.leader_caller, request.clone())
            .expect("direct cancel");
        assert_eq!(first.task.state, TaskState::Cancelled);
        assert!(first.message.is_none());

        let retry = harness
            .service
            .cancel_task(&harness.leader_caller, request.clone())
            .expect("same request is idempotent");
        assert_eq!(retry.task.id, first.task.id);
        assert_eq!(retry.task.version, first.task.version);
        assert!(retry.message.is_none());

        let changed_reason = harness.service.cancel_task(
            &harness.leader_caller,
            CancelTaskRequest {
                reason: "different semantics".into(),
                ..request.clone()
            },
        );
        assert!(matches!(
            changed_reason,
            Err(CollaborationError::Conflict(_))
        ));

        let reused_for_message = harness.service.send_message(
            &harness.leader_caller,
            message_request("direct-cancel-request", "different operation"),
        );
        assert!(matches!(
            reused_for_message,
            Err(CollaborationError::Conflict(_))
        ));

        let second = harness
            .service
            .assign_task(&harness.leader_caller, assign_request("direct-cancel-2"))
            .expect("assign second task");
        let changed_task = harness.service.cancel_task(
            &harness.leader_caller,
            CancelTaskRequest {
                task_id: second.task.id.clone(),
                ..request
            },
        );
        assert!(matches!(changed_task, Err(CollaborationError::Conflict(_))));
        assert_eq!(
            harness
                .service
                .store()
                .task(&second.task.id)
                .expect("second task unchanged")
                .state,
            TaskState::Assigned
        );
    }

    #[test]
    fn pause_resume_and_display_only_team_edit_preserve_routing() {
        let harness = setup();
        let queued = harness
            .service
            .send_message(&harness.leader_caller, message_request("pause-1", "queued"))
            .expect("send");
        harness
            .service
            .set_team_enabled(&harness.team.id, false)
            .expect("pause");
        assert_eq!(
            harness
                .service
                .store()
                .message(&queued.id)
                .expect("suspended")
                .state,
            MessageState::Suspended
        );
        let before = harness
            .service
            .store()
            .team(&harness.team.id)
            .expect("before");
        let config = harness
            .service
            .team_configuration(&harness.team.id)
            .expect("config");
        let active_session = |member_id: &str| {
            config
                .bindings
                .iter()
                .find(|binding| binding.member_id == member_id && binding.released_at.is_none())
                .expect("active binding")
                .grok_session_id
                .clone()
        };
        let replaced = harness
            .service
            .replace_team_config(
                &harness.team.id,
                ReplaceTeamConfigRequest {
                    name: "Renamed team".into(),
                    workspace_fingerprint: before.workspace_fingerprint.clone(),
                    members: vec![
                        TeamConfigMemberInput {
                            id: Some(harness.leader.id.clone()),
                            alias: harness.leader.alias.clone(),
                            display_name: "Lead display".into(),
                            avatar_id: "leader-new".into(),
                            role: Role::Leader,
                            enabled: true,
                            grok_session_id: Some(active_session(&harness.leader.id)),
                        },
                        TeamConfigMemberInput {
                            id: Some(harness.worker_a.id.clone()),
                            alias: harness.worker_a.alias.clone(),
                            display_name: harness.worker_a.display_name.clone(),
                            avatar_id: harness.worker_a.avatar_id.clone(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some(active_session(&harness.worker_a.id)),
                        },
                        TeamConfigMemberInput {
                            id: Some(harness.worker_b.id.clone()),
                            alias: harness.worker_b.alias.clone(),
                            display_name: harness.worker_b.display_name.clone(),
                            avatar_id: harness.worker_b.avatar_id.clone(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some(active_session(&harness.worker_b.id)),
                        },
                        TeamConfigMemberInput {
                            id: Some(harness.worker_c.id.clone()),
                            alias: harness.worker_c.alias.clone(),
                            display_name: harness.worker_c.display_name.clone(),
                            avatar_id: harness.worker_c.avatar_id.clone(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: Some(active_session(&harness.worker_c.id)),
                        },
                    ],
                },
            )
            .expect("display edit");
        assert_eq!(replaced.team.routing_revision, before.routing_revision);
        assert!(replaced.team.config_revision > before.config_revision);
        harness
            .service
            .set_team_enabled(&harness.team.id, true)
            .expect("resume");
        assert_eq!(
            harness
                .service
                .store()
                .message(&queued.id)
                .expect("queued again")
                .state,
            MessageState::Queued
        );
    }

    #[test]
    fn paused_unbound_roster_can_be_saved_but_not_enabled_until_bound() {
        let service = CollaborationService::in_memory().expect("service");
        service.set_global_enabled(true).expect("global");
        let team = service
            .create_team(NewTeam {
                name: "Draft".into(),
                workspace_fingerprint: "workspace-draft".into(),
                enabled: false,
            })
            .expect("team");
        let draft = service
            .replace_team_config(
                &team.id,
                ReplaceTeamConfigRequest {
                    name: "Draft".into(),
                    workspace_fingerprint: "workspace-draft".into(),
                    members: vec![
                        TeamConfigMemberInput {
                            id: None,
                            alias: "main".into(),
                            display_name: "Main".into(),
                            avatar_id: "leader-1".into(),
                            role: Role::Leader,
                            enabled: true,
                            grok_session_id: None,
                        },
                        TeamConfigMemberInput {
                            id: None,
                            alias: "worker-a".into(),
                            display_name: "Worker".into(),
                            avatar_id: "worker-1".into(),
                            role: Role::Worker,
                            enabled: true,
                            grok_session_id: None,
                        },
                    ],
                },
            )
            .expect("save unbound draft");
        assert!(draft.bindings.is_empty());
        assert!(matches!(
            service.set_team_enabled(&team.id, true),
            Err(CollaborationError::InvalidInput(_))
        ));
        for member in &draft.members {
            service
                .bind_member(NewBinding {
                    member_id: member.id.clone(),
                    grok_session_id: format!("grok-{}", member.alias),
                })
                .expect("bind member");
        }
        assert!(
            service
                .set_team_enabled(&team.id, true)
                .expect("enable fully bound roster")
                .enabled
        );
    }
}
