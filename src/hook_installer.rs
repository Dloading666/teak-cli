// Coffee CLI Hook Installer
//
// At app launch, ensure all three integrated CLIs are wired to the dynamic
// island status bus:
//
// The forwarder is the Coffee CLI binary itself (`<exe> __hook` /
// `<exe> __codex-notify`, implemented in hook_forwarder.rs) — NOT a Python
// script. The Python forwarders failed on Windows machines without Python
// (`python` hit the MS Store alias stub → "python not found" hook errors in
// Claude's transcript); a native subcommand on the always-present binary
// removes the interpreter dependency entirely. The .py files are still
// dropped under ~/.coffee-cli/hooks/ as protocol-reference copies only.
//
//   Claude Code
//     1. ~/.claude/settings.json — registers `<exe> __hook` on 5 events
//     2. ~/.claude/settings.local.json — stale entries from v1.8.5 stripped
//     3. ~/.coffee-cli/hooks/coffee-cli-hook.py — reference copy (unused)
//
//   Codex
//     1. ~/.codex/config.toml — `notify = ["<exe>", "__codex-notify"]` line,
//        only added if there's no top-level `notify` already (don't clobber
//        user config). It's global to all Codex sessions but no-ops when the
//        COFFEE_CLI_* env vars are absent.
//     2. ~/.coffee-cli/hooks/coffee-cli-codex-notify.py — reference copy
//
//   OpenCode
//     1. ~/.config/opencode/plugins/coffee-cli-island.js — auto-loaded by
//        OpenCode/Bun on every session. Same env-var no-op gate as Codex.
//
//   Hermes Agent (paths are HERMES_HOME-relative — `%LOCALAPPDATA%\hermes`
//   on Windows, `~/.hermes` elsewhere; see tools/hermes.rs::hermes_home)
//     1. <HERMES_HOME>/plugins/coffee-cli-status/__init__.py — Python
//        plugin registering hooks for pre_llm_call / pre_tool_call /
//        pre_approval_request / on_session_start / etc.
//     2. <HERMES_HOME>/plugins/coffee-cli-status/plugin.yaml — manifest
//     3. `hermes plugins enable coffee-cli-status` — Hermes' opt-in CLI
//        gate (third-party plugins don't load until allow-listed in
//        <HERMES_HOME>/config.yaml). We let Hermes' own command do the
//        YAML edit so we don't have to YAML-round-trip user config.
//
// IMPORTANT — Claude event list discipline:
// Claude Code rejects the *entire* hooks block if it contains an unknown
// event name (cf. vibe-notch source comment, anthropics/claude-code#6305).
// The 5 events below are the proven-working set as of Claude Code v2.x.
// Permission-prompt detection rides on `Notification` (subtype
// `permission_prompt`), NOT a separate `PermissionRequest` event — that
// name silently invalidated the whole config in Coffee CLI ≤ v1.8.5.
//
// Errors are logged, never fatal — a broken installer must not prevent
// Coffee CLI from starting.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const HOOK_SCRIPT: &str = include_str!("../scripts/coffee-cli-hook.py");
const SCRIPT_FILENAME: &str = "coffee-cli-hook.py";

const CODEX_NOTIFY_SCRIPT: &str = include_str!("../scripts/coffee-cli-codex-notify.py");
const CODEX_NOTIFY_FILENAME: &str = "coffee-cli-codex-notify.py";

/// Argv markers for the native forwarder built into the Coffee CLI binary
/// (see hook_forwarder.rs). The hook command is now `<exe> __hook` /
/// `<exe> __codex-notify` — no Python. These tokens double as the
/// "is this our entry?" sentinel so re-installs are idempotent and old
/// Python-based entries get migrated in place.
const HOOK_SUBCOMMAND: &str = "__hook";
const CODEX_NOTIFY_SUBCOMMAND: &str = "__codex-notify";

const OPENCODE_PLUGIN_SCRIPT: &str = include_str!("../scripts/coffee-cli-opencode-plugin.js");
const OPENCODE_PLUGIN_FILENAME: &str = "coffee-cli-island.js";

const HERMES_PLUGIN_SCRIPT: &str = include_str!("../scripts/coffee-cli-hermes-plugin.py");
const HERMES_PLUGIN_NAME: &str = "coffee-cli-status";
const HERMES_PLUGIN_YAML: &str = "name: coffee-cli-status\nversion: \"1.0\"\ndescription: Forwards Hermes session lifecycle events to Coffee CLI's tab status bus over local TCP. No-ops outside Coffee CLI.\n";

/// Events Coffee CLI listens for. Mirrors vibe-notch (ClaudeIsland)'s
/// proven-working set; do not add unknown event names — Claude Code drops
/// the whole hooks block on first unrecognized key.
const EVENTS: &[&str] = &[
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
];

/// Events where Claude expects a `matcher` regex (tool name filter).
const EVENTS_WITH_MATCHER: &[&str] = &["PreToolUse", "PostToolUse"];

pub fn install_all() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("[hook-installer] no home dir — skipping");
            return;
        }
    };

    for tool in crate::tools::TOOLS {
        if tool.has_hook_surface {
            dispatch_install(tool, &home);
        }
    }
}

/// Install hook(s) for a single tool. Called from the launchpad's
/// window-focus rescan when a CLI flips from not-installed → installed,
/// so users who install a CLI while Coffee CLI is running don't have
/// to restart to get tab status indicators. Idempotent.
pub fn install_for_tool(tool: &str) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let Some(descriptor) = crate::tools::find(tool) else { return };
    if !descriptor.has_hook_surface {
        return;
    }
    dispatch_install(descriptor, &home);
}

/// Per-tool installer dispatch. Gates on `binary_on_path` (we don't
/// materialize `~/.<tool>/` for tools the user hasn't installed) then
/// runs the tool's bespoke config-patching shape. The unknown-id arm
/// is reachable only when a registry entry declares a hook surface
/// but no installer arm exists yet — that's a build-time omission
/// worth a log line.
fn dispatch_install(tool: &crate::tools::ToolDescriptor, home: &Path) {
    if !crate::server::binary_on_path(tool.binary_name) {
        return;
    }
    match tool.id {
        "claude" => install_claude(home),
        "codex" => install_codex(home),
        "opencode" => {
            install_opencode(home);
            ensure_opencode_tui_theme_default(home, "opencode");
        }
        // MiMo Code (Xiaomi OpenCode fork) ships the same opaque #000 default
        // canvas, so it needs the identical tui.json transparency override. It
        // does NOT get the OpenCode island plugin — only the theme write.
        "mimocode" => ensure_opencode_tui_theme_default(home, "mimocode"),
        "hermes" => install_hermes(home),
        other => {
            eprintln!(
                "[hook-installer] tool '{}' declares a hook surface but has no installer — \
                 add an arm to dispatch_install",
                other
            );
        }
    }
}

/// TUI theme we default OpenCode-family tools (OpenCode, MiMo Code) into.
/// `lucent-orng` sets all four background slots (background / backgroundPanel
/// / backgroundElement / backgroundMenu) to `"transparent"`, which is what
/// makes Coffee CLI's terminal bg — and the Glass theme's wallpaper blur —
/// actually visible behind the TUI. Confirmed working for OpenCode 2026-05-09;
/// MiMo Code is a Xiaomi OpenCode fork that ships the same bundled themes and
/// the same opaque #000 default canvas, so it needs the identical override.
const OPENCODE_DEFAULT_THEME: &str = "lucent-orng";

/// Theme value Coffee CLI used to write into tui.json before we discovered
/// `lucent-orng` actually delivers transparency. `system` *generates* a
/// transparent bg in source, but the panel slots still resolve to opaque
/// shades of palette[0], so OpenCode renders an almost-black canvas. We
/// migrate any tui.json we previously stamped with `system` to the new
/// default; user-set themes (anything other than `system`) are left alone.
const OPENCODE_LEGACY_THEME: &str = "system";

fn install_claude(home: &Path) {
    // Keep a protocol-reference copy of the Python forwarder co-located with
    // the other tools' debug copies. It's no longer the registered command
    // (the native binary is — see below), so a write failure is non-fatal.
    if let Err(e) = write_script(home) {
        eprintln!("[hook-installer] failed to write claude hook reference copy: {}", e);
    }

    // The hook command is the Coffee CLI binary itself: `<exe> __hook`. No
    // interpreter dependency — this is what fixes the "python not found"
    // hook error on Windows machines without Python.
    let command = match claude_hook_command() {
        Some(c) => c,
        None => {
            eprintln!("[hook-installer] current_exe() failed — cannot install claude hook");
            return;
        }
    };

    // Primary target: ~/.claude/settings.json. Local-settings.json was
    // tried in v1.8.5 but hooks declared there fire unreliably under Claude
    // Code v2.x (workspace-trust gate, cf. anthropics/claude-code#11519).
    let primary = home.join(".claude").join("settings.json");
    if let Err(e) = patch_settings(&primary, &command) {
        eprintln!(
            "[hook-installer] failed to patch {}: {}",
            primary.display(),
            e
        );
    }

    // Strip stale Coffee CLI entries from settings.local.json (v1.8.5 wrote
    // there). Leaves user's other keys untouched. Without this cleanup the
    // hook would fire twice per event on machines that ran v1.8.5.
    let local = home.join(".claude").join("settings.local.json");
    if local.exists() {
        if let Err(e) = strip_coffee_hooks(&local) {
            eprintln!(
                "[hook-installer] failed to clean {}: {}",
                local.display(),
                e
            );
        }
    }
}

/// Codex notify forwarder — registers `notify = ["<exe>", "__codex-notify"]`
/// in ~/.codex/config.toml if (and only if) the user doesn't already have a
/// top-level notify. The forwarder is the Coffee CLI binary itself (no
/// Python). We never overwrite a user's custom notify command — too high a
/// risk of stomping on their setup.
fn install_codex(home: &Path) {
    // Protocol-reference copy alongside the other forwarders' debug copies.
    // No longer the registered command, so a write failure is non-fatal.
    if let Err(e) = write_aux_script(home, CODEX_NOTIFY_FILENAME, CODEX_NOTIFY_SCRIPT) {
        eprintln!("[hook-installer] failed to write codex notify reference copy: {}", e);
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[hook-installer] current_exe() failed — cannot install codex notify: {}", e);
            return;
        }
    };

    let config_path = home.join(".codex").join("config.toml");
    if let Err(e) = patch_codex_config(&config_path, &exe) {
        eprintln!(
            "[hook-installer] failed to patch {}: {}",
            config_path.display(),
            e
        );
    }
}

/// OpenCode plugin — written directly to ~/.config/opencode/plugins/ where
/// OpenCode auto-discovers it on session start. No config file edits needed.
/// We also keep a copy at ~/.coffee-cli/hooks/ so the source is co-located
/// with the other forwarders and easy to find when debugging.
fn install_opencode(home: &Path) {
    if let Err(e) = write_aux_script(home, OPENCODE_PLUGIN_FILENAME, OPENCODE_PLUGIN_SCRIPT) {
        eprintln!("[hook-installer] failed to write opencode plugin: {}", e);
        return;
    }

    let plugin_dir = home.join(".config").join("opencode").join("plugins");
    if let Err(e) = fs::create_dir_all(&plugin_dir) {
        eprintln!(
            "[hook-installer] failed to create {}: {}",
            plugin_dir.display(),
            e
        );
        return;
    }
    let plugin_path = plugin_dir.join(OPENCODE_PLUGIN_FILENAME);
    if let Err(e) = fs::write(&plugin_path, OPENCODE_PLUGIN_SCRIPT) {
        eprintln!(
            "[hook-installer] failed to write {}: {}",
            plugin_path.display(),
            e
        );
    }
}

/// Hermes Agent plugin — drop a 2-file Python plugin into
/// <HERMES_HOME>/plugins/coffee-cli-status/, then ask Hermes itself to
/// enable it via `hermes plugins enable coffee-cli-status`. Hermes
/// general plugins are opt-in by default (third-party code doesn't
/// run until allow-listed in <HERMES_HOME>/config.yaml), and shelling
/// out to Hermes' own CLI is safer than us round-tripping the user's
/// config.yaml — comments and key ordering survive intact.
///
/// `home` here is only used for the debug-copy under `~/.coffee-cli/`;
/// Hermes's plugin dir lives under `hermes_home()` because Windows
/// puts it at `%LOCALAPPDATA%\hermes\plugins\` (not `~/.hermes\plugins\`).
///
/// Idempotent: if the plugin is already enabled, `hermes plugins
/// enable` is a no-op. Errors are logged, never fatal.
fn install_hermes(home: &Path) {
    let plugin_dir = crate::tools::hermes::hermes_home()
        .join("plugins")
        .join(HERMES_PLUGIN_NAME);
    if let Err(e) = fs::create_dir_all(&plugin_dir) {
        eprintln!(
            "[hook-installer] failed to create {}: {}",
            plugin_dir.display(),
            e
        );
        return;
    }

    let init_path = plugin_dir.join("__init__.py");
    if let Err(e) = fs::write(&init_path, HERMES_PLUGIN_SCRIPT) {
        eprintln!(
            "[hook-installer] failed to write {}: {}",
            init_path.display(),
            e
        );
        return;
    }

    let manifest_path = plugin_dir.join("plugin.yaml");
    if let Err(e) = fs::write(&manifest_path, HERMES_PLUGIN_YAML) {
        eprintln!(
            "[hook-installer] failed to write {}: {}",
            manifest_path.display(),
            e
        );
        return;
    }

    // Also keep a debug copy under ~/.coffee-cli/hooks/ so the source is
    // co-located with the other forwarders for grep-friendly debugging.
    let _ = write_aux_script(
        home,
        "coffee-cli-hermes-plugin.py",
        HERMES_PLUGIN_SCRIPT,
    );

    // Hermes' allow-list gate. We invoke `hermes plugins enable
    // coffee-cli-status` rather than editing config.yaml ourselves —
    // Hermes' own command knows the canonical YAML shape and won't clobber
    // the user's comments / quoted strings / anchor references. The call
    // is idempotent: running it twice does not duplicate the entry.
    use std::process::Command;
    let mut cmd = Command::new("hermes");
    cmd.args(["plugins", "enable", HERMES_PLUGIN_NAME]);
    // CREATE_NO_WINDOW (0x08000000): install_all() runs in Tauri's setup hook
    // every launch, so without this flag spawning the `hermes` CLI flashes a
    // console window on Windows at startup. No-op on other platforms.
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    match cmd.output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            eprintln!(
                "[hook-installer] `hermes plugins enable {}` exited {} — \
                 user may need to enable it manually via `hermes plugins`",
                HERMES_PLUGIN_NAME,
                out.status,
            );
        }
        Err(e) => {
            eprintln!(
                "[hook-installer] failed to run `hermes plugins enable`: {} \
                 — user may need to enable it manually via `hermes plugins`",
                e,
            );
        }
    }
}

/// Ensure ~/.config/<config_subdir>/tui.json has `"theme": "lucent-orng"` so
/// the OpenCode-family TUI's four bg slots resolve to "transparent" — which is
/// what actually lets Coffee CLI's terminal bg (and the Glass theme's wallpaper
/// blur) show through. Without this the TUI picks its bundled opaque theme that
/// paints a #000 canvas no terminal setting can override. Shared by OpenCode
/// (`opencode`) and its Xiaomi fork MiMo Code (`mimocode`).
///
/// Policy:
///   - File missing                              → create with default theme.
///   - File exists, no `theme`                   → add default theme.
///   - File exists, `theme = "system"`           → migrate (we wrote that
///                                                 ourselves before realising
///                                                 it doesn't actually deliver
///                                                 transparency in practice).
///   - File exists, `theme = anything else`      → leave alone.
///   - File unparseable                          → leave alone.
///
/// All failures are logged, never fatal.
fn ensure_opencode_tui_theme_default(home: &Path, config_subdir: &str) {
    let config_dir = home.join(".config").join(config_subdir);
    let tui_path = config_dir.join("tui.json");

    if let Err(e) = fs::create_dir_all(&config_dir) {
        eprintln!(
            "[hook-installer] failed to create {}: {}",
            config_dir.display(),
            e
        );
        return;
    }

    if !tui_path.exists() {
        let initial = json!({
            "$schema": "https://opencode.ai/tui.json",
            "theme": OPENCODE_DEFAULT_THEME,
        });
        let body = match serde_json::to_string_pretty(&initial) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[hook-installer] tui.json serialize failed: {}", e);
                return;
            }
        };
        if let Err(e) = fs::write(&tui_path, body) {
            eprintln!(
                "[hook-installer] failed to write {}: {}",
                tui_path.display(),
                e
            );
        }
        return;
    }

    let text = match fs::read_to_string(&tui_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[hook-installer] read {} failed: {}", tui_path.display(), e);
            return;
        }
    };

    let mut root: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return, // malformed user file — don't touch
    };
    let Some(obj) = root.as_object_mut() else { return };
    let needs_write = match obj.get("theme") {
        None => true,
        Some(Value::String(s)) if s == OPENCODE_LEGACY_THEME => true,
        _ => false, // user (or our new default) has a non-legacy theme set — respect it
    };
    if !needs_write {
        return;
    }
    obj.insert(
        "theme".to_string(),
        Value::String(OPENCODE_DEFAULT_THEME.to_string()),
    );
    if !obj.contains_key("$schema") {
        obj.insert(
            "$schema".to_string(),
            Value::String("https://opencode.ai/tui.json".to_string()),
        );
    }

    let body = match serde_json::to_string_pretty(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[hook-installer] tui.json reserialize failed: {}", e);
            return;
        }
    };
    if let Err(e) = fs::write(&tui_path, body) {
        eprintln!(
            "[hook-installer] failed to update {}: {}",
            tui_path.display(),
            e
        );
    }
}

/// Remove every Coffee CLI hook entry from `path` without touching any other
/// user-owned key. Used to clean up after the v1.8.5 settings.local.json
/// install location.
fn strip_coffee_hooks(path: &Path) -> anyhow::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut root: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(()), // unparseable user file — leave it alone
    };
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(());
    };

    let mut empty_events = Vec::new();
    for (event, slot) in hooks.iter_mut() {
        if let Some(arr) = slot.as_array_mut() {
            arr.retain(|e| !is_coffee_entry(e));
            if arr.is_empty() {
                empty_events.push(event.clone());
            }
        }
    }
    for k in empty_events {
        hooks.remove(&k);
    }

    // If the hooks object is now fully empty, remove the key itself rather
    // than leaving an empty `"hooks": {}` artifact.
    let hooks_empty = root
        .get("hooks")
        .and_then(|h| h.as_object())
        .map(|o| o.is_empty())
        .unwrap_or(false);
    if hooks_empty {
        if let Some(obj) = root.as_object_mut() {
            obj.remove("hooks");
        }
    }

    fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn write_script(home: &Path) -> anyhow::Result<PathBuf> {
    write_aux_script(home, SCRIPT_FILENAME, HOOK_SCRIPT)
}

/// Generic helper: write `contents` to ~/.coffee-cli/hooks/<filename>,
/// chmod 755 on Unix, return the absolute path.
fn write_aux_script(home: &Path, filename: &str, contents: &str) -> anyhow::Result<PathBuf> {
    let dir = home.join(".coffee-cli").join("hooks");
    fs::create_dir_all(&dir)?;
    let path = dir.join(filename);
    fs::write(&path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    Ok(path)
}

/// Add a `notify = ["<exe>", "__codex-notify"]` line to ~/.codex/config.toml
/// when safe. Three cases, matched in order:
///   1. File doesn't exist or is empty → create it with our notify line.
///   2. File contains a top-level notify already pointing at our forwarder
///      (the native subcommand, or the legacy Python script) → rewrite it to
///      the current absolute exe path so an upgrade or moved $HOME doesn't
///      break the hook, and migrate the legacy Python form in place.
///   3. File contains a top-level notify pointing elsewhere → leave it alone
///      and log a warning. Never overwrite a user's custom notify command.
///
/// "Top-level" means before the first `[section]` header. A `notify` entry
/// inside a `[section]` is a different key entirely (e.g. `[mcp.notify]`)
/// and we don't touch it.
fn patch_codex_config(path: &Path, exe: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let exe_str = exe.display().to_string();
    // Codex execs the notify argv directly (no shell), so the raw path is
    // correct. TOML strings escape backslashes and quotes — Windows paths
    // have plenty of backslashes, so escape them to parse cleanly.
    let escaped = exe_str.replace('\\', "\\\\").replace('"', "\\\"");
    let new_line = format!("notify = [\"{}\", \"{}\"]", escaped, CODEX_NOTIFY_SUBCOMMAND);

    let existing = if path.exists() {
        fs::read_to_string(path).unwrap_or_default()
    } else {
        String::new()
    };

    if existing.trim().is_empty() {
        let header = "# Coffee CLI registered this notify command for the dynamic-island\n# status indicator. Safe to remove if you don't use Coffee CLI — the\n# script no-ops when COFFEE_CLI_* env vars aren't set.\n";
        fs::write(path, format!("{}{}\n", header, new_line))?;
        return Ok(());
    }

    // Scan top-level (before any `[...]` section header) for an existing
    // `notify = ` line.
    let mut top_level_notify_line: Option<usize> = None;
    let mut top_level_notify_value: String = String::new();
    for (i, line) in existing.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            // entered a section table — stop scanning top-level
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("notify") {
            // "notify =" or "notify=" possibly with whitespace
            let rest = rest.trim_start();
            if rest.starts_with('=') {
                top_level_notify_line = Some(i);
                top_level_notify_value = rest.to_string();
                break;
            }
        }
    }

    match top_level_notify_line {
        None => {
            // Append at top so it stays top-level even if user later adds
            // `[section]` blocks below.
            let mut buf = String::new();
            buf.push_str(&new_line);
            buf.push('\n');
            buf.push_str(&existing);
            if !buf.ends_with('\n') {
                buf.push('\n');
            }
            fs::write(path, buf)?;
        }
        Some(idx) => {
            // Is the existing notify pointing at us? Match either the native
            // subcommand (current form) or the legacy Python filename (so we
            // migrate old installs in place), independent of $HOME / casing.
            let points_at_us = top_level_notify_value.contains(CODEX_NOTIFY_SUBCOMMAND)
                || top_level_notify_value.contains(CODEX_NOTIFY_FILENAME);
            if points_at_us {
                let mut lines: Vec<String> =
                    existing.lines().map(|s| s.to_string()).collect();
                lines[idx] = new_line;
                let mut joined = lines.join("\n");
                if existing.ends_with('\n') {
                    joined.push('\n');
                }
                fs::write(path, joined)?;
            } else {
                eprintln!(
                    "[hook-installer] codex {} already has a top-level `notify`; \
                     leaving alone — Codex turn-complete events won't reach \
                     the dynamic island",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn patch_settings(path: &Path, command: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut root: Value = if path.exists() {
        let text = fs::read_to_string(path).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };
    if !root.is_object() {
        root = json!({});
    }

    // Ensure "hooks" is an object
    let needs_reset = root
        .get("hooks")
        .map(|h| !h.is_object())
        .unwrap_or(true);
    if needs_reset {
        root.as_object_mut()
            .unwrap()
            .insert("hooks".into(), json!({}));
    }

    let hook_cmd = json!({ "type": "command", "command": command });

    let hooks = root
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .expect("hooks is object");

    for event in EVENTS {
        let entry = if EVENTS_WITH_MATCHER.contains(event) {
            json!({ "matcher": "*", "hooks": [hook_cmd.clone()] })
        } else {
            json!({ "hooks": [hook_cmd.clone()] })
        };

        let slot = hooks
            .entry(event.to_string())
            .or_insert_with(|| json!([]));
        if !slot.is_array() {
            *slot = json!([]);
        }
        let arr = slot.as_array_mut().unwrap();
        arr.retain(|e| !is_coffee_entry(e));
        arr.push(entry);
    }

    fs::write(path, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

fn is_coffee_entry(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hs| {
            hs.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    // Match the legacy Python command (so old entries get
                    // migrated in place) and the native command, which is
                    // `"<exe>" __hook` — i.e. `__hook` is the final argv
                    // token. We check the LAST whitespace-delimited token
                    // rather than a bare `contains("__hook")` so a user's own
                    // hook whose command path merely *contains* "__hook"
                    // (e.g. /home/u/.__hooks/lint.sh) is never misclassified
                    // as ours and stripped.
                    .map(|s| {
                        s.contains(SCRIPT_FILENAME)
                            || s.split_whitespace().last() == Some(HOOK_SUBCOMMAND)
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Build the Claude Code hook command: `"<exe>" __hook`. Claude runs
/// shell-form hooks via Git Bash on Windows, where backslash paths are
/// fragile — emit forward slashes there (Git Bash and CreateProcess both
/// accept `C:/...`). Returns None only if `current_exe()` fails.
fn claude_hook_command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let p = exe.display().to_string();
    let p = if cfg!(target_os = "windows") {
        p.replace('\\', "/")
    } else {
        p
    };
    Some(format!("\"{}\" {}", p, HOOK_SUBCOMMAND))
}
