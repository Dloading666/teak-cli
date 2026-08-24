//! Tauri-facing management bridge for Grok Build collaboration settings.
//!
//! This module deliberately does not own terminal discovery, PTY writes, or
//! broker/helper startup. It validates and maps UI DTOs onto the durable core,
//! then returns an explicit lifecycle directive for the integration layer to
//! reconcile. Normal terminal sessions are never touched here.

use super::grok::HelperInvocation;
use super::model::{
    CollabResult, CollaborationError, ListenerState, NewTeam, ReplaceTeamConfigRequest, Role,
    Runtime, RuntimeState, TeamConfigMemberInput, TeamConfiguration, PROVIDER_GROK_BUILD,
};
use super::service::CollaborationService;
use super::store::{runtime_from_row, RUNTIME_COLUMNS};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MAX_WORKERS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollaborationProviderDto {
    #[serde(rename = "grok-build")]
    GrokBuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationMemberRoleDto {
    Leader,
    Worker,
}

impl From<CollaborationMemberRoleDto> for Role {
    fn from(value: CollaborationMemberRoleDto) -> Self {
        match value {
            CollaborationMemberRoleDto::Leader => Self::Leader,
            CollaborationMemberRoleDto::Worker => Self::Worker,
        }
    }
}

impl From<Role> for CollaborationMemberRoleDto {
    fn from(value: Role) -> Self {
        match value {
            Role::Leader => Self::Leader,
            Role::Worker => Self::Worker,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationMemberStatusDto {
    Unbound,
    Connecting,
    Ready,
    Busy,
    WaitingUser,
    Offline,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollaborationMemberDto {
    pub id: String,
    pub alias: String,
    pub display_name: String,
    pub role: CollaborationMemberRoleDto,
    pub avatar_id: String,
    pub native_session_id: String,
    pub status: CollaborationMemberStatusDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollaborationTeamDto {
    pub id: String,
    pub name: String,
    pub workspace: String,
    pub provider: CollaborationProviderDto,
    pub leader: CollaborationMemberDto,
    pub workers: Vec<CollaborationMemberDto>,
    pub paused: bool,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_tasks: Option<i64>,
    /// Newest terminal reports for this team. IDs and outcomes only; never
    /// title, instructions, or report body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_reports: Vec<CollaborationReportDeliveryDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollaborationReportOutcomeDto {
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollaborationReportDeliveryDto {
    pub id: String,
    pub worker_member_id: String,
    pub outcome: CollaborationReportOutcomeDto,
    pub delivered_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollaborationSnapshotDto {
    pub enabled: bool,
    pub teams: Vec<CollaborationTeamDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokSessionStateDto {
    Saved,
    Live,
    Ready,
    Busy,
    WaitingUser,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrokSessionOptionDto {
    pub native_session_id: String,
    pub label: String,
    pub workspace: String,
    pub state: GrokSessionStateDto,
}

/// Small, transport-neutral record that the existing terminal/history
/// registry can map into without exposing its private session structs here.
/// Entries without a real Grok-native session token must pass `None` and are
/// intentionally omitted; a Teak terminal tab ID is not a valid substitute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSessionRegistryEntry {
    pub tool: String,
    pub native_session_id: Option<String>,
    pub label: String,
    pub workspace: String,
    pub state: GrokSessionStateDto,
}

/// Minimal live-terminal attestation supplied by Teak's application-owned
/// registry. The frontend never supplies these records: Tauri snapshots them
/// immediately before computing a launch/bootstrap decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTerminalSessionEntry {
    pub terminal_session_id: String,
    pub tool: String,
    pub native_session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberLaunchStatusDto {
    AlreadyCollaborationActive,
    OrdinaryLiveCollision,
    ResumeAllowed,
    Blocked,
}

/// Backend-owned launch decision. Display fields come from the current
/// durable roster, never from a stale editor draft. `runtime_generation` is
/// present only when an exact collaboration runtime is already live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberLaunchPlanDto {
    pub status: MemberLaunchStatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub team_id: String,
    pub member_id: String,
    pub member_alias: String,
    pub member_display_name: String,
    pub terminal_title: String,
    pub workspace: String,
    pub native_session_id: String,
    pub revision: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_generation: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapStatusDto {
    AlreadyReady,
    PromptRequired,
}

/// Result of an explicit, user-visible bootstrap request. Teak returns the
/// prompt but never writes it to a PTY in this layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BootstrapAttemptDto {
    pub status: BootstrapStatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub terminal_session_id: String,
    pub runtime_generation: i64,
}

/// Durable mutations are complete when this is returned, but broker/helper
/// lifecycle reconciliation is intentionally left to the owning integration
/// layer. There is no PTY-input fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeLifecycleDirective {
    None,
    ReconcileTeam {
        team_id: String,
        paused: bool,
        archived: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "the runtime lifecycle directive must be reconciled by the integration layer"]
pub struct ManagementMutation<T> {
    pub value: T,
    pub lifecycle: RuntimeLifecycleDirective,
}

/// Returns the settings snapshot from the collaboration database. Session
/// discovery is deliberately not performed here; it belongs to
/// `list_grok_sessions` and the terminal registry.
pub fn get_snapshot(service: &CollaborationService) -> CollabResult<CollaborationSnapshotDto> {
    let enabled = service.global_enabled()?;
    let configurations = service.list_team_configurations()?;
    let mut teams = Vec::with_capacity(configurations.len());
    for configuration in configurations {
        teams.push(team_configuration_to_dto(service, configuration)?);
    }
    Ok(CollaborationSnapshotDto { enabled, teams })
}

/// Saves a complete paused roster. Non-UUID team IDs mean create; non-UUID
/// member IDs become `None` so the core generates trusted IDs. A UUID member
/// is retained only for an existing team, where `replace_team_config` verifies
/// that it already belongs to that team.
pub fn save_team(
    service: &CollaborationService,
    input: CollaborationTeamDto,
) -> CollabResult<ManagementMutation<CollaborationTeamDto>> {
    let validated = validate_team_input(input)?;
    let existing_team_id = persistent_id(&validated.dto.id);
    let (team_id, before_routing_revision, created) = if let Some(team_id) = existing_team_id {
        let before = service.team_configuration(&team_id)?;
        if before.team.provider != PROVIDER_GROK_BUILD {
            return Err(CollaborationError::Unauthorized("provider"));
        }
        if let Some(expected) = validated.dto.revision {
            if expected != before.team.config_revision {
                return Err(CollaborationError::Conflict(
                    "team configuration changed; reload before saving".into(),
                ));
            }
        }
        (team_id, Some(before.team.routing_revision), false)
    } else {
        let team = service.create_team(NewTeam {
            name: validated.name.clone(),
            workspace_fingerprint: validated.workspace.clone(),
            enabled: false,
        })?;
        (team.id, None, true)
    };

    let request = build_replace_request(
        &validated,
        if created {
            MemberIdPolicy::AllNew
        } else {
            MemberIdPolicy::KeepUuids
        },
    );
    let configuration = match service.replace_team_config(&team_id, request) {
        Ok(configuration) => configuration,
        Err(error) => {
            // Creation and first roster replacement are separate public core
            // operations. Compensate a failed first replacement by archiving
            // the just-created empty team so it cannot appear as usable.
            if created {
                let _ = service.archive_team(&team_id);
            }
            return Err(error);
        }
    };
    let routing_changed = before_routing_revision
        .is_none_or(|revision| revision != configuration.team.routing_revision);
    let value = team_configuration_to_dto(service, configuration)?;
    let lifecycle = if routing_changed {
        RuntimeLifecycleDirective::ReconcileTeam {
            team_id: value.id.clone(),
            paused: value.paused,
            archived: value.archived,
        }
    } else {
        RuntimeLifecycleDirective::None
    };
    Ok(ManagementMutation { value, lifecycle })
}

/// `paused = false` is the activation boundary. The bridge performs a clear
/// preflight for a complete 1+1..3 roster and active bindings; the service
/// repeats the checks transactionally together with the global gate.
pub fn set_team_paused(
    service: &CollaborationService,
    team_id: &str,
    paused: bool,
) -> CollabResult<ManagementMutation<CollaborationTeamDto>> {
    let team_id = require_persistent_id("team id", team_id)?;
    if !paused {
        let configuration = service.team_configuration(&team_id)?;
        ensure_ready_to_unpause(&configuration)?;
    }
    service.set_team_enabled(&team_id, !paused)?;
    let value = team_configuration_to_dto(service, service.team_configuration(&team_id)?)?;
    Ok(ManagementMutation {
        lifecycle: RuntimeLifecycleDirective::ReconcileTeam {
            team_id,
            paused: value.paused,
            archived: value.archived,
        },
        value,
    })
}

pub fn archive_team(
    service: &CollaborationService,
    team_id: &str,
) -> CollabResult<ManagementMutation<()>> {
    let team_id = require_persistent_id("team id", team_id)?;
    service.archive_team(&team_id)?;
    Ok(ManagementMutation {
        value: (),
        lifecycle: RuntimeLifecycleDirective::ReconcileTeam {
            team_id,
            paused: true,
            archived: true,
        },
    })
}

/// Filters and deduplicates records supplied by Teak's existing native/live
/// session registries. It does not scan Grok files or consult collaboration
/// bindings, so ordinary session discovery remains the single source of truth.
pub fn list_grok_sessions(
    entries: impl IntoIterator<Item = NativeSessionRegistryEntry>,
) -> Vec<GrokSessionOptionDto> {
    let mut sessions = BTreeMap::<String, GrokSessionOptionDto>::new();
    for entry in entries {
        if !entry.tool.eq_ignore_ascii_case("grok") {
            continue;
        }
        let Some(native_session_id) = entry
            .native_session_id
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let label = nonempty_or(entry.label.trim(), &native_session_id).to_owned();
        let workspace = entry.workspace.trim().to_owned();
        let candidate = GrokSessionOptionDto {
            native_session_id: native_session_id.clone(),
            label,
            workspace,
            state: entry.state,
        };
        match sessions.entry(native_session_id) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                let current = slot.get_mut();
                if session_state_rank(candidate.state) > session_state_rank(current.state) {
                    current.state = candidate.state;
                }
                if (current.label.is_empty() || current.label == current.native_session_id)
                    && !candidate.label.is_empty()
                {
                    current.label = candidate.label;
                }
                if current.workspace.is_empty() && !candidate.workspace.is_empty() {
                    current.workspace = candidate.workspace;
                }
            }
        }
    }
    let mut result: Vec<_> = sessions.into_values().collect();
    result.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.native_session_id.cmp(&right.native_session_id))
    });
    result
}

/// Computes the only supported launch/resume decision for one saved team
/// member. This is intentionally advisory with respect to process spawning:
/// `prepare_grok_resume` repeats the security-sensitive checks while creating
/// the exact runtime generation.
pub fn get_member_launch_plan(
    service: &CollaborationService,
    team_id: &str,
    member_id: &str,
    expected_revision: Option<i64>,
    broker_running: bool,
    live_terminals: &[LiveTerminalSessionEntry],
) -> CollabResult<MemberLaunchPlanDto> {
    let team_id = require_persistent_id("team id", team_id)?;
    let member_id = require_persistent_id("member id", member_id)?;
    let configuration = service.team_configuration(&team_id)?;
    let member = configuration
        .members
        .iter()
        .find(|member| member.id == member_id)
        .ok_or(CollaborationError::NotFound("member"))?;
    let binding = configuration.bindings.iter().find(|binding| {
        binding.member_id == member.id
            && binding.released_at.is_none()
            && binding.provider == PROVIDER_GROK_BUILD
    });
    let native_session_id = binding
        .map(|binding| binding.grok_session_id.clone())
        .unwrap_or_default();
    let terminal_title = format!("{} · {}", member.display_name, configuration.team.name);
    let plan = |status,
                reason_code: Option<&str>,
                terminal_session_id: Option<String>,
                runtime_generation: Option<i64>| MemberLaunchPlanDto {
        status,
        reason_code: reason_code.map(str::to_owned),
        team_id: configuration.team.id.clone(),
        member_id: member.id.clone(),
        member_alias: member.alias.clone(),
        member_display_name: member.display_name.clone(),
        terminal_title: terminal_title.clone(),
        workspace: configuration.team.workspace_fingerprint.clone(),
        native_session_id: native_session_id.clone(),
        revision: configuration.team.config_revision,
        terminal_session_id,
        runtime_generation,
    };

    if expected_revision != Some(configuration.team.config_revision) {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("stale_team_revision"),
            None,
            None,
        ));
    }
    if configuration.team.provider != PROVIDER_GROK_BUILD {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("unsupported_provider"),
            None,
            None,
        ));
    }
    if !service.global_enabled()? {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("collaboration_disabled"),
            None,
            None,
        ));
    }
    if configuration.team.archived_at.is_some() {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("team_archived"),
            None,
            None,
        ));
    }
    if !configuration.team.enabled {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("team_paused"),
            None,
            None,
        ));
    }
    if !member.enabled {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("member_disabled"),
            None,
            None,
        ));
    }
    let Some(binding) = binding else {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("member_unbound"),
            None,
            None,
        ));
    };
    match canonicalize_workspace(&configuration.team.workspace_fingerprint) {
        Ok(workspace) if workspace == configuration.team.workspace_fingerprint => {}
        Ok(_) => {
            return Ok(plan(
                MemberLaunchStatusDto::Blocked,
                Some("workspace_attestation_changed"),
                None,
                None,
            ));
        }
        Err(_) => {
            return Ok(plan(
                MemberLaunchStatusDto::Blocked,
                Some("workspace_unavailable"),
                None,
                None,
            ));
        }
    }
    if !broker_running {
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("broker_unavailable"),
            None,
            None,
        ));
    }

    if let Some(runtime) = service.active_runtime_for_grok_session(&binding.grok_session_id)? {
        if !runtime_matches_scope(&configuration, member, binding, &runtime) {
            return Ok(plan(
                MemberLaunchStatusDto::Blocked,
                Some("runtime_scope_mismatch"),
                Some(runtime.terminal_session_id),
                Some(runtime.terminal_generation),
            ));
        }
        if live_terminal_matches(live_terminals, &runtime, &binding.grok_session_id) {
            return Ok(plan(
                MemberLaunchStatusDto::AlreadyCollaborationActive,
                None,
                Some(runtime.terminal_session_id),
                Some(runtime.terminal_generation),
            ));
        }
        return Ok(plan(
            MemberLaunchStatusDto::Blocked,
            Some("runtime_not_live"),
            Some(runtime.terminal_session_id),
            Some(runtime.terminal_generation),
        ));
    }

    if let Some(live) = live_terminals.iter().find(|live| {
        live.tool.eq_ignore_ascii_case("grok")
            && live.native_session_id.as_deref() == Some(binding.grok_session_id.as_str())
    }) {
        return Ok(plan(
            MemberLaunchStatusDto::OrdinaryLiveCollision,
            Some("native_session_live_outside_collaboration"),
            Some(live.terminal_session_id.clone()),
            None,
        ));
    }

    Ok(plan(MemberLaunchStatusDto::ResumeAllowed, None, None, None))
}

/// Authorizes one explicit bootstrap turn after the collaboration-bound Grok
/// process has started. The returned prompt is intentionally data-only; the
/// caller must display it as a normal user message before sending it through
/// the existing terminal action path.
pub fn begin_bootstrap(
    service: &CollaborationService,
    helper: &HelperInvocation,
    team_id: &str,
    member_id: &str,
    terminal_session_id: &str,
    expected_generation: i64,
    broker_running: bool,
    live_terminals: &[LiveTerminalSessionEntry],
) -> CollabResult<BootstrapAttemptDto> {
    let team_id = require_persistent_id("team id", team_id)?;
    let member_id = require_persistent_id("member id", member_id)?;
    let terminal_session_id = terminal_session_id.trim();
    if terminal_session_id.is_empty() {
        return Err(CollaborationError::InvalidInput(
            "terminal session id is required".into(),
        ));
    }
    if expected_generation < 1 {
        return Err(CollaborationError::InvalidInput(
            "runtime generation must be positive".into(),
        ));
    }
    if !broker_running {
        return Err(CollaborationError::Suspended);
    }
    if !service.global_enabled()? {
        return Err(CollaborationError::Suspended);
    }

    let configuration = service.team_configuration(&team_id)?;
    if configuration.team.provider != PROVIDER_GROK_BUILD
        || configuration.team.archived_at.is_some()
        || !configuration.team.enabled
    {
        return Err(CollaborationError::Suspended);
    }
    let member = configuration
        .members
        .iter()
        .find(|member| member.id == member_id && member.enabled)
        .ok_or(CollaborationError::NotFound("enabled member"))?;
    let binding = configuration
        .bindings
        .iter()
        .find(|binding| {
            binding.member_id == member.id
                && binding.released_at.is_none()
                && binding.provider == PROVIDER_GROK_BUILD
        })
        .ok_or(CollaborationError::NotFound("active binding"))?;
    let runtime = service
        .active_runtime_for_grok_session(&binding.grok_session_id)?
        .ok_or(CollaborationError::Unauthorized("runtime_unavailable"))?;
    if !runtime_matches_scope(&configuration, member, binding, &runtime)
        || runtime.terminal_session_id != terminal_session_id
    {
        return Err(CollaborationError::Unauthorized("runtime_scope"));
    }
    if runtime.terminal_generation != expected_generation {
        return Err(CollaborationError::StaleGeneration);
    }
    if !live_terminal_matches(live_terminals, &runtime, &binding.grok_session_id) {
        return Err(CollaborationError::Unauthorized("terminal_not_live"));
    }
    if runtime.runtime_state == RuntimeState::Exited {
        return Err(CollaborationError::InvalidState {
            entity: "runtime",
            state: "exited".into(),
        });
    }
    if runtime.listener_state == ListenerState::Ready {
        return Ok(BootstrapAttemptDto {
            status: BootstrapStatusDto::AlreadyReady,
            attempt_id: None,
            prompt: None,
            terminal_session_id: runtime.terminal_session_id,
            runtime_generation: runtime.terminal_generation,
        });
    }
    if runtime.listener_state != ListenerState::Connecting {
        return Err(CollaborationError::InvalidState {
            entity: "collaboration listener",
            state: runtime.listener_state.to_string(),
        });
    }

    // The runtime id is unique per registered generation, making this stable
    // across retries without another mutable "bootstrap attempt" table.
    let attempt_id = format!("bootstrap-{}", runtime.id);
    let listen_command = format!(
        "{} listen",
        helper
            .shell_prefix()
            .map_err(|error| CollaborationError::InvalidInput(error.to_string()))?
    );
    let prompt = render_bootstrap_prompt(&attempt_id, &member.alias, &listen_command);
    Ok(BootstrapAttemptDto {
        status: BootstrapStatusDto::PromptRequired,
        attempt_id: Some(attempt_id),
        prompt: Some(prompt),
        terminal_session_id: runtime.terminal_session_id,
        runtime_generation: runtime.terminal_generation,
    })
}

fn runtime_matches_scope(
    configuration: &TeamConfiguration,
    member: &super::model::Member,
    binding: &super::model::Binding,
    runtime: &Runtime,
) -> bool {
    runtime.revoked_at.is_none()
        && runtime.member_id == member.id
        && runtime.binding_id == binding.id
        && runtime.observed_grok_session_id == binding.grok_session_id
        && runtime.routing_revision == configuration.team.routing_revision
        && runtime.attested_provider == PROVIDER_GROK_BUILD
        && runtime.attested_workspace_fingerprint == configuration.team.workspace_fingerprint
}

fn live_terminal_matches(
    live_terminals: &[LiveTerminalSessionEntry],
    runtime: &Runtime,
    native_session_id: &str,
) -> bool {
    live_terminals.iter().any(|live| {
        live.terminal_session_id == runtime.terminal_session_id
            && live.tool.eq_ignore_ascii_case("grok")
            && live.native_session_id.as_deref() == Some(native_session_id)
    })
}

fn render_bootstrap_prompt(attempt_id: &str, alias: &str, listen_command: &str) -> String {
    format!(
        "Teak collaboration bootstrap ({attempt_id}) for the authorized member `{alias}`.\n\
         This is an explicit, user-visible setup turn. Use Grok's `monitor` tool exactly once with the following parameters:\n\
         - command: `{listen_command}`\n\
         - description: `Teak collaboration inbox for {alias}`\n\
         - persistent: `true`\n\
         Do not run this as a normal or background Bash command, and do not alter, chain, pipe, redirect, or wrap the command. After the monitor starts, prefer helper inline flags such as --text, --title, --instructions, and --summary for later collaboration calls. After the monitor starts, briefly confirm that Teak collaboration is initialized."
    )
}

/// Canonical workspace paths are persisted as the runtime attestation scope.
/// Nonexistent paths, files, and non-UTF-8 paths are rejected.
pub fn canonicalize_workspace(workspace: &str) -> CollabResult<String> {
    let workspace = workspace.trim();
    if workspace.is_empty() {
        return Err(CollaborationError::InvalidInput(
            "workspace is required".into(),
        ));
    }
    let canonical = Path::new(workspace).canonicalize().map_err(|error| {
        CollaborationError::InvalidInput(format!("workspace does not exist: {error}"))
    })?;
    if !canonical.is_dir() {
        return Err(CollaborationError::InvalidInput(
            "workspace must be an existing directory".into(),
        ));
    }
    path_to_attestation_string(canonical)
}

fn path_to_attestation_string(path: PathBuf) -> CollabResult<String> {
    #[cfg(windows)]
    let path = {
        let rendered = path.to_string_lossy();
        rendered
            .strip_prefix(r"\\?\")
            .map(PathBuf::from)
            .unwrap_or(path)
    };
    path.into_os_string()
        .into_string()
        .map_err(|_| CollaborationError::InvalidInput("workspace path must be valid UTF-8".into()))
}

#[derive(Debug)]
struct ValidatedTeamInput {
    dto: CollaborationTeamDto,
    name: String,
    workspace: String,
}

fn validate_team_input(mut dto: CollaborationTeamDto) -> CollabResult<ValidatedTeamInput> {
    if dto.archived {
        return Err(CollaborationError::InvalidInput(
            "archived teams cannot be saved".into(),
        ));
    }
    if !dto.paused {
        return Err(CollaborationError::InvalidState {
            entity: "team",
            state: "enabled; pause before editing".into(),
        });
    }
    if dto.workers.is_empty() || dto.workers.len() > MAX_WORKERS {
        return Err(CollaborationError::Capacity("team_workers"));
    }
    if dto.leader.role != CollaborationMemberRoleDto::Leader
        || dto
            .workers
            .iter()
            .any(|worker| worker.role != CollaborationMemberRoleDto::Worker)
    {
        return Err(CollaborationError::InvalidInput(
            "roster requires one leader and 1-3 workers".into(),
        ));
    }

    let name = dto.name.trim().to_owned();
    if name.is_empty() {
        return Err(CollaborationError::InvalidInput(
            "team name is required".into(),
        ));
    }
    let workspace = canonicalize_workspace(&dto.workspace)?;
    dto.name = name.clone();
    dto.workspace = workspace.clone();

    let mut ids = BTreeSet::new();
    let mut aliases = BTreeSet::new();
    let mut sessions = BTreeSet::new();
    for member in std::iter::once(&mut dto.leader).chain(dto.workers.iter_mut()) {
        member.id = member.id.trim().to_owned();
        member.alias = member.alias.trim().to_owned();
        member.display_name = member.display_name.trim().to_owned();
        member.avatar_id = member.avatar_id.trim().to_owned();
        member.native_session_id = member.native_session_id.trim().to_owned();
        let canonical_id = persistent_id(&member.id).unwrap_or_else(|| member.id.clone());
        if member.id.is_empty() || !ids.insert(canonical_id.clone()) {
            return Err(CollaborationError::InvalidInput(
                "member ids must be present and unique within the editor".into(),
            ));
        }
        member.id = canonical_id;
        if !valid_alias(&member.alias) || !aliases.insert(member.alias.clone()) {
            return Err(CollaborationError::InvalidInput(
                "member aliases are invalid or duplicated".into(),
            ));
        }
        if member.display_name.is_empty() || member.avatar_id.is_empty() {
            return Err(CollaborationError::InvalidInput(
                "member display name and avatar are required".into(),
            ));
        }
        if !member.native_session_id.is_empty() {
            if Uuid::parse_str(&member.native_session_id).is_err() {
                return Err(CollaborationError::InvalidInput(
                    "Grok session ids must be UUIDs".into(),
                ));
            }
            if !sessions.insert(member.native_session_id.clone()) {
                return Err(CollaborationError::InvalidInput(
                    "Grok sessions must be unique within a team".into(),
                ));
            }
        }
    }
    Ok(ValidatedTeamInput {
        dto,
        name,
        workspace,
    })
}

#[derive(Debug, Clone, Copy)]
enum MemberIdPolicy {
    AllNew,
    KeepUuids,
}

fn build_replace_request(
    input: &ValidatedTeamInput,
    id_policy: MemberIdPolicy,
) -> ReplaceTeamConfigRequest {
    let members = std::iter::once(&input.dto.leader)
        .chain(input.dto.workers.iter())
        .map(|member| TeamConfigMemberInput {
            id: match id_policy {
                MemberIdPolicy::AllNew => None,
                MemberIdPolicy::KeepUuids => persistent_id(&member.id),
            },
            alias: member.alias.clone(),
            display_name: member.display_name.clone(),
            avatar_id: member.avatar_id.clone(),
            role: member.role.into(),
            enabled: true,
            grok_session_id: nonempty_owned(&member.native_session_id),
        })
        .collect();
    ReplaceTeamConfigRequest {
        name: input.name.clone(),
        workspace_fingerprint: input.workspace.clone(),
        members,
    }
}

fn ensure_ready_to_unpause(configuration: &TeamConfiguration) -> CollabResult<()> {
    let enabled: Vec<_> = configuration
        .members
        .iter()
        .filter(|member| member.enabled)
        .collect();
    let leaders = enabled
        .iter()
        .filter(|member| member.role == Role::Leader)
        .count();
    let workers = enabled
        .iter()
        .filter(|member| member.role == Role::Worker)
        .count();
    if leaders != 1 || !(1..=MAX_WORKERS).contains(&workers) {
        return Err(CollaborationError::InvalidInput(
            "team activation requires one leader and 1-3 workers".into(),
        ));
    }
    let active_bound_members: BTreeSet<_> = configuration
        .bindings
        .iter()
        .filter(|binding| binding.released_at.is_none() && binding.provider == PROVIDER_GROK_BUILD)
        .map(|binding| binding.member_id.as_str())
        .collect();
    if enabled
        .iter()
        .any(|member| !active_bound_members.contains(member.id.as_str()))
    {
        return Err(CollaborationError::InvalidInput(
            "every member must bind a Grok session before activation".into(),
        ));
    }
    Ok(())
}

fn team_configuration_to_dto(
    service: &CollaborationService,
    configuration: TeamConfiguration,
) -> CollabResult<CollaborationTeamDto> {
    if configuration.team.provider != PROVIDER_GROK_BUILD {
        return Err(CollaborationError::Unauthorized("provider"));
    }
    let (runtimes, pending_tasks, recent_reports) =
        load_team_runtime_view(service, &configuration.team.id)?;
    let active_bindings: BTreeMap<_, _> = configuration
        .bindings
        .iter()
        .filter(|binding| binding.released_at.is_none() && binding.provider == PROVIDER_GROK_BUILD)
        .map(|binding| (binding.member_id.as_str(), binding))
        .collect();

    let mut leader = None;
    let mut workers = Vec::new();
    for member in configuration.members.iter().filter(|member| member.enabled) {
        let binding = active_bindings.get(member.id.as_str()).copied();
        let native_session_id = binding
            .map(|binding| binding.grok_session_id.clone())
            .unwrap_or_default();
        let runtime = runtimes.get(member.id.as_str());
        let runtime_scope_valid = runtime.zip(binding).is_none_or(|(runtime, binding)| {
            runtime.binding_id == binding.id
                && runtime.observed_grok_session_id == binding.grok_session_id
                && runtime.routing_revision == configuration.team.routing_revision
                && runtime.attested_provider == PROVIDER_GROK_BUILD
                && runtime.attested_workspace_fingerprint
                    == configuration.team.workspace_fingerprint
        });
        let status = member_status(binding.is_some(), runtime, runtime_scope_valid);
        let dto = CollaborationMemberDto {
            id: member.id.clone(),
            alias: member.alias.clone(),
            display_name: member.display_name.clone(),
            role: member.role.into(),
            avatar_id: member.avatar_id.clone(),
            native_session_id,
            status,
        };
        match member.role {
            Role::Leader => {
                if leader.is_some() {
                    return Err(CollaborationError::InvalidInput(
                        "team contains multiple enabled leaders".into(),
                    ));
                }
                leader = Some(dto);
            }
            Role::Worker => workers.push(dto),
        }
    }
    let leader = leader
        .ok_or_else(|| CollaborationError::InvalidInput("team has no enabled leader".into()))?;
    if workers.len() > MAX_WORKERS {
        return Err(CollaborationError::Capacity("team_workers"));
    }

    Ok(CollaborationTeamDto {
        id: configuration.team.id,
        name: configuration.team.name,
        workspace: configuration.team.workspace_fingerprint,
        provider: CollaborationProviderDto::GrokBuild,
        leader,
        workers,
        paused: !configuration.team.enabled,
        archived: configuration.team.archived_at.is_some(),
        revision: Some(configuration.team.config_revision),
        pending_tasks: Some(pending_tasks),
        recent_reports,
    })
}

const MAX_RECENT_REPORTS: i64 = 8;

fn load_team_runtime_view(
    service: &CollaborationService,
    team_id: &str,
) -> CollabResult<(
    BTreeMap<String, Runtime>,
    i64,
    Vec<CollaborationReportDeliveryDto>,
)> {
    let connection = service.store().lock()?;
    let runtimes = {
        let mut statement = connection.prepare(&format!(
            "SELECT {RUNTIME_COLUMNS} FROM collab_runtime
             WHERE revoked_at IS NULL AND member_id IN
               (SELECT id FROM collab_member WHERE team_id=?1)"
        ))?;
        let rows = statement
            .query_map([team_id], runtime_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|runtime| (runtime.member_id.clone(), runtime))
            .collect()
    };
    let pending_tasks = connection.query_row(
        "SELECT COUNT(*) FROM collab_task WHERE team_id=?1
         AND state NOT IN ('reported_completed','reported_failed','cancelled')",
        [team_id],
        |row| row.get(0),
    )?;
    let recent_reports = {
        let mut statement = connection.prepare(
            "SELECT id, assignee_member_id, state, terminal_at FROM collab_task
             WHERE team_id=?1
               AND state IN ('reported_completed','reported_failed')
               AND terminal_at IS NOT NULL
             ORDER BY terminal_at DESC
             LIMIT ?2",
        )?;
        let rows = statement
            .query_map(rusqlite::params![team_id, MAX_RECENT_REPORTS], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .filter_map(report_delivery_from_row)
            .collect()
    };
    Ok((runtimes, pending_tasks, recent_reports))
}

fn report_delivery_from_row(
    row: (String, String, String, i64),
) -> Option<CollaborationReportDeliveryDto> {
    let (id, worker_member_id, state, delivered_at) = row;
    let outcome = match state.as_str() {
        "reported_completed" => CollaborationReportOutcomeDto::Completed,
        "reported_failed" => CollaborationReportOutcomeDto::Failed,
        _ => return None,
    };
    Some(CollaborationReportDeliveryDto {
        id,
        worker_member_id,
        outcome,
        delivered_at,
    })
}

fn member_status(
    bound: bool,
    runtime: Option<&Runtime>,
    runtime_scope_valid: bool,
) -> CollaborationMemberStatusDto {
    if !bound {
        return CollaborationMemberStatusDto::Unbound;
    }
    let Some(runtime) = runtime else {
        return CollaborationMemberStatusDto::Offline;
    };
    if !runtime_scope_valid {
        return CollaborationMemberStatusDto::Error;
    }
    match runtime.listener_state {
        ListenerState::Offline => CollaborationMemberStatusDto::Offline,
        ListenerState::Connecting => CollaborationMemberStatusDto::Connecting,
        ListenerState::Degraded => CollaborationMemberStatusDto::Error,
        ListenerState::Ready => match runtime.runtime_state {
            RuntimeState::Unknown => CollaborationMemberStatusDto::Connecting,
            RuntimeState::Idle => CollaborationMemberStatusDto::Ready,
            RuntimeState::Busy => CollaborationMemberStatusDto::Busy,
            RuntimeState::WaitingUser => CollaborationMemberStatusDto::WaitingUser,
            RuntimeState::Exited => CollaborationMemberStatusDto::Offline,
        },
    }
}

fn persistent_id(value: &str) -> Option<String> {
    Uuid::parse_str(value.trim())
        .ok()
        .map(|value| value.to_string())
}

fn require_persistent_id(field: &str, value: &str) -> CollabResult<String> {
    persistent_id(value).ok_or_else(|| {
        CollaborationError::InvalidInput(format!("{field} must be a persisted UUID"))
    })
}

fn valid_alias(alias: &str) -> bool {
    let mut bytes = alias.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    alias.len() <= 32
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn nonempty_owned(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn nonempty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn session_state_rank(state: GrokSessionStateDto) -> u8 {
    match state {
        GrokSessionStateDto::Saved => 0,
        GrokSessionStateDto::Offline => 1,
        GrokSessionStateDto::Live => 2,
        GrokSessionStateDto::Ready => 3,
        GrokSessionStateDto::Busy => 4,
        GrokSessionStateDto::WaitingUser => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::model::{AuthMethod, NewRuntime};

    fn workspace() -> PathBuf {
        let path = std::env::temp_dir().join(format!("teak-management-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).expect("temp workspace");
        path
    }

    fn helper() -> HelperInvocation {
        HelperInvocation::hidden_subcommand(std::env::current_exe().expect("test executable"))
            .expect("test helper")
    }

    fn member(id: &str, role: CollaborationMemberRoleDto, session: &str) -> CollaborationMemberDto {
        CollaborationMemberDto {
            id: id.into(),
            alias: if role == CollaborationMemberRoleDto::Leader {
                "main".into()
            } else {
                "worker-a".into()
            },
            display_name: if role == CollaborationMemberRoleDto::Leader {
                "Main".into()
            } else {
                "Worker A".into()
            },
            role,
            avatar_id: "cedar".into(),
            native_session_id: session.into(),
            status: CollaborationMemberStatusDto::Unbound,
        }
    }

    fn team(workspace: &Path) -> CollaborationTeamDto {
        CollaborationTeamDto {
            id: "team-local".into(),
            name: "Core team".into(),
            workspace: workspace.to_string_lossy().into_owned(),
            provider: CollaborationProviderDto::GrokBuild,
            leader: member("member-local-main", CollaborationMemberRoleDto::Leader, ""),
            workers: vec![member(
                "member-local-worker",
                CollaborationMemberRoleDto::Worker,
                "",
            )],
            paused: true,
            archived: false,
            revision: None,
            pending_tasks: None,
            recent_reports: Vec::new(),
        }
    }

    fn enabled_team(service: &CollaborationService, workspace: &Path) -> CollaborationTeamDto {
        service
            .set_global_enabled(true)
            .expect("enable collaboration core");
        let mut input = team(workspace);
        input.leader.native_session_id = "32f64a93-11f2-4f50-bbc1-56fe3025b8fb".to_string();
        input.workers[0].native_session_id = "7aef2abf-206f-4476-a7a6-29626498ff90".to_string();
        let saved = save_team(service, input).expect("save bound team").value;
        set_team_paused(service, &saved.id, false)
            .expect("activate team")
            .value
    }

    fn register_member_runtime(
        service: &CollaborationService,
        team: &CollaborationTeamDto,
        member: &CollaborationMemberDto,
        terminal_session_id: &str,
        generation: i64,
        listener_state: ListenerState,
    ) -> Runtime {
        let configuration = service.team_configuration(&team.id).expect("configuration");
        let binding = configuration
            .bindings
            .iter()
            .find(|binding| binding.member_id == member.id && binding.released_at.is_none())
            .expect("active binding");
        service
            .register_runtime(NewRuntime {
                member_id: member.id.clone(),
                binding_id: binding.id.clone(),
                terminal_session_id: terminal_session_id.to_string(),
                terminal_generation: generation,
                observed_grok_session_id: binding.grok_session_id.clone(),
                process_id: None,
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(format!("secret-{generation}")),
                token_epoch: generation,
                attested_workspace_fingerprint: team.workspace.clone(),
                grok_version: "1.0.5".into(),
                helper_protocol_version: "1".into(),
                capability_probe_result: "verified".into(),
                listener_state,
                runtime_state: if listener_state == ListenerState::Ready {
                    RuntimeState::Idle
                } else {
                    RuntimeState::Unknown
                },
            })
            .expect("register runtime")
    }

    #[test]
    fn report_delivery_mapping_drops_non_terminal_states() {
        let completed = report_delivery_from_row((
            "task-1".into(),
            "worker-1".into(),
            "reported_completed".into(),
            42,
        ))
        .expect("completed");
        assert_eq!(completed.id, "task-1");
        assert_eq!(completed.worker_member_id, "worker-1");
        assert_eq!(completed.outcome, CollaborationReportOutcomeDto::Completed);
        assert_eq!(completed.delivered_at, 42);
        let failed = report_delivery_from_row((
            "task-2".into(),
            "worker-1".into(),
            "reported_failed".into(),
            7,
        ))
        .expect("failed");
        assert_eq!(failed.outcome, CollaborationReportOutcomeDto::Failed);
        assert!(report_delivery_from_row((
            "task-3".into(),
            "worker-1".into(),
            "running".into(),
            1
        ))
        .is_none());
        let encoded = serde_json::to_value(&completed).expect("serialize");
        assert_eq!(encoded["workerMemberId"], "worker-1");
        assert_eq!(encoded["outcome"], "completed");
        assert!(encoded.get("title").is_none());
        assert!(encoded.get("instructions").is_none());
        assert!(encoded.get("summary").is_none());
        assert!(encoded.get("payload").is_none());
    }

    #[test]
    fn serde_contract_is_camel_case() {
        let path = workspace();
        let encoded = serde_json::to_value(team(&path)).expect("serialize");
        assert!(encoded.get("pendingTasks").is_none());
        assert!(encoded.get("recentReports").is_none());
        assert_eq!(encoded["leader"]["displayName"], "Main");
        assert_eq!(encoded["leader"]["nativeSessionId"], "");
        assert_eq!(encoded["provider"], "grok-build");
        std::fs::remove_dir(&path).expect("remove temp workspace");
    }

    #[test]
    fn paused_unbound_roster_can_be_saved_but_not_unpaused() {
        let path = workspace();
        let service = CollaborationService::in_memory().expect("service");
        let saved = save_team(&service, team(&path)).expect("save").value;
        assert!(Uuid::parse_str(&saved.id).is_ok());
        assert!(Uuid::parse_str(&saved.leader.id).is_ok());
        assert_eq!(saved.leader.status, CollaborationMemberStatusDto::Unbound);
        assert!(set_team_paused(&service, &saved.id, false).is_err());
        let snapshot = get_snapshot(&service).expect("snapshot");
        assert!(!snapshot.enabled);
        assert_eq!(snapshot.teams, vec![saved]);
        let encoded = serde_json::to_value(&snapshot).expect("snapshot json");
        assert!(encoded["teams"][0].get("recentReports").is_none());
        std::fs::remove_dir(&path).expect("remove temp workspace");
    }

    #[test]
    fn registry_mapping_filters_and_prefers_live_state() {
        let sessions = list_grok_sessions([
            NativeSessionRegistryEntry {
                tool: "claude".into(),
                native_session_id: Some("not-grok".into()),
                label: "Other".into(),
                workspace: "/tmp".into(),
                state: GrokSessionStateDto::Live,
            },
            NativeSessionRegistryEntry {
                tool: "grok".into(),
                native_session_id: Some("grok-1".into()),
                label: "Saved name".into(),
                workspace: "/project".into(),
                state: GrokSessionStateDto::Saved,
            },
            NativeSessionRegistryEntry {
                tool: "GROK".into(),
                native_session_id: Some("grok-1".into()),
                label: String::new(),
                workspace: String::new(),
                state: GrokSessionStateDto::Busy,
            },
        ]);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].label, "Saved name");
        assert_eq!(sessions[0].workspace, "/project");
        assert_eq!(sessions[0].state, GrokSessionStateDto::Busy);
    }

    #[test]
    fn launch_plan_is_backend_gated_and_detects_an_ordinary_live_collision() {
        let path = workspace();
        let service = CollaborationService::in_memory().expect("service");
        let saved = enabled_team(&service, &path);
        let revision = saved.revision.expect("revision");

        let allowed = get_member_launch_plan(
            &service,
            &saved.id,
            &saved.leader.id,
            Some(revision),
            true,
            &[],
        )
        .expect("launch plan");
        assert_eq!(allowed.status, MemberLaunchStatusDto::ResumeAllowed);
        assert_eq!(
            allowed.workspace,
            path.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(allowed.member_display_name, "Main");

        let live = vec![LiveTerminalSessionEntry {
            terminal_session_id: "ordinary-grok-tab".into(),
            tool: "grok".into(),
            native_session_id: Some(saved.leader.native_session_id.clone()),
        }];
        let collision = get_member_launch_plan(
            &service,
            &saved.id,
            &saved.leader.id,
            Some(revision),
            true,
            &live,
        )
        .expect("collision plan");
        assert_eq!(
            collision.status,
            MemberLaunchStatusDto::OrdinaryLiveCollision
        );
        assert_eq!(
            collision.terminal_session_id.as_deref(),
            Some("ordinary-grok-tab")
        );

        let stale = get_member_launch_plan(
            &service,
            &saved.id,
            &saved.leader.id,
            Some(revision - 1),
            true,
            &[],
        )
        .expect("stale plan");
        assert_eq!(stale.status, MemberLaunchStatusDto::Blocked);
        assert_eq!(stale.reason_code.as_deref(), Some("stale_team_revision"));
        std::fs::remove_dir(&path).expect("remove temp workspace");
    }

    #[test]
    fn bootstrap_is_exact_generation_idempotent_and_ready_aware() {
        let path = workspace();
        let service = CollaborationService::in_memory().expect("service");
        let saved = enabled_team(&service, &path);
        let runtime = register_member_runtime(
            &service,
            &saved,
            &saved.leader,
            "collaboration-tab",
            41,
            ListenerState::Connecting,
        );
        let live = vec![LiveTerminalSessionEntry {
            terminal_session_id: "collaboration-tab".into(),
            tool: "grok".into(),
            native_session_id: Some(saved.leader.native_session_id.clone()),
        }];

        let plan = get_member_launch_plan(
            &service,
            &saved.id,
            &saved.leader.id,
            saved.revision,
            true,
            &live,
        )
        .expect("active plan");
        assert_eq!(
            plan.status,
            MemberLaunchStatusDto::AlreadyCollaborationActive
        );
        assert_eq!(plan.runtime_generation, Some(41));

        let first = begin_bootstrap(
            &service,
            &helper(),
            &saved.id,
            &saved.leader.id,
            "collaboration-tab",
            41,
            true,
            &live,
        )
        .expect("bootstrap");
        let retry = begin_bootstrap(
            &service,
            &helper(),
            &saved.id,
            &saved.leader.id,
            "collaboration-tab",
            41,
            true,
            &live,
        )
        .expect("bootstrap retry");
        assert_eq!(first, retry);
        assert_eq!(first.status, BootstrapStatusDto::PromptRequired);
        assert_eq!(
            first.attempt_id.as_deref(),
            Some(format!("bootstrap-{}", runtime.id).as_str())
        );
        let prompt = first.prompt.as_deref().expect("visible prompt");
        assert!(prompt.contains("`monitor` tool"));
        assert!(prompt.contains(" listen`"));
        assert!(prompt.contains("persistent: `true`"));
        assert!(begin_bootstrap(
            &service,
            &helper(),
            &saved.id,
            &saved.leader.id,
            "collaboration-tab",
            42,
            true,
            &live,
        )
        .is_err());

        register_member_runtime(
            &service,
            &saved,
            &saved.leader,
            "ready-tab",
            42,
            ListenerState::Ready,
        );
        let ready_live = vec![LiveTerminalSessionEntry {
            terminal_session_id: "ready-tab".into(),
            tool: "grok".into(),
            native_session_id: Some(saved.leader.native_session_id.clone()),
        }];
        let ready = begin_bootstrap(
            &service,
            &helper(),
            &saved.id,
            &saved.leader.id,
            "ready-tab",
            42,
            true,
            &ready_live,
        )
        .expect("already ready");
        assert_eq!(ready.status, BootstrapStatusDto::AlreadyReady);
        assert!(ready.attempt_id.is_none());
        assert!(ready.prompt.is_none());
        std::fs::remove_dir(&path).expect("remove temp workspace");
    }
}
