//! Application-owned collaboration lifecycle and Grok launch integration.

use super::broker::{start_broker, BrokerHandle};
#[cfg(unix)]
use super::grok::install_helper_shim;
use super::grok::{
    build_launch_spec, probe_grok, CollaborationRole, CollaborationRules, GrokLaunchConfig,
    GrokLaunchMode, GrokLaunchSpec, HelperInvocation,
};
use super::management::RuntimeLifecycleDirective;
use super::model::{now_ms, AuthMethod, ListenerState, NewRuntime, Role, RuntimeState};
use super::protocol::{AuthProof, PROTOCOL_VERSION};
use super::CollaborationService;
use std::collections::HashMap;
#[cfg(unix)]
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[cfg(unix)]
struct OwnerLock {
    _file: File,
}

struct ActiveLaunch {
    team_id: String,
    generation: i64,
    body_dir: PathBuf,
}

pub struct PreparedGrokLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub extra_env: Vec<(String, String)>,
    /// Exact capability generation registered for this PTY launch. Lifecycle
    /// callbacks must present it when revoking so a late callback from an old
    /// process cannot remove a replacement launch that reused the tab ID.
    pub generation: i64,
}

pub struct CollaborationRuntime {
    service: Arc<CollaborationService>,
    endpoint: PathBuf,
    body_root: PathBuf,
    helper_bin_root: PathBuf,
    broker: Mutex<Option<BrokerHandle>>,
    launch_guard: Mutex<()>,
    active_launches: Mutex<HashMap<String, ActiveLaunch>>,
    next_generation: AtomicI64,
    startup_error: Mutex<Option<String>>,
    /// Held until every other runtime field has been dropped. This prevents a
    /// duplicate Teak process from touching the collaboration database or
    /// broker before Tauri's release-only single-instance plugin intercepts
    /// it.
    #[cfg(unix)]
    _owner_lock: Option<OwnerLock>,
}

impl CollaborationRuntime {
    pub fn open_default() -> Result<Self, String> {
        let home = dirs::home_dir()
            .ok_or_else(|| "could not locate the user home directory".to_string())?;
        let root = home.join(".teak-cli").join("collaboration");
        Self::open_at(&root)
    }

    fn open_at(root: &Path) -> Result<Self, String> {
        // This must happen before SQLite is opened: opening the service may
        // create/migrate the database, and startup recovery revokes persisted
        // capabilities. A secondary app instance is not allowed to perform
        // either operation against the primary instance.
        #[cfg(unix)]
        let owner_lock = acquire_owner_lock(root)?;

        let service = Arc::new(
            CollaborationService::open(root.join("collaboration.db"))
                .map_err(|error| error.to_string())?,
        );
        service
            .revoke_all_active_runtimes("broker_restart")
            .map_err(|error| format!("collaboration runtime cleanup failed: {error}"))?;
        service
            .recover(now_ms())
            .map_err(|error| format!("collaboration recovery failed: {error}"))?;
        let runtime = Self {
            service,
            endpoint: root.join("broker.sock"),
            body_root: root.join("bodies"),
            helper_bin_root: root.join("bin"),
            broker: Mutex::new(None),
            launch_guard: Mutex::new(()),
            active_launches: Mutex::new(HashMap::new()),
            next_generation: AtomicI64::new(now_ms().max(1)),
            startup_error: Mutex::new(None),
            #[cfg(unix)]
            _owner_lock: Some(owner_lock),
        };

        if runtime
            .service
            .global_enabled()
            .map_err(|error| error.to_string())?
        {
            if let Err(error) = runtime.start_broker_if_needed() {
                // A persisted on-state without a transport must fail closed.
                // This revokes capabilities but never terminates a PTY.
                let _ = runtime.service.set_global_enabled(false);
                if let Ok(mut startup_error) = runtime.startup_error.lock() {
                    *startup_error = Some(error);
                }
            }
        }
        Ok(runtime)
    }

    pub fn service(&self) -> &Arc<CollaborationService> {
        &self.service
    }

    pub fn is_broker_running(&self) -> bool {
        self.broker
            .lock()
            .map(|broker| broker.is_some())
            .unwrap_or(false)
    }

    /// Returns the exact helper command used by both Grok launch rules and the
    /// user-visible monitor bootstrap prompt. On Unix this installs/refreshes
    /// the private no-space shim first, so those two permission surfaces can
    /// never drift back to the quoted application-bundle path.
    pub fn collaboration_helper(&self) -> Result<HelperInvocation, String> {
        let teak_executable = std::env::current_exe()
            .map_err(|error| format!("could not resolve the Teak executable: {error}"))?;
        #[cfg(unix)]
        {
            install_helper_shim(&self.helper_bin_root, &teak_executable)
                .map_err(|error| error.to_string())
        }
        #[cfg(not(unix))]
        {
            HelperInvocation::hidden_subcommand(teak_executable).map_err(|error| error.to_string())
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            self.start_broker_if_needed()?;
            if let Err(error) = self.service.set_global_enabled(true) {
                self.stop_broker();
                return Err(error.to_string());
            }
            // Global disable revokes every runtime and suspends in-flight
            // delivery. Revalidate immediately after re-enabling so messages
            // targeting those retired generations become an explicit
            // user-retry block instead of remaining suspended forever. This
            // deliberately never retargets a message to a new generation.
            if let Err(error) = self.service.recover(now_ms()) {
                let rollback = self.service.set_global_enabled(false);
                self.stop_broker();
                self.cleanup_all_active_launches();
                return match rollback {
                    Ok(()) => Err(format!(
                        "collaboration recovery failed after enabling; collaboration was disabled again: {error}"
                    )),
                    Err(rollback_error) => Err(format!(
                        "collaboration recovery failed after enabling: {error}; disabling collaboration also failed: {rollback_error}"
                    )),
                };
            }
            if let Ok(mut startup_error) = self.startup_error.lock() {
                *startup_error = None;
            }
        } else {
            self.service
                .set_global_enabled(false)
                .map_err(|error| error.to_string())?;
            self.stop_broker();
            self.cleanup_all_active_launches();
        }
        self.notify();
        Ok(())
    }

    pub fn notify(&self) {
        if let Ok(broker) = self.broker.lock() {
            if let Some(broker) = broker.as_ref() {
                broker.notifier().notify();
            }
        }
    }

    pub fn prepare_grok_resume(
        &self,
        terminal_session_id: &str,
        program: &str,
        original_args: &[String],
        workspace: &Path,
        grok_session_id: &str,
        live_grok_sessions: &[(String, String)],
    ) -> Result<Option<PreparedGrokLaunch>, String> {
        let _guard = self
            .launch_guard
            .lock()
            .map_err(|_| "collaboration launch lock was poisoned".to_string())?;
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|error| format!("could not verify collaboration workspace: {error}"))?;
        let workspace_fingerprint = canonical_workspace.to_string_lossy().into_owned();
        let Some(resolved) = self
            .service
            .resolve_enabled_binding(grok_session_id, &workspace_fingerprint)
            .map_err(|error| error.to_string())?
        else {
            // Not a team-authorized session: preserve the normal launch path.
            return Ok(None);
        };

        reject_other_live_grok_session(terminal_session_id, grok_session_id, live_grok_sessions)?;

        if !self.is_broker_running() {
            return Err("collaboration is enabled but its local broker is unavailable".to_string());
        }
        if let Some(active) = resolved
            .active_runtime
            .filter(|active| active.terminal_session_id != terminal_session_id)
        {
            return Err(format!(
                "Grok session {} is already attached to Teak terminal {}",
                grok_session_id, active.terminal_session_id
            ));
        }
        if let Some(active) = self
            .service
            .active_runtime_for_grok_session(grok_session_id)
            .map_err(|error| error.to_string())?
            .filter(|active| active.terminal_session_id != terminal_session_id)
        {
            return Err(format!(
                "Grok session {} is already attached to Teak terminal {}",
                grok_session_id, active.terminal_session_id
            ));
        }

        let binary = PathBuf::from(program);
        let binary_name = binary
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if binary_name != "grok" && binary_name != "grok.exe" {
            return Err(
                "collaboration-bound Grok sessions cannot use a custom command wrapper".to_string(),
            );
        }
        let probe = probe_grok(&binary).map_err(|error| error.to_string())?;
        if !probe.supported {
            return Err(probe.unsupported_reason.unwrap_or_else(|| {
                "this Grok version is not supported for collaboration".to_string()
            }));
        }

        let extra_args = extract_user_resume_args(original_args, grok_session_id)?;
        let team = self
            .service
            .team_configuration(&resolved.team.id)
            .map_err(|error| error.to_string())?;
        let allowed_aliases = match resolved.member.role {
            Role::Leader => team
                .members
                .iter()
                .filter(|member| member.enabled && member.role == Role::Worker)
                .map(|member| member.alias.clone())
                .collect::<Vec<_>>(),
            Role::Worker => team
                .members
                .iter()
                .filter(|member| member.enabled && member.role == Role::Leader)
                .map(|member| member.alias.clone())
                .collect::<Vec<_>>(),
        };
        let role = match resolved.member.role {
            Role::Leader => CollaborationRole::Leader,
            Role::Worker => CollaborationRole::Worker,
        };
        let helper = self.collaboration_helper()?;
        let rules = CollaborationRules {
            role,
            self_alias: resolved.member.alias.clone(),
            allowed_aliases,
        }
        .render(&helper)
        .map_err(|error| error.to_string())?;

        let generation = self.next_generation.fetch_add(1, Ordering::SeqCst).max(1);
        let body_dir = self
            .body_root
            .join(&resolved.member.id)
            .join(generation.to_string());
        create_private_directory(&body_dir)?;
        let bearer = format!("{}.{}", Uuid::new_v4(), Uuid::new_v4());
        let allow_rule = helper
            .grok_allow_rule()
            .map_err(|error| error.to_string())?;
        let mut launch = build_launch_spec(GrokLaunchConfig {
            binary,
            mode: GrokLaunchMode::Resume {
                session_id: grok_session_id.to_string(),
            },
            rules,
            allow_rules: vec![allow_rule],
            deny_rules: Vec::new(),
            helper,
            endpoint: self.endpoint.to_string_lossy().into_owned(),
            body_dir: Some(body_dir.to_string_lossy().into_owned()),
            member_id: resolved.member.id.clone(),
            member_alias: resolved.member.alias.clone(),
            generation: generation.to_string(),
            auth: AuthProof::Bearer {
                token: bearer.clone(),
            },
        })
        .map_err(|error| {
            let _ = std::fs::remove_dir_all(&body_dir);
            error.to_string()
        })?;
        launch.args.extend(extra_args);

        let mut active_launches = self
            .active_launches
            .lock()
            .map_err(|_| "collaboration launch registry was poisoned".to_string())?;
        if let Err(error) = self.service.register_runtime(NewRuntime {
            member_id: resolved.member.id,
            binding_id: resolved.binding.id,
            terminal_session_id: terminal_session_id.to_string(),
            terminal_generation: generation,
            observed_grok_session_id: grok_session_id.to_string(),
            process_id: None,
            auth_method: AuthMethod::EnvBearer,
            bearer_secret: Some(bearer),
            token_epoch: generation,
            attested_workspace_fingerprint: workspace_fingerprint,
            grok_version: probe.version.to_string(),
            helper_protocol_version: PROTOCOL_VERSION.to_string(),
            capability_probe_result: "grok_flags_verified".to_string(),
            listener_state: ListenerState::Connecting,
            runtime_state: RuntimeState::Unknown,
        }) {
            let _ = std::fs::remove_dir_all(&body_dir);
            return Err(error.to_string());
        }

        let replaced = active_launches.insert(
            terminal_session_id.to_string(),
            ActiveLaunch {
                team_id: resolved.team.id,
                generation,
                body_dir,
            },
        );
        drop(active_launches);
        if let Some(replaced) = replaced {
            let _ = std::fs::remove_dir_all(replaced.body_dir);
        }

        Ok(Some(prepared(launch, generation)))
    }

    pub fn revoke_terminal(&self, terminal_session_id: &str, reason: &str) {
        let _ = self.revoke_terminal_matching_generation(terminal_session_id, None, reason);
    }

    /// Revoke only when the currently registered launch still has the
    /// generation owned by the caller. PTY exit and spawn-failure callbacks
    /// use this CAS-style path because a same-tab restart may already have
    /// installed generation N+1 before generation N's watcher runs.
    pub fn revoke_terminal_generation(
        &self,
        terminal_session_id: &str,
        expected_generation: i64,
        reason: &str,
    ) -> bool {
        self.revoke_terminal_matching_generation(
            terminal_session_id,
            Some(expected_generation),
            reason,
        )
    }

    /// Retire a collaboration capability when the Grok process changes its
    /// native session in-place (for example via `/new` or a fork). Team
    /// membership is bound to the user-selected native session ID, not merely
    /// to a long-lived PTY process. The generation comparison keeps a late
    /// token event from an older process from revoking a replacement launch.
    pub fn observe_terminal_native_session(
        &self,
        terminal_session_id: &str,
        expected_generation: i64,
        expected_grok_session_id: &str,
        observed_grok_session_id: &str,
    ) -> bool {
        if observed_grok_session_id == expected_grok_session_id {
            return false;
        }
        self.revoke_terminal_generation(
            terminal_session_id,
            expected_generation,
            "native_session_changed",
        )
    }

    /// Mirrors a Teak-owned terminal activity edge into collaboration state.
    /// The caller supplies only the terminal tab identity and observed state;
    /// the capability generation is resolved from the backend-owned launch
    /// registry so a frontend cannot select another member or generation.
    pub fn observe_terminal_activity(
        &self,
        terminal_session_id: &str,
        runtime_state: RuntimeState,
    ) -> Result<bool, String> {
        let generation = self
            .active_launches
            .lock()
            .map_err(|_| "collaboration launch registry was poisoned".to_string())?
            .get(terminal_session_id)
            .map(|active| active.generation);
        let Some(generation) = generation else {
            return Ok(false);
        };
        let changed = self
            .service
            .observe_ready_runtime_state(terminal_session_id, generation, runtime_state)
            .map_err(|error| error.to_string())?;
        if changed {
            self.notify();
        }
        Ok(changed)
    }

    fn revoke_terminal_matching_generation(
        &self,
        terminal_session_id: &str,
        expected_generation: Option<i64>,
        reason: &str,
    ) -> bool {
        let Ok(mut launches) = self.active_launches.lock() else {
            return false;
        };
        let generation = match (launches.get(terminal_session_id), expected_generation) {
            (Some(active), None) => active.generation,
            (Some(active), Some(expected)) if active.generation == expected => active.generation,
            _ => return false,
        };
        if let Err(error) =
            self.service
                .revoke_terminal_runtime(terminal_session_id, generation, reason)
        {
            // Keep the exact generation and its private body directory available
            // for a later exit/kill/shutdown retry. The durable capability may
            // still be active, so stop the transport before releasing the local
            // registry lock rather than allowing a stale bearer to authenticate.
            eprintln!(
                "[collaboration] runtime revoke failed; broker stopped pending retry: {error}"
            );
            self.stop_broker();
            return false;
        }
        let Some(active) = launches.remove(terminal_session_id) else {
            return false;
        };
        drop(launches);
        let _ = std::fs::remove_dir_all(active.body_dir);
        self.notify();
        true
    }

    /// Applies the lifecycle effect paired with a durable management
    /// mutation. Pausing or archiving a team retires every collaboration
    /// generation and removes its private body directories, while leaving the
    /// underlying PTYs alive as ordinary terminals.
    pub fn reconcile_lifecycle(&self, directive: &RuntimeLifecycleDirective) -> Result<(), String> {
        let _guard = self
            .launch_guard
            .lock()
            .map_err(|_| "collaboration launch lock was poisoned".to_string())?;
        let result = (|| -> Result<(), String> {
            match directive {
                RuntimeLifecycleDirective::None => {}
                RuntimeLifecycleDirective::ReconcileTeam {
                    team_id,
                    paused,
                    archived,
                } if *paused || *archived => {
                    let reason = if *archived {
                        "team_archived"
                    } else {
                        "team_paused"
                    };
                    self.service
                        .revoke_team_active_runtimes(team_id, reason)
                        .map_err(|error| error.to_string())?;
                    let body_dirs = {
                        let mut launches = self.active_launches.lock().map_err(|_| {
                            "collaboration launch registry was poisoned".to_string()
                        })?;
                        let terminal_ids = launches
                            .iter()
                            .filter(|(_, launch)| launch.team_id == *team_id)
                            .map(|(terminal_id, _)| terminal_id.clone())
                            .collect::<Vec<_>>();
                        terminal_ids
                            .into_iter()
                            .filter_map(|terminal_id| launches.remove(&terminal_id))
                            .map(|launch| launch.body_dir)
                            .collect::<Vec<_>>()
                    };
                    let cleanup_errors = body_dirs
                        .into_iter()
                        .filter_map(|body_dir| {
                            std::fs::remove_dir_all(&body_dir)
                                .err()
                                .map(|error| format!("{}: {error}", body_dir.to_string_lossy()))
                        })
                        .collect::<Vec<_>>();
                    if !cleanup_errors.is_empty() {
                        return Err(format!(
                            "could not remove revoked collaboration body directories: {}",
                            cleanup_errors.join("; ")
                        ));
                    }
                }
                RuntimeLifecycleDirective::ReconcileTeam { .. } => {}
            }
            Ok(())
        })();
        // Always wake listeners after a possible durable revoke, including
        // when private body cleanup failed. They must re-authenticate and exit
        // the stale generation even if local cleanup needs user attention.
        self.notify();
        result
    }

    pub fn shutdown(&self) {
        let session_ids = self
            .active_launches
            .lock()
            .map(|launches| launches.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for session_id in session_ids {
            self.revoke_terminal(&session_id, "app_exit");
        }
        self.stop_broker();
    }

    fn start_broker_if_needed(&self) -> Result<(), String> {
        let mut broker = self
            .broker
            .lock()
            .map_err(|_| "collaboration broker lock was poisoned".to_string())?;
        if broker.is_none() {
            *broker = Some(start_broker(self.service.clone(), self.endpoint.clone())?);
        }
        Ok(())
    }

    fn stop_broker(&self) {
        let broker = self.broker.lock().ok().and_then(|mut value| value.take());
        if let Some(mut broker) = broker {
            broker.shutdown();
        }
    }

    fn cleanup_all_active_launches(&self) {
        let launches = self
            .active_launches
            .lock()
            .map(|mut launches| {
                launches
                    .drain()
                    .map(|(_, launch)| launch)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for launch in launches {
            let _ = std::fs::remove_dir_all(launch.body_dir);
        }
    }
}

impl Drop for CollaborationRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn prepared(launch: GrokLaunchSpec, generation: i64) -> PreparedGrokLaunch {
    PreparedGrokLaunch {
        program: launch.program,
        args: launch.args,
        extra_env: launch.extra_env,
        generation,
    }
}

fn extract_user_resume_args(
    original_args: &[String],
    grok_session_id: &str,
) -> Result<Vec<String>, String> {
    // Grok 1.0.5 parses long options case-sensitively. Keep this list aligned
    // with its documented canonical flags and compatibility aliases: user
    // tool configuration must not replace Teak-owned policy, select another
    // native identity/workspace, or bypass the visible bootstrap turn.
    const RESERVED_LONG_OPTIONS: &[&str] = &[
        "--",
        "--resume",
        "--load",
        "--continue",
        "--session-id",
        "--fork-session",
        "--cwd",
        "--worktree",
        "--worktree-ref",
        "--ref",
        "--restore-code",
        "--rules",
        "--append-system-prompt",
        "--system-prompt-override",
        "--system-prompt",
        "--allow",
        "--allowedTools",
        "--deny",
        "--disallowedTools",
        "--tools",
        "--disallowed-tools",
        "--agent",
        "--agents",
        "-p",
        "--single",
        "--prompt-file",
        "--prompt-json",
        "--output-format",
        "--json-schema",
        "--max-turns",
    ];
    // These options may tune an otherwise identical native Grok process but
    // cannot select a different session/workspace, inject a prompt, replace
    // collaboration policy, or remove required tools. A split spelling may
    // therefore consume exactly one following non-option token.
    const SAFE_SPLIT_VALUE_OPTIONS: &[&str] = &[
        "--model",
        "--reasoning-effort",
        "--effort",
        "--permission-mode",
        "--sandbox",
        "--debug-file",
        "--leader-socket",
        "--client-identifier",
        "--hunk-tracker-mode",
        "--compaction-mode",
        "--compaction-detail",
        "--background-wait-timeout",
    ];

    fn conflicting_short_option(argument: &str) -> Option<&'static str> {
        let cluster = argument.strip_prefix('-')?;
        if cluster.is_empty() || cluster.starts_with('-') {
            return None;
        }
        for option in cluster.chars() {
            match option {
                // Grok accepts attached values for each of these options.
                'p' => return Some("-p"),
                'r' => return Some("-r"),
                's' => return Some("-s"),
                'w' => return Some("-w"),
                // `-c` is boolean but can occur in a short-option cluster.
                'c' => return Some("-c"),
                // A model value consumes the remainder of the cluster.
                'm' => return None,
                // Other currently documented boolean short options.
                'v' | 'h' => continue,
                _ => return None,
            }
        }
        None
    }

    let mut found_resume = false;
    let mut extras = Vec::new();
    let mut safe_value_for: Option<&str> = None;
    let mut index = 0;
    while index < original_args.len() {
        if original_args[index] == "--resume"
            && original_args.get(index + 1).map(String::as_str) == Some(grok_session_id)
            && !found_resume
        {
            found_resume = true;
            index += 2;
            continue;
        }
        // The production spec always supplies this flag. Drop an identical
        // user-configured copy because Grok rejects duplicate boolean flags.
        if original_args[index] == "--no-subagents" {
            index += 1;
            continue;
        }
        let argument = &original_args[index];
        let conflicting_option = RESERVED_LONG_OPTIONS
            .iter()
            .copied()
            .find(|reserved| argument == reserved || argument.starts_with(&format!("{reserved}=")))
            .or_else(|| conflicting_short_option(argument));
        if let Some(option) = conflicting_option {
            return Err(format!(
                "user Grok argument {option} conflicts with collaboration launch policy"
            ));
        }

        if let Some(option) = safe_value_for.take() {
            if argument.starts_with('-') {
                return Err(format!(
                    "user Grok argument {option} is missing its value for collaboration launch"
                ));
            }
            extras.push(argument.clone());
            index += 1;
            continue;
        }

        if argument == "-m" {
            safe_value_for = Some("-m");
        } else if let Some(option) = SAFE_SPLIT_VALUE_OPTIONS
            .iter()
            .copied()
            .find(|option| argument == option)
        {
            safe_value_for = Some(option);
        } else if !argument.starts_with('-') {
            return Err(
                "user Grok positional prompt conflicts with collaboration launch policy"
                    .to_string(),
            );
        }
        extras.push(argument.clone());
        index += 1;
    }
    if let Some(option) = safe_value_for {
        return Err(format!(
            "user Grok argument {option} is missing its value for collaboration launch"
        ));
    }
    if !found_resume {
        return Err("collaboration launch could not verify the Grok resume arguments".to_string());
    }
    Ok(extras)
}

fn reject_other_live_grok_session(
    terminal_session_id: &str,
    grok_session_id: &str,
    live_grok_sessions: &[(String, String)],
) -> Result<(), String> {
    if let Some((other_terminal_id, _)) =
        live_grok_sessions
            .iter()
            .find(|(other_terminal_id, other_grok_session_id)| {
                other_terminal_id != terminal_session_id && other_grok_session_id == grok_session_id
            })
    {
        return Err(format!(
            "Grok session {grok_session_id} is already active in Teak terminal {other_terminal_id}; close it before starting the collaboration-bound runtime"
        ));
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|error| format!("could not create collaboration body directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("could not protect collaboration body directory: {error}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn acquire_owner_lock(root: &Path) -> Result<OwnerLock, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    if !root.is_absolute() {
        return Err("collaboration runtime root must be absolute".to_string());
    }
    std::fs::create_dir_all(root)
        .map_err(|error| format!("could not create collaboration runtime directory: {error}"))?;

    let root_path = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| "collaboration runtime path contains a NUL byte".to_string())?;
    let root_fd = unsafe {
        libc::open(
            root_path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "refusing unsafe collaboration runtime directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    let root_directory = unsafe { File::from_raw_fd(root_fd) };
    let root_stat = secure_owned_unix_node(
        root_directory.as_raw_fd(),
        libc::S_IFDIR,
        0o700,
        "collaboration runtime directory",
        false,
    )?;

    // Confirm the secured descriptor still names the non-symlink directory
    // reachable at `root`; the database is opened by path immediately after
    // this function returns.
    let path_metadata = std::fs::symlink_metadata(root).map_err(|error| {
        format!("could not revalidate collaboration runtime directory: {error}")
    })?;
    if path_metadata.file_type().is_symlink()
        || !path_metadata.is_dir()
        || path_metadata.dev() != root_stat.st_dev as u64
        || path_metadata.ino() != root_stat.st_ino as u64
    {
        return Err("collaboration runtime directory changed during owner-lock setup".to_string());
    }

    let lock_name = CString::new("owner.lock").expect("static owner lock name");
    let lock_fd = unsafe {
        libc::openat(
            root_directory.as_raw_fd(),
            lock_name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0o600,
        )
    };
    if lock_fd < 0 {
        return Err(format!(
            "could not securely open collaboration owner lock: {}",
            std::io::Error::last_os_error()
        ));
    }
    let lock_file = unsafe { File::from_raw_fd(lock_fd) };
    secure_owned_unix_node(
        lock_file.as_raw_fd(),
        libc::S_IFREG,
        0o600,
        "collaboration owner lock",
        true,
    )?;

    if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        let code = error.raw_os_error();
        if code == Some(libc::EWOULDBLOCK) || code == Some(libc::EAGAIN) {
            return Err(
                "collaboration runtime is already owned by another Teak process".to_string(),
            );
        }
        return Err(format!(
            "could not acquire collaboration runtime owner lock: {error}"
        ));
    }

    Ok(OwnerLock { _file: lock_file })
}

#[cfg(unix)]
fn secure_owned_unix_node(
    fd: std::os::fd::RawFd,
    expected_kind: libc::mode_t,
    expected_permissions: libc::mode_t,
    label: &str,
    require_single_link: bool,
) -> Result<libc::stat, String> {
    let mut stat = unix_fstat(fd, label)?;
    if stat.st_mode & libc::S_IFMT != expected_kind {
        return Err(format!("refusing {label}: unexpected file type"));
    }
    if stat.st_uid != unsafe { libc::geteuid() } {
        return Err(format!("refusing {label}: it is owned by another user"));
    }
    if require_single_link && stat.st_nlink != 1 {
        return Err(format!("refusing {label}: it has multiple hard links"));
    }

    if unsafe { libc::fchmod(fd, expected_permissions) } != 0 {
        return Err(format!(
            "could not protect {label}: {}",
            std::io::Error::last_os_error()
        ));
    }
    stat = unix_fstat(fd, label)?;
    if stat.st_mode & libc::S_IFMT != expected_kind
        || stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & 0o7777 != expected_permissions
        || (require_single_link && stat.st_nlink != 1)
    {
        return Err(format!("could not verify secure {label} metadata"));
    }
    Ok(stat)
}

#[cfg(unix)]
fn unix_fstat(fd: std::os::fd::RawFd, label: &str) -> Result<libc::stat, String> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(format!(
            "could not inspect {label}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { stat.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration::management::{
        archive_team, begin_bootstrap, get_member_launch_plan, set_team_paused, BootstrapStatusDto,
        LiveTerminalSessionEntry, MemberLaunchStatusDto,
    };
    use crate::collaboration::model::{
        ActorType, Binding, CallerIdentity, Member, MessageKind, MessageState, NewBinding,
        NewMember, NewTeam, SendMessageRequest, Team,
    };

    struct LifecycleFixture {
        runtime: CollaborationRuntime,
        team: Team,
        leader: Member,
        leader_binding: Binding,
        worker: Member,
        worker_binding: Binding,
        workspace: PathBuf,
    }

    fn in_memory_runtime() -> CollaborationRuntime {
        CollaborationRuntime {
            service: Arc::new(CollaborationService::in_memory().expect("in-memory service")),
            endpoint: std::env::temp_dir()
                .join(format!("teak-collab-runtime-test-{}.sock", Uuid::new_v4())),
            body_root: std::env::temp_dir().join(format!(
                "teak-collab-runtime-test-bodies-{}",
                Uuid::new_v4()
            )),
            helper_bin_root: std::env::temp_dir()
                .join(format!("teak-collab-runtime-test-bin-{}", Uuid::new_v4())),
            broker: Mutex::new(None),
            launch_guard: Mutex::new(()),
            active_launches: Mutex::new(HashMap::new()),
            next_generation: AtomicI64::new(1),
            startup_error: Mutex::new(None),
            #[cfg(unix)]
            _owner_lock: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn owner_lock_blocks_secondary_runtime_without_mutating_primary_and_releases_on_drop() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = PathBuf::from("/tmp").join(format!("teak-collab-lock-{}", Uuid::new_v4()));
        let first = CollaborationRuntime::open_at(&root).expect("first runtime owns root");
        first.set_enabled(true).expect("enable primary broker");
        assert!(first.is_broker_running());

        let lock_metadata =
            std::fs::symlink_metadata(root.join("owner.lock")).expect("owner lock metadata");
        assert!(lock_metadata.is_file());
        assert!(!lock_metadata.file_type().is_symlink());
        assert_eq!(lock_metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(lock_metadata.permissions().mode() & 0o7777, 0o600);
        let root_metadata = std::fs::symlink_metadata(&root).expect("runtime root metadata");
        assert!(root_metadata.is_dir());
        assert_eq!(root_metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(root_metadata.permissions().mode() & 0o7777, 0o700);

        let secondary_error = CollaborationRuntime::open_at(&root)
            .err()
            .expect("secondary runtime must fail closed");
        assert!(secondary_error.contains("already owned"));
        assert!(first
            .service()
            .global_enabled()
            .expect("primary global state"));
        assert!(first.is_broker_running());

        drop(first);
        let third = CollaborationRuntime::open_at(&root)
            .expect("owner lock must be reacquirable after primary drop");
        assert!(third
            .service()
            .global_enabled()
            .expect("persisted global state"));
        assert!(third.is_broker_running());
        third.set_enabled(false).expect("disable test runtime");
        drop(third);
        std::fs::remove_dir_all(root).expect("remove owner-lock test root");
    }

    #[cfg(unix)]
    #[test]
    fn owner_lock_refuses_a_symlink_before_opening_the_database() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = std::env::temp_dir().join(format!(
            "teak-collaboration-owner-lock-symlink-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir(&root).expect("owner-lock symlink root");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("protect owner-lock symlink root");
        let target = root.join("target");
        std::fs::write(&target, b"do not open through the symlink").expect("symlink target");
        symlink(&target, root.join("owner.lock")).expect("owner lock symlink");

        let error = CollaborationRuntime::open_at(&root)
            .err()
            .expect("unsafe owner lock must fail closed");
        assert!(error.contains("securely open collaboration owner lock"));
        assert!(!root.join("collaboration.db").exists());

        std::fs::remove_dir_all(root).expect("remove owner-lock symlink test root");
    }

    #[cfg(unix)]
    #[test]
    fn owner_lock_refuses_a_symlink_runtime_root() {
        use std::os::unix::fs::symlink;

        let base =
            PathBuf::from("/tmp").join(format!("teak-collab-root-symlink-{}", Uuid::new_v4()));
        let real_root = base.join("real");
        let linked_root = base.join("linked");
        std::fs::create_dir_all(&real_root).expect("real collaboration root");
        symlink(&real_root, &linked_root).expect("linked collaboration root");

        let error = CollaborationRuntime::open_at(&linked_root)
            .err()
            .expect("symlink runtime root must fail closed");
        assert!(error.contains("unsafe collaboration runtime directory"));
        assert!(!real_root.join("collaboration.db").exists());

        std::fs::remove_dir_all(base).expect("remove runtime-root symlink test directory");
    }

    fn lifecycle_fixture(label: &str) -> LifecycleFixture {
        lifecycle_fixture_with_sessions(
            label,
            format!("grok-{label}-main"),
            format!("grok-{label}-worker"),
        )
    }

    fn lifecycle_fixture_with_sessions(
        label: &str,
        leader_session_id: String,
        worker_session_id: String,
    ) -> LifecycleFixture {
        let runtime = in_memory_runtime();
        let service = runtime.service();
        let workspace = std::env::temp_dir().join(format!(
            "teak-collaboration-lifecycle-{label}-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir(&workspace).expect("lifecycle workspace");
        let workspace = workspace
            .canonicalize()
            .expect("canonical lifecycle workspace");
        service.set_global_enabled(true).expect("global enable");
        let team = service
            .create_team(NewTeam {
                name: format!("Lifecycle {label}"),
                workspace_fingerprint: workspace.to_string_lossy().into_owned(),
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
        let worker = service
            .add_member(NewMember {
                team_id: team.id.clone(),
                alias: "worker-a".into(),
                display_name: "Worker A".into(),
                avatar_id: "worker-1".into(),
                role: Role::Worker,
                enabled: true,
            })
            .expect("worker");
        let leader_binding = service
            .bind_member(NewBinding {
                member_id: leader.id.clone(),
                grok_session_id: leader_session_id,
            })
            .expect("leader binding");
        let worker_binding = service
            .bind_member(NewBinding {
                member_id: worker.id.clone(),
                grok_session_id: worker_session_id,
            })
            .expect("worker binding");
        service.install_v1_acl(&team.id).expect("ACL");
        service
            .set_team_enabled(&team.id, true)
            .expect("team enable");
        LifecycleFixture {
            runtime,
            team,
            leader,
            leader_binding,
            worker,
            worker_binding,
            workspace,
        }
    }

    fn register_lifecycle_runtime(
        fixture: &LifecycleFixture,
        terminal_session_id: &str,
        generation: i64,
        listener_state: ListenerState,
    ) -> super::super::model::Runtime {
        register_lifecycle_member_runtime(
            fixture,
            &fixture.leader,
            &fixture.leader_binding,
            terminal_session_id,
            generation,
            listener_state,
            &format!("lifecycle-secret-{generation}"),
        )
    }

    fn register_lifecycle_member_runtime(
        fixture: &LifecycleFixture,
        member: &Member,
        binding: &Binding,
        terminal_session_id: &str,
        generation: i64,
        listener_state: ListenerState,
        secret: &str,
    ) -> super::super::model::Runtime {
        fixture
            .runtime
            .service()
            .register_runtime(NewRuntime {
                member_id: member.id.clone(),
                binding_id: binding.id.clone(),
                terminal_session_id: terminal_session_id.into(),
                terminal_generation: generation,
                observed_grok_session_id: binding.grok_session_id.clone(),
                process_id: None,
                auth_method: AuthMethod::EnvBearer,
                bearer_secret: Some(secret.to_string()),
                token_epoch: generation,
                attested_workspace_fingerprint: fixture.workspace.to_string_lossy().into_owned(),
                grok_version: "test".into(),
                helper_protocol_version: PROTOCOL_VERSION.to_string(),
                capability_probe_result: "ok".into(),
                listener_state,
                runtime_state: if listener_state == ListenerState::Ready {
                    RuntimeState::Idle
                } else {
                    RuntimeState::Unknown
                },
            })
            .expect("lifecycle runtime")
    }

    fn track_lifecycle_launch(
        fixture: &LifecycleFixture,
        terminal_session_id: &str,
        generation: i64,
    ) -> PathBuf {
        let body_dir = fixture
            .runtime
            .body_root
            .join(format!("{terminal_session_id}-{generation}"));
        create_private_directory(&body_dir).expect("lifecycle body directory");
        fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .insert(
                terminal_session_id.into(),
                ActiveLaunch {
                    team_id: fixture.team.id.clone(),
                    generation,
                    body_dir: body_dir.clone(),
                },
            );
        body_dir
    }

    #[cfg(unix)]
    fn write_supported_fake_grok(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let binary = root.join("grok");
        std::fs::write(
            &binary,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'grok 1.0.5 (test)'\nelif [ \"$1\" = \"--help\" ]; then\n  echo '--rules --allow --session-id --resume --no-subagents'\nelse\n  exit 2\nfi\n",
        )
        .expect("write fake Grok");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
            .expect("make fake Grok executable");
        binary
    }

    #[test]
    fn keeps_ordinary_user_args_but_owns_collaboration_flags() {
        let args = vec![
            "--resume".to_string(),
            "32f64a93-11f2-4f50-bbc1-56fe3025b8fb".to_string(),
            "--no-alt-screen".to_string(),
            "--model".to_string(),
            "grok-build".to_string(),
            "--reasoning-effort=xhigh".to_string(),
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
            "--debug-file".to_string(),
            "/tmp/teak-grok-debug.log".to_string(),
            "-mgrok-build".to_string(),
            "--no-subagents".to_string(),
        ];
        assert_eq!(
            extract_user_resume_args(&args, "32f64a93-11f2-4f50-bbc1-56fe3025b8fb").unwrap(),
            vec![
                "--no-alt-screen",
                "--model",
                "grok-build",
                "--reasoning-effort=xhigh",
                "--permission-mode",
                "acceptEdits",
                "--debug-file",
                "/tmp/teak-grok-debug.log",
                "-mgrok-build"
            ]
        );
    }

    #[test]
    fn rejects_collaboration_policy_overrides_in_split_and_equals_forms() {
        const SESSION_ID: &str = "32f64a93-11f2-4f50-bbc1-56fe3025b8fb";
        let reserved_with_values = [
            "--resume",
            "--load",
            "--continue",
            "--session-id",
            "--fork-session",
            "--cwd",
            "--worktree",
            "--worktree-ref",
            "--ref",
            "--restore-code",
            "--rules",
            "--append-system-prompt",
            "--system-prompt-override",
            "--system-prompt",
            "--allow",
            "--allowedTools",
            "--deny",
            "--disallowedTools",
            "--tools",
            "--disallowed-tools",
            "--agent",
            "--agents",
            "--single",
            "--prompt-file",
            "--prompt-json",
            "--output-format",
            "--json-schema",
            "--max-turns",
        ];

        for option in reserved_with_values {
            for suffix in [None, Some("=override")] {
                let argument = format!("{option}{}", suffix.unwrap_or_default());
                let mut args = vec!["--resume".to_string(), SESSION_ID.to_string(), argument];
                if suffix.is_none() {
                    args.push("override".to_string());
                }
                let error = extract_user_resume_args(&args, SESSION_ID)
                    .expect_err("collaboration policy override must be rejected");
                assert!(
                    error.contains(option),
                    "unexpected error for {option}: {error}"
                );
            }
        }

        for option in ["-p", "-r", "-s", "-w"] {
            for argument in [
                option.to_string(),
                format!("{option}=override"),
                format!("{option}override"),
            ] {
                let mut args = vec![
                    "--resume".to_string(),
                    SESSION_ID.to_string(),
                    argument.clone(),
                ];
                if argument == option {
                    args.push("override".to_string());
                }
                let error = extract_user_resume_args(&args, SESSION_ID)
                    .expect_err("reserved short option must be rejected");
                assert!(
                    error.contains(option),
                    "unexpected error for {argument}: {error}"
                );
            }
        }

        for argument in ["-c", "-cv", "-vc"] {
            let args = vec![
                "--resume".to_string(),
                SESSION_ID.to_string(),
                argument.to_string(),
            ];
            let error = extract_user_resume_args(&args, SESSION_ID)
                .expect_err("continue short option must be rejected");
            assert!(
                error.contains("-c"),
                "unexpected error for {argument}: {error}"
            );
        }
    }

    #[test]
    fn rejects_bare_positional_prompt_for_collaboration_resume() {
        const SESSION_ID: &str = "32f64a93-11f2-4f50-bbc1-56fe3025b8fb";
        let args = vec![
            "--resume".to_string(),
            SESSION_ID.to_string(),
            "--model".to_string(),
            "grok-build".to_string(),
            "start an unapproved bootstrap turn".to_string(),
        ];
        let error = extract_user_resume_args(&args, SESSION_ID)
            .expect_err("a positional prompt must not survive collaboration preparation");
        assert!(error.contains("positional prompt"));
    }

    #[test]
    fn master_switch_off_leaves_an_unbound_grok_resume_untouched() {
        let runtime = in_memory_runtime();
        let grok_session_id = "32f64a93-11f2-4f50-bbc1-56fe3025b8fb";
        let original_args = vec![
            "--resume".to_string(),
            grok_session_id.to_string(),
            "--no-alt-screen".to_string(),
            "--system-prompt=ordinary-user-prompt".to_string(),
            "--allowedTools=Bash(*)".to_string(),
            "--single=ordinary-user-turn".to_string(),
            "-rother-session".to_string(),
            "-wordinary-worktree".to_string(),
            "--cwd=/tmp/ordinary-workspace".to_string(),
            "--max-turns=1".to_string(),
            "ordinary positional prompt".to_string(),
        ];

        let prepared = runtime
            .prepare_grok_resume(
                "ordinary-terminal",
                "grok",
                &original_args,
                &std::env::current_dir().expect("current directory"),
                grok_session_id,
                &[],
            )
            .expect("an unbound session must keep the ordinary launch path");

        assert!(prepared.is_none());
        assert_eq!(
            original_args,
            vec![
                "--resume".to_string(),
                grok_session_id.to_string(),
                "--no-alt-screen".to_string(),
                "--system-prompt=ordinary-user-prompt".to_string(),
                "--allowedTools=Bash(*)".to_string(),
                "--single=ordinary-user-turn".to_string(),
                "-rother-session".to_string(),
                "-wordinary-worktree".to_string(),
                "--cwd=/tmp/ordinary-workspace".to_string(),
                "--max-turns=1".to_string(),
                "ordinary positional prompt".to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn global_reenable_blocks_suspended_old_generation_for_explicit_user_retry() {
        let mut fixture = lifecycle_fixture("global-reenable");
        let broker_root =
            PathBuf::from("/tmp").join(format!("teak-collab-reenable-{}", Uuid::new_v4()));
        create_private_directory(&broker_root).expect("private re-enable broker root");
        fixture.runtime.endpoint = broker_root.join("broker.sock");

        let leader_generation = 31;
        let worker_generation = 41;
        register_lifecycle_runtime(
            &fixture,
            "global-reenable-leader",
            leader_generation,
            ListenerState::Ready,
        );
        register_lifecycle_member_runtime(
            &fixture,
            &fixture.worker,
            &fixture.worker_binding,
            "global-reenable-worker",
            worker_generation,
            ListenerState::Ready,
            "global-reenable-worker-secret",
        );
        let queued = fixture
            .runtime
            .service()
            .send_message(
                &CallerIdentity {
                    member_id: fixture.leader.id.clone(),
                    terminal_generation: leader_generation,
                    token_epoch: leader_generation,
                    bearer_secret: Some(format!("lifecycle-secret-{leader_generation}")),
                },
                SendMessageRequest {
                    recipient_alias: fixture.worker.alias.clone(),
                    kind: MessageKind::Message,
                    task_id: None,
                    reply_to_message_id: None,
                    payload_text: "must not be redirected after global restart".into(),
                    request_id: "global-reenable-message".into(),
                    retry_of_message_id: None,
                    not_before: None,
                    expires_at: None,
                },
            )
            .expect("queue message before global disable");
        assert_eq!(queued.state, MessageState::Queued);
        assert_eq!(queued.recipient_generation, worker_generation);

        fixture
            .runtime
            .set_enabled(false)
            .expect("disable collaboration globally");
        assert_eq!(
            fixture
                .runtime
                .service()
                .store()
                .message(&queued.id)
                .expect("suspended message")
                .state,
            MessageState::Suspended
        );
        assert!(
            fixture
                .runtime
                .service()
                .store()
                .team(&fixture.team.id)
                .expect("team remains configured")
                .enabled
        );

        fixture
            .runtime
            .set_enabled(true)
            .expect("re-enable with recovery");
        assert!(fixture.runtime.is_broker_running());
        let blocked = fixture
            .runtime
            .service()
            .store()
            .message(&queued.id)
            .expect("revalidated old-generation message");
        assert_eq!(blocked.state, MessageState::Blocked);
        assert_eq!(blocked.recipient_generation, worker_generation);
        assert_eq!(blocked.blocked_reason.as_deref(), Some("stale_target"));
        assert_eq!(blocked.resolution_policy.as_deref(), Some("user_retry"));

        register_lifecycle_member_runtime(
            &fixture,
            &fixture.worker,
            &fixture.worker_binding,
            "global-reenable-worker-new",
            worker_generation + 1,
            ListenerState::Connecting,
            "global-reenable-worker-new-secret",
        );
        let still_blocked = fixture
            .runtime
            .service()
            .store()
            .message(&queued.id)
            .expect("old message remains explicit retry");
        assert_eq!(still_blocked.state, MessageState::Blocked);
        assert_eq!(still_blocked.recipient_generation, worker_generation);
        assert_eq!(
            still_blocked.resolution_policy.as_deref(),
            Some("user_retry")
        );

        fixture
            .runtime
            .set_enabled(false)
            .expect("stop re-enable test broker");
        std::fs::remove_dir_all(&broker_root).expect("remove re-enable broker root");
        std::fs::remove_dir_all(&fixture.workspace).expect("remove re-enable workspace");
    }

    #[test]
    fn rejects_a_collaboration_attach_when_the_native_session_is_live_elsewhere() {
        let grok_session_id = "32f64a93-11f2-4f50-bbc1-56fe3025b8fb";
        let same_terminal = vec![("same-terminal".to_string(), grok_session_id.to_string())];
        assert!(
            reject_other_live_grok_session("same-terminal", grok_session_id, &same_terminal)
                .is_ok()
        );

        let live = vec![
            ("same-terminal".to_string(), grok_session_id.to_string()),
            ("ordinary-terminal".to_string(), grok_session_id.to_string()),
        ];

        assert!(reject_other_live_grok_session("same-terminal", grok_session_id, &live).is_err());
        assert!(reject_other_live_grok_session(
            "same-terminal",
            "7aef2abf-206f-4476-a7a6-29626498ff90",
            &live,
        )
        .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn same_terminal_replacement_is_atomic_and_cleans_old_body_after_commit() {
        let leader_session_id = "32f64a93-11f2-4f50-bbc1-56fe3025b8fb".to_string();
        let worker_session_id = "7aef2abf-206f-4476-a7a6-29626498ff90".to_string();
        let mut fixture = lifecycle_fixture_with_sessions(
            "same-terminal-replace",
            leader_session_id.clone(),
            worker_session_id,
        );
        let private_root = PathBuf::from("/tmp").join(format!(
            "teak-collab-same-terminal-replace-{}",
            Uuid::new_v4()
        ));
        create_private_directory(&private_root).expect("private replacement root");
        fixture.runtime.endpoint = private_root.join("broker.sock");
        fixture.runtime.helper_bin_root = private_root.join("bin");
        fixture
            .runtime
            .start_broker_if_needed()
            .expect("replacement broker");
        let grok = write_supported_fake_grok(&private_root);

        let terminal_session_id = "same-terminal-replace";
        let old_generation = 81;
        let old_runtime = register_lifecycle_runtime(
            &fixture,
            terminal_session_id,
            old_generation,
            ListenerState::Ready,
        );
        let old_body = track_lifecycle_launch(&fixture, terminal_session_id, old_generation);
        let args = vec!["--resume".to_string(), leader_session_id.clone()];
        let live = vec![(terminal_session_id.to_string(), leader_session_id.clone())];

        fixture
            .runtime
            .service()
            .store()
            .lock()
            .expect("database lock")
            .execute_batch(
                "CREATE TEMP TRIGGER fail_runtime_replace
                 BEFORE INSERT ON collab_runtime
                 BEGIN SELECT RAISE(ABORT, 'forced replacement failure'); END;",
            )
            .expect("install deterministic replacement failure");
        assert!(fixture
            .runtime
            .prepare_grok_resume(
                terminal_session_id,
                grok.to_str().expect("UTF-8 fake Grok path"),
                &args,
                &fixture.workspace,
                &leader_session_id,
                &live,
            )
            .is_err());
        let still_active = fixture
            .runtime
            .service()
            .active_runtime_for_grok_session(&leader_session_id)
            .expect("active runtime query")
            .expect("old runtime preserved after rollback");
        assert_eq!(still_active.id, old_runtime.id);
        assert_eq!(still_active.revoked_at, None);
        let launches = fixture
            .runtime
            .active_launches
            .lock()
            .expect("old launch retained after rollback");
        let retained = launches
            .get(terminal_session_id)
            .expect("old launch remains registered");
        assert_eq!(retained.generation, old_generation);
        assert_eq!(retained.body_dir, old_body);
        assert!(old_body.is_dir());
        drop(launches);

        fixture
            .runtime
            .service()
            .store()
            .lock()
            .expect("database lock")
            .execute_batch("DROP TRIGGER fail_runtime_replace;")
            .expect("remove deterministic replacement failure");
        let prepared = fixture
            .runtime
            .prepare_grok_resume(
                terminal_session_id,
                grok.to_str().expect("UTF-8 fake Grok path"),
                &args,
                &fixture.workspace,
                &leader_session_id,
                &live,
            )
            .expect("same terminal replacement")
            .expect("collaboration launch");
        assert_ne!(prepared.generation, old_generation);
        let retired = fixture
            .runtime
            .service()
            .store()
            .runtime(&old_runtime.id)
            .expect("old runtime retired atomically");
        assert!(retired.revoked_at.is_some());
        let replacement = fixture
            .runtime
            .service()
            .active_runtime_for_grok_session(&leader_session_id)
            .expect("replacement runtime query")
            .expect("replacement runtime");
        assert_eq!(replacement.terminal_generation, prepared.generation);
        assert_eq!(replacement.terminal_session_id, terminal_session_id);
        let launches = fixture
            .runtime
            .active_launches
            .lock()
            .expect("replacement launch registry");
        let active = launches
            .get(terminal_session_id)
            .expect("replacement launch registered");
        assert_eq!(active.generation, prepared.generation);
        assert!(active.body_dir.is_dir());
        drop(launches);
        assert!(!old_body.exists());

        fixture
            .runtime
            .set_enabled(false)
            .expect("stop replacement broker");
        std::fs::remove_dir_all(&private_root).expect("remove replacement root");
        std::fs::remove_dir_all(&fixture.workspace).expect("remove replacement workspace");
    }

    #[test]
    fn pause_retires_ready_runtime_and_resume_requires_a_new_generation() {
        let fixture = lifecycle_fixture("pause");
        let old =
            register_lifecycle_runtime(&fixture, "pause-terminal-old", 11, ListenerState::Ready);
        register_lifecycle_member_runtime(
            &fixture,
            &fixture.worker,
            &fixture.worker_binding,
            "pause-worker-old",
            21,
            ListenerState::Ready,
            "pause-worker-old-secret",
        );
        let old_body = track_lifecycle_launch(&fixture, "pause-terminal-old", 11);
        let old_target = fixture
            .runtime
            .service()
            .send_message(
                &CallerIdentity {
                    member_id: fixture.leader.id.clone(),
                    terminal_generation: 11,
                    token_epoch: 11,
                    bearer_secret: Some("lifecycle-secret-11".into()),
                },
                SendMessageRequest {
                    recipient_alias: fixture.worker.alias.clone(),
                    kind: MessageKind::Message,
                    task_id: None,
                    reply_to_message_id: None,
                    payload_text: "must not cross pause generation".into(),
                    request_id: "pause-old-target".into(),
                    retry_of_message_id: None,
                    not_before: None,
                    expires_at: None,
                },
            )
            .expect("queue old pause target");
        assert_eq!(old_target.recipient_generation, 21);

        let paused =
            set_team_paused(fixture.runtime.service(), &fixture.team.id, true).expect("pause team");
        fixture
            .runtime
            .reconcile_lifecycle(&paused.lifecycle)
            .expect("reconcile pause");

        let retired = fixture
            .runtime
            .service()
            .store()
            .runtime(&old.id)
            .expect("retired runtime");
        assert!(retired.revoked_at.is_some());
        assert_eq!(retired.listener_state, ListenerState::Offline);
        assert_eq!(retired.runtime_state, RuntimeState::Exited);
        assert!(!old_body.exists());
        assert!(fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .is_empty());
        let blocked = fixture
            .runtime
            .service()
            .store()
            .message(&old_target.id)
            .expect("blocked pause target");
        assert_eq!(blocked.state, MessageState::Blocked);
        assert_eq!(blocked.blocked_reason.as_deref(), Some("stale_target"));
        assert_eq!(blocked.resolution_policy.as_deref(), Some("user_retry"));
        assert_eq!(blocked.recipient_generation, 21);

        let resumed = set_team_paused(fixture.runtime.service(), &fixture.team.id, false)
            .expect("resume team");
        let resumed_revision = resumed.value.revision;
        fixture
            .runtime
            .reconcile_lifecycle(&resumed.lifecycle)
            .expect("reconcile resume");
        let launch_plan = get_member_launch_plan(
            fixture.runtime.service(),
            &fixture.team.id,
            &fixture.leader.id,
            resumed_revision,
            true,
            &[],
        )
        .expect("launch plan after resume");
        assert_eq!(launch_plan.status, MemberLaunchStatusDto::ResumeAllowed);
        assert_eq!(launch_plan.runtime_generation, None);

        let old_live = vec![LiveTerminalSessionEntry {
            terminal_session_id: "pause-terminal-old".into(),
            tool: "grok".into(),
            native_session_id: Some(fixture.leader_binding.grok_session_id.clone()),
        }];
        let helper =
            HelperInvocation::hidden_subcommand(std::env::current_exe().expect("test executable"))
                .expect("helper");
        assert!(begin_bootstrap(
            fixture.runtime.service(),
            &helper,
            &fixture.team.id,
            &fixture.leader.id,
            "pause-terminal-old",
            11,
            true,
            &old_live,
        )
        .is_err());

        let replacement = register_lifecycle_runtime(
            &fixture,
            "pause-terminal-new",
            12,
            ListenerState::Connecting,
        );
        register_lifecycle_member_runtime(
            &fixture,
            &fixture.worker,
            &fixture.worker_binding,
            "pause-worker-new",
            22,
            ListenerState::Connecting,
            "pause-worker-new-secret",
        );
        assert_ne!(replacement.terminal_generation, old.terminal_generation);
        let still_blocked = fixture
            .runtime
            .service()
            .store()
            .message(&old_target.id)
            .expect("old message remains user retry");
        assert_eq!(still_blocked.state, MessageState::Blocked);
        assert_eq!(still_blocked.recipient_generation, 21);
        let replacement_live = vec![LiveTerminalSessionEntry {
            terminal_session_id: "pause-terminal-new".into(),
            tool: "grok".into(),
            native_session_id: Some(fixture.leader_binding.grok_session_id.clone()),
        }];
        let bootstrap = begin_bootstrap(
            fixture.runtime.service(),
            &helper,
            &fixture.team.id,
            &fixture.leader.id,
            "pause-terminal-new",
            12,
            true,
            &replacement_live,
        )
        .expect("replacement bootstrap");
        assert_eq!(bootstrap.status, BootstrapStatusDto::PromptRequired);

        std::fs::remove_dir_all(&fixture.workspace).expect("remove workspace");
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_revoke_notifies_listener_even_when_body_cleanup_fails() {
        let mut fixture = lifecycle_fixture("cleanup-notify");
        let private_root =
            PathBuf::from("/tmp").join(format!("teak-collab-cleanup-notify-{}", Uuid::new_v4()));
        create_private_directory(&private_root).expect("private cleanup-notify root");
        fixture.runtime.endpoint = private_root.join("broker.sock");
        fixture
            .runtime
            .start_broker_if_needed()
            .expect("cleanup-notify broker");
        let notifier = fixture
            .runtime
            .broker
            .lock()
            .expect("broker registry")
            .as_ref()
            .expect("running broker")
            .notifier();
        let sequence_before = notifier.test_sequence();

        let generation = 61;
        let active = register_lifecycle_runtime(
            &fixture,
            "cleanup-notify-terminal",
            generation,
            ListenerState::Ready,
        );
        let body_file = private_root.join("body-is-a-file");
        std::fs::write(&body_file, b"cleanup must report this invalid body path")
            .expect("invalid body file");
        fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .insert(
                "cleanup-notify-terminal".into(),
                ActiveLaunch {
                    team_id: fixture.team.id.clone(),
                    generation,
                    body_dir: body_file.clone(),
                },
            );

        let paused = set_team_paused(fixture.runtime.service(), &fixture.team.id, true)
            .expect("durably pause team");
        let cleanup_error = fixture
            .runtime
            .reconcile_lifecycle(&paused.lifecycle)
            .err()
            .expect("invalid body path must report cleanup failure");
        assert!(cleanup_error.contains("could not remove revoked collaboration body"));
        assert!(notifier.test_sequence() > sequence_before);
        let retired = fixture
            .runtime
            .service()
            .store()
            .runtime(&active.id)
            .expect("durably retired runtime");
        assert!(retired.revoked_at.is_some());
        assert_eq!(retired.listener_state, ListenerState::Offline);
        assert!(fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .is_empty());
        assert!(body_file.is_file());

        fixture
            .runtime
            .set_enabled(false)
            .expect("stop cleanup-notify broker");
        std::fs::remove_dir_all(&private_root).expect("remove cleanup-notify root");
        std::fs::remove_dir_all(&fixture.workspace).expect("remove cleanup-notify workspace");
    }

    #[cfg(unix)]
    #[test]
    fn revoke_persistence_failure_keeps_launch_for_fail_closed_retry() {
        let mut fixture = lifecycle_fixture("revoke-retry");
        let private_root =
            PathBuf::from("/tmp").join(format!("teak-collab-revoke-retry-{}", Uuid::new_v4()));
        create_private_directory(&private_root).expect("private revoke-retry root");
        fixture.runtime.endpoint = private_root.join("broker.sock");
        fixture
            .runtime
            .start_broker_if_needed()
            .expect("revoke-retry broker");
        assert!(fixture.runtime.is_broker_running());

        let terminal_session_id = "revoke-retry-terminal";
        let generation = 71;
        let active = register_lifecycle_runtime(
            &fixture,
            terminal_session_id,
            generation,
            ListenerState::Ready,
        );
        let body_dir = private_root.join("body");
        create_private_directory(&body_dir).expect("revoke-retry body");
        fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .insert(
                terminal_session_id.into(),
                ActiveLaunch {
                    team_id: fixture.team.id.clone(),
                    generation,
                    body_dir: body_dir.clone(),
                },
            );

        fixture
            .runtime
            .service()
            .store()
            .lock()
            .expect("database lock")
            .execute_batch(
                "CREATE TEMP TRIGGER fail_runtime_revoke
                 BEFORE UPDATE OF revoked_at ON collab_runtime
                 WHEN NEW.revoked_at IS NOT NULL
                 BEGIN SELECT RAISE(ABORT, 'forced runtime revoke failure'); END;",
            )
            .expect("install deterministic revoke failure");

        assert!(!fixture.runtime.revoke_terminal_generation(
            terminal_session_id,
            generation,
            "forced_failure"
        ));
        assert!(!fixture.runtime.is_broker_running());
        let persisted = fixture
            .runtime
            .service()
            .store()
            .runtime(&active.id)
            .expect("runtime remains active after failed revoke");
        assert_eq!(persisted.revoked_at, None);
        let launches = fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry retained");
        let retained = launches
            .get(terminal_session_id)
            .expect("failed revoke retains retry handle");
        assert_eq!(retained.generation, generation);
        assert_eq!(retained.body_dir, body_dir);
        assert!(retained.body_dir.is_dir());
        drop(launches);

        fixture
            .runtime
            .service()
            .store()
            .lock()
            .expect("database lock")
            .execute_batch("DROP TRIGGER fail_runtime_revoke;")
            .expect("remove deterministic revoke failure");
        assert!(fixture.runtime.revoke_terminal_generation(
            terminal_session_id,
            generation,
            "retry_succeeded"
        ));
        assert!(fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry cleaned")
            .get(terminal_session_id)
            .is_none());
        assert!(!body_dir.exists());
        let retired = fixture
            .runtime
            .service()
            .store()
            .runtime(&active.id)
            .expect("runtime retired after retry");
        assert!(retired.revoked_at.is_some());

        std::fs::remove_dir_all(&private_root).expect("remove revoke-retry root");
        std::fs::remove_dir_all(&fixture.workspace).expect("remove revoke-retry workspace");
    }

    #[test]
    fn archive_retires_ready_runtime_and_never_reuses_its_generation() {
        let fixture = lifecycle_fixture("archive");
        let old =
            register_lifecycle_runtime(&fixture, "archive-terminal-old", 21, ListenerState::Ready);
        let old_body = track_lifecycle_launch(&fixture, "archive-terminal-old", 21);

        let archived =
            archive_team(fixture.runtime.service(), &fixture.team.id).expect("archive team");
        fixture
            .runtime
            .reconcile_lifecycle(&archived.lifecycle)
            .expect("reconcile archive");

        let retired = fixture
            .runtime
            .service()
            .store()
            .runtime(&old.id)
            .expect("retired runtime");
        assert!(retired.revoked_at.is_some());
        assert_eq!(retired.listener_state, ListenerState::Offline);
        assert_eq!(retired.runtime_state, RuntimeState::Exited);
        assert!(!old_body.exists());
        assert!(fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .is_empty());
        assert!(fixture
            .runtime
            .service()
            .active_runtime_for_grok_session(&fixture.leader_binding.grok_session_id)
            .expect("active runtime query")
            .is_none());
        assert!(set_team_paused(fixture.runtime.service(), &fixture.team.id, false).is_err());

        let archived_live = vec![LiveTerminalSessionEntry {
            terminal_session_id: "archive-terminal-old".into(),
            tool: "grok".into(),
            native_session_id: Some(fixture.leader_binding.grok_session_id.clone()),
        }];
        let helper =
            HelperInvocation::hidden_subcommand(std::env::current_exe().expect("test executable"))
                .expect("helper");
        assert!(begin_bootstrap(
            fixture.runtime.service(),
            &helper,
            &fixture.team.id,
            &fixture.leader.id,
            "archive-terminal-old",
            21,
            true,
            &archived_live,
        )
        .is_err());

        std::fs::remove_dir_all(&fixture.workspace).expect("remove workspace");
    }

    #[test]
    fn native_session_change_retires_only_the_matching_collaboration_generation() {
        let fixture = lifecycle_fixture("native-session-change");
        let runtime = register_lifecycle_runtime(
            &fixture,
            "native-session-terminal",
            31,
            ListenerState::Ready,
        );
        register_lifecycle_member_runtime(
            &fixture,
            &fixture.worker,
            &fixture.worker_binding,
            "native-session-worker",
            41,
            ListenerState::Ready,
            "native-session-worker-secret",
        );
        let body_dir = track_lifecycle_launch(&fixture, "native-session-terminal", 31);
        let old_target = fixture
            .runtime
            .service()
            .send_message(
                &CallerIdentity {
                    member_id: fixture.worker.id.clone(),
                    terminal_generation: 41,
                    token_epoch: 41,
                    bearer_secret: Some("native-session-worker-secret".into()),
                },
                SendMessageRequest {
                    recipient_alias: fixture.leader.alias.clone(),
                    kind: MessageKind::Message,
                    task_id: None,
                    reply_to_message_id: None,
                    payload_text: "must remain bound to native session A generation".into(),
                    request_id: "native-session-old-target".into(),
                    retry_of_message_id: None,
                    not_before: None,
                    expires_at: None,
                },
            )
            .expect("queue message for native session A");
        assert_eq!(old_target.recipient_generation, 31);

        assert!(!fixture.runtime.observe_terminal_native_session(
            "native-session-terminal",
            31,
            &fixture.leader_binding.grok_session_id,
            &fixture.leader_binding.grok_session_id,
        ));
        assert!(fixture.runtime.observe_terminal_native_session(
            "native-session-terminal",
            31,
            &fixture.leader_binding.grok_session_id,
            "different-native-session-id",
        ));

        let retired = fixture
            .runtime
            .service()
            .store()
            .runtime(&runtime.id)
            .expect("retired runtime");
        assert!(retired.revoked_at.is_some());
        assert_eq!(retired.listener_state, ListenerState::Offline);
        assert_eq!(retired.runtime_state, RuntimeState::Exited);
        assert!(!body_dir.exists());
        assert!(fixture
            .runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .is_empty());
        let blocked = fixture
            .runtime
            .service()
            .store()
            .message(&old_target.id)
            .expect("old native-session target");
        assert_eq!(blocked.state, MessageState::Blocked);
        assert_eq!(blocked.blocked_reason.as_deref(), Some("stale_target"));
        assert_eq!(blocked.resolution_policy.as_deref(), Some("user_retry"));
        assert_eq!(blocked.recipient_generation, 31);
        assert!(!fixture
            .runtime
            .observe_terminal_activity("native-session-terminal", RuntimeState::Busy)
            .expect("same PTY is ordinary after native session change"));

        std::fs::remove_dir_all(&fixture.workspace).expect("remove workspace");
    }

    #[test]
    fn terminal_activity_uses_backend_generation_and_emits_ready_edges_only() {
        let fixture = lifecycle_fixture("terminal-activity");
        let terminal_session_id = "terminal-activity-worker";
        let generation = 37;
        let runtime = register_lifecycle_member_runtime(
            &fixture,
            &fixture.worker,
            &fixture.worker_binding,
            terminal_session_id,
            generation,
            ListenerState::Connecting,
            "terminal-activity-worker-secret",
        );
        track_lifecycle_launch(&fixture, terminal_session_id, generation);

        assert!(!fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::Busy)
            .expect("connecting listener is not observable"));
        assert_eq!(
            fixture
                .runtime
                .service()
                .store()
                .runtime(&runtime.id)
                .expect("connecting runtime")
                .runtime_state,
            RuntimeState::Unknown
        );

        let caller = CallerIdentity {
            member_id: fixture.worker.id.clone(),
            terminal_generation: generation,
            token_epoch: generation,
            bearer_secret: Some("terminal-activity-worker-secret".into()),
        };
        fixture
            .runtime
            .service()
            .update_runtime_state(&caller, ListenerState::Ready, RuntimeState::Idle)
            .expect("listener ready");
        let baseline = fixture
            .runtime
            .service()
            .store()
            .events_after(0, 10_000)
            .expect("baseline events")
            .last()
            .map(|event| event.sequence)
            .unwrap_or(0);

        assert!(fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::Busy)
            .expect("busy edge"));
        assert!(!fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::Busy)
            .expect("duplicate busy edge"));
        assert!(fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::WaitingUser)
            .expect("waiting-user edge"));
        assert!(fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::Idle)
            .expect("idle edge"));
        assert!(fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::Unknown)
            .expect_err("unknown is not a terminal activity state")
            .contains("idle, busy, or waiting_user"));

        let edges = fixture
            .runtime
            .service()
            .store()
            .events_after(baseline, 10)
            .expect("terminal activity events");
        assert_eq!(
            edges
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["member_busy", "member_waiting_user", "member_ready"]
        );
        assert!(edges.iter().all(|event| {
            event.aggregate_type == "runtime"
                && event.aggregate_id == runtime.id
                && event.actor_type == ActorType::Broker
                && event.actor_member_id.as_deref() == Some(fixture.worker.id.as_str())
                && event.redacted_metadata_json == r#"{"source":"terminal_activity"}"#
        }));

        assert!(fixture.runtime.revoke_terminal_generation(
            terminal_session_id,
            generation,
            "test_complete",
        ));
        assert!(!fixture
            .runtime
            .observe_terminal_activity(terminal_session_id, RuntimeState::Busy)
            .expect("retired terminal is ordinary"));
        assert!(!fixture
            .runtime
            .observe_terminal_activity("ordinary-terminal", RuntimeState::Busy)
            .expect("ordinary terminal is ignored"));

        std::fs::remove_dir_all(&fixture.workspace).expect("remove workspace");
    }

    #[test]
    fn late_old_generation_exit_preserves_replacement_launch() {
        let runtime = in_memory_runtime();
        let service = runtime.service.clone();
        let terminal_session_id = "restarted-terminal";
        let grok_session_id = "grok-generation-cas";
        let workspace = "workspace-generation-cas";

        service.set_global_enabled(true).expect("global enable");
        let team = service
            .create_team(NewTeam {
                name: "Generation CAS".into(),
                workspace_fingerprint: workspace.into(),
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
        let worker = service
            .add_member(NewMember {
                team_id: team.id.clone(),
                alias: "worker-a".into(),
                display_name: "Worker A".into(),
                avatar_id: "worker-1".into(),
                role: Role::Worker,
                enabled: true,
            })
            .expect("worker");
        let leader_binding = service
            .bind_member(NewBinding {
                member_id: leader.id.clone(),
                grok_session_id: grok_session_id.into(),
            })
            .expect("leader binding");
        service
            .bind_member(NewBinding {
                member_id: worker.id,
                grok_session_id: "grok-generation-worker".into(),
            })
            .expect("worker binding");
        service.install_v1_acl(&team.id).expect("ACL");
        service
            .set_team_enabled(&team.id, true)
            .expect("team enable");

        for generation in [1, 2] {
            service
                .register_runtime(NewRuntime {
                    member_id: leader.id.clone(),
                    binding_id: leader_binding.id.clone(),
                    terminal_session_id: terminal_session_id.into(),
                    terminal_generation: generation,
                    observed_grok_session_id: grok_session_id.into(),
                    process_id: None,
                    auth_method: AuthMethod::EnvBearer,
                    bearer_secret: Some(format!("secret-{generation}")),
                    token_epoch: generation,
                    attested_workspace_fingerprint: workspace.into(),
                    grok_version: "test".into(),
                    helper_protocol_version: PROTOCOL_VERSION.to_string(),
                    capability_probe_result: "ok".into(),
                    listener_state: ListenerState::Connecting,
                    runtime_state: RuntimeState::Unknown,
                })
                .expect("runtime generation");
        }

        let replacement_body = runtime.body_root.join("replacement-generation-2");
        create_private_directory(&replacement_body).expect("replacement body");
        runtime
            .active_launches
            .lock()
            .expect("launch registry")
            .insert(
                terminal_session_id.into(),
                ActiveLaunch {
                    team_id: team.id.clone(),
                    generation: 2,
                    body_dir: replacement_body.clone(),
                },
            );

        assert!(!runtime.revoke_terminal_generation(terminal_session_id, 1, "late_process_exit"));

        let active_runtime = service
            .active_runtime_for_grok_session(grok_session_id)
            .expect("active runtime query")
            .expect("replacement runtime");
        assert_eq!(active_runtime.terminal_generation, 2);
        assert_eq!(active_runtime.revoked_at, None);
        let launches = runtime.active_launches.lock().expect("launch registry");
        let active_launch = launches
            .get(terminal_session_id)
            .expect("replacement launch");
        assert_eq!(active_launch.generation, 2);
        assert_eq!(active_launch.body_dir, replacement_body);
        assert!(active_launch.body_dir.is_dir());
    }
}
