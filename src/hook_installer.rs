// Coffee CLI legacy hook cleanup
//
// Coffee no longer installs status hooks or plugins into third-party tools.
// At app launch this module removes artifacts written by older releases while
// preserving user-owned hooks and unrelated configuration.
//
//   Claude Code
//     1. No hook install. TierTerminal reads Claude's native OSC title.
//     2. Prior Coffee entries are stripped from settings.json and
//        settings.local.json while user-owned hooks remain untouched.
//     3. The obsolete ~/.coffee-cli/hooks/coffee-cli-hook.py copy is removed.
//
//   Codex
//     1. No hook install. TierTerminal reads Codex's native OSC terminal title.
//     2. Prior Coffee hook / notify / trust entries are removed at app launch.
//     3. Remove the obsolete ~/.coffee-cli/hooks notify script copy.
//
//   OpenCode / MiMo Code
//     1. Remove coffee-cli-island.js from each tool's plugin directory.
//     2. Remove the old debug copy and diagnostic log.
//     3. Keep the unrelated transparent TUI theme migration.
//
//   Hermes Agent
//     1. Remove Coffee's plugin files from <HERMES_HOME>/plugins/.
//     2. Remove coffee-cli-status from plugins.enabled/disabled in config.yaml.
//
//   Kimi Code
//     1. Remove only `[[hooks]]` blocks whose command ends in __kimi-hook.
//
//   Grok Build
//     1. Remove every Coffee-owned JSON hook file containing __grok-hook.
//
// Errors are logged, never fatal — a broken installer must not prevent
// Coffee CLI from starting.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SCRIPT_FILENAME: &str = "coffee-cli-hook.py";

const CODEX_NOTIFY_FILENAME: &str = "coffee-cli-codex-notify.py";

/// Tokens used only to recognize entries written by older Coffee releases.
const HOOK_SUBCOMMAND: &str = "__hook";
const CODEX_NOTIFY_SUBCOMMAND: &str = "__codex-notify";

const OPENCODE_PLUGIN_FILENAME: &str = "coffee-cli-island.js";

const HERMES_PLUGIN_NAME: &str = "coffee-cli-status";

pub fn cleanup_all() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("[hook-installer] no home dir — skipping");
            return;
        }
    };

    for tool in crate::tools::TOOLS {
        if tool.has_legacy_hook_artifacts {
            cleanup_tool(tool, &home);
        }
    }

    // Windows-only: opencode/mimocode's `opencode upgrade` (which re-runs
    // `npm install -g`) shatters the global bin links when the binary is
    // running — npm renames opencode.cmd → .opencode.cmd-<rand>, then the
    // write of the new file fails because cmd.exe holds a lock on it, leaving
    // orphans and no usable bin. Detect that state at launch and repair it
    // by re-running the install. See repair_broken_npm_bins() for details.
    #[cfg(target_os = "windows")]
    {
        crate::hook_installer::repair_broken_npm_bins();
    }
}

/// Re-run cleanup for one tool after the launchpad's PATH rescan. This also
/// applies the OpenCode-family transparent theme when a CLI is newly installed.
pub fn maintain_for_tool(tool: &str) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return,
    };
    let Some(descriptor) = crate::tools::find(tool) else {
        return;
    };
    if !descriptor.has_legacy_hook_artifacts {
        return;
    }
    cleanup_tool(descriptor, &home);
}

/// Per-tool legacy cleanup dispatch. Cleanup runs without a PATH gate so an
/// uninstalled CLI cannot strand Coffee's old entries in user configuration.
fn cleanup_tool(tool: &crate::tools::ToolDescriptor, home: &Path) {
    match tool.id {
        "claude" => {
            cleanup_claude(home);
            return;
        }
        "codex" => {
            cleanup_codex(home);
            return;
        }
        "opencode" => {
            cleanup_opencode_plugin(home, "opencode");
            if crate::server::binary_on_path(tool.binary_name) {
                ensure_opencode_tui_theme_default(home, "opencode");
            }
        }
        "mimocode" => {
            cleanup_opencode_plugin(home, "mimocode");
            if crate::server::binary_on_path(tool.binary_name) {
                ensure_opencode_tui_theme_default(home, "mimocode");
            }
        }
        "kilo" => {
            cleanup_opencode_plugin(home, "kilo");
            if crate::server::binary_on_path(tool.binary_name) {
                ensure_opencode_tui_theme_default(home, "kilo");
            }
        }
        "hermes" => cleanup_hermes_plugin(home),
        "kimicode" => cleanup_kimi_hooks(home),
        "grok" => cleanup_grok_hooks(home),
        other => {
            eprintln!(
                "[hook-installer] tool '{}' declares legacy hook artifacts but has no cleanup arm",
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

fn cleanup_claude(home: &Path) {
    // Claude's native title now drives Coffee's working/idle state. Remove
    // every Coffee-installed handler from both historical config locations;
    // malformed files and user-owned hooks are deliberately left untouched.
    for path in [
        home.join(".claude").join("settings.json"),
        home.join(".claude").join("settings.local.json"),
    ] {
        if !path.exists() {
            continue;
        }
        if let Err(e) = strip_coffee_hooks(&path) {
            eprintln!("[hook-installer] failed to clean {}: {}", path.display(), e);
        }
    }

    // This fixed path was created only by Coffee CLI. It is not executable
    // configuration anymore, so remove it as part of the same migration.
    let legacy_script = home.join(".coffee-cli").join("hooks").join(SCRIPT_FILENAME);
    if legacy_script.exists() {
        if let Err(e) = fs::remove_file(&legacy_script) {
            eprintln!(
                "[hook-installer] failed to remove {}: {}",
                legacy_script.display(),
                e
            );
        }
    }
}

/// Codex hook-driven dynamic-island support has been removed. Coffee reads
/// Codex's native OSC terminal-title activity directly in TierTerminal instead,
/// avoiding hook trust churn while still receiving working / input / idle.
/// Any prior Coffee CLI codex hook / notify / trust install is stripped so existing setups
/// stop firing the broken hooks and stop hitting codex's "hooks need review"
/// prompt.
fn cleanup_codex(home: &Path) {
    cleanup_codex_island_install(home);
    let script = home
        .join(".coffee-cli")
        .join("hooks")
        .join(CODEX_NOTIFY_FILENAME);
    remove_marked_file(&script, &["Coffee CLI", "Codex Notify Forwarder"]);
}

/// Remove every Coffee CLI codex dynamic-island artifact from ~/.codex so a
/// prior install (hooks.json entries, the `notify` line, `[hooks.state]` trust
/// blocks) stops firing the broken/renamed hooks. Idempotent; preserves
/// user-owned codex hooks and all other config. Errors are logged, not fatal.
fn cleanup_codex_island_install(home: &Path) {
    let hooks_path = home.join(".codex").join("hooks.json");
    let config_path = home.join(".codex").join("config.toml");
    if let Err(e) = strip_codex_managed_hooks(&hooks_path) {
        eprintln!(
            "[hook-installer] failed to strip codex managed hooks: {}",
            e
        );
    }
    if let Err(e) = strip_codex_notify(&config_path) {
        eprintln!("[hook-installer] failed to strip codex notify line: {}", e);
    }
    if let Err(e) = strip_codex_managed_trust(&config_path, &hooks_path) {
        eprintln!(
            "[hook-installer] failed to strip codex managed trust: {}",
            e
        );
    }
}

/// Remove our `__codex-hook` entries from the 4 managed events in
/// ~/.codex/hooks.json, dropping events left empty. A malformed or user-owned
/// file is left untouched.
fn strip_codex_managed_hooks(hooks_path: &Path) -> anyhow::Result<()> {
    if !hooks_path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(hooks_path).unwrap_or_default();
    let mut root: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    if !root.is_object() {
        return Ok(()); // malformed — don't touch
    }
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(());
    };
    let mut changed = false;
    for event in [
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "Stop",
    ] {
        let Some(arr) = hooks.get_mut(event).and_then(|e| e.as_array_mut()) else {
            continue;
        };
        let mut event_changed = false;
        arr.retain_mut(|group| {
            let Some(entries) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                return true;
            };
            let before = entries.len();
            entries.retain(|entry| !is_coffee_codex_entry(entry));
            let group_changed = entries.len() != before;
            changed |= group_changed;
            event_changed |= group_changed;
            !group_changed || !entries.is_empty()
        });
        if event_changed && arr.is_empty() {
            hooks.remove(event);
        }
    }
    if !changed {
        return Ok(());
    }
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
    let out = serde_json::to_string_pretty(&root)?;
    fs::write(hooks_path, out)?;
    Ok(())
}

/// Remove our top-level `notify = ["<exe>", "__codex-notify"]` line from
/// ~/.codex/config.toml (only the top-level one — a notify inside a [section]
/// is a different key we don't touch). Byte-preserving line edit.
fn strip_codex_notify(config_path: &Path) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let existing = fs::read_to_string(config_path).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut seen_section = false;
    let mut changed = false;
    for line in existing.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            seen_section = true;
        }
        if !seen_section && is_our_notify_line(trimmed) {
            changed = true;
            continue;
        }
        out.push(line.to_string());
    }
    if changed {
        let mut joined = out.join("\n");
        if !joined.ends_with('\n') {
            joined.push('\n');
        }
        fs::write(config_path, joined)?;
    }
    Ok(())
}

fn is_our_notify_line(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix("notify") else {
        return false;
    };
    let rest = rest.trim_start();
    if !rest.starts_with('=') {
        return false;
    }
    rest.contains(CODEX_NOTIFY_SUBCOMMAND) || rest.contains(CODEX_NOTIFY_FILENAME)
}

/// Remove our `[hooks.state."<our-source>:<event>:g:h"]` blocks from
/// ~/.codex/config.toml — keyed by our hooks.json path + the 4 managed event
/// labels. Other tools'/user's trust blocks are preserved.
fn strip_codex_managed_trust(config_path: &Path, hooks_path: &Path) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let existing = fs::read_to_string(config_path).unwrap_or_default();
    let source_prefix = format!("{}:", hooks_path.to_string_lossy());
    let labels = [
        "session_start",
        "user_prompt_submit",
        "permission_request",
        "stop",
    ];
    let lines: Vec<&str> = existing.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if let Some(key) = parse_codex_state_header(trimmed) {
            let is_ours = key.starts_with(&source_prefix)
                && labels
                    .iter()
                    .any(|l| key[source_prefix.len()..].starts_with(&format!("{}:", l)));
            if is_ours {
                let mut j = i + 1;
                while j < lines.len() && !is_toml_table_header(lines[j]) {
                    j += 1;
                }
                changed = true;
                i = j;
                continue;
            }
        }
        out.push(lines[i].to_string());
        i += 1;
    }
    if changed {
        let mut joined = out.join("\n");
        if !joined.ends_with('\n') {
            joined.push('\n');
        }
        fs::write(config_path, joined)?;
    }
    Ok(())
}

/// Remove the OpenCode-family plugin written by older Coffee releases. The
/// fixed filename is only deleted when its contents carry Coffee's marker, so
/// a user replacement with the same name survives.
fn cleanup_opencode_plugin(home: &Path, config_subdir: &str) {
    let plugin_path = home
        .join(".config")
        .join(config_subdir)
        .join("plugins")
        .join(OPENCODE_PLUGIN_FILENAME);
    remove_marked_file(
        &plugin_path,
        &["Coffee CLI", "CoffeeCliIslandPlugin"],
    );

    let debug_copy = home
        .join(".coffee-cli")
        .join("hooks")
        .join(OPENCODE_PLUGIN_FILENAME);
    remove_marked_file(
        &debug_copy,
        &["Coffee CLI", "CoffeeCliIslandPlugin"],
    );

    // This log name was hardcoded by Coffee's old plugin and has no other
    // producer. It may contain prompt/event diagnostics, so remove it too.
    let _ = fs::remove_file(home.join("coffee-cli-opencode.log"));
}

/// Remove Coffee's Hermes plugin files and allow-list entry without invoking
/// Hermes or reserializing the user's YAML. Block-list and flow-list forms are
/// both handled; comments and unrelated formatting remain intact.
fn cleanup_hermes_plugin(home: &Path) {
    let hermes_home = crate::tools::hermes::hermes_home();
    let plugin_dir = hermes_home.join("plugins").join(HERMES_PLUGIN_NAME);
    let init_path = plugin_dir.join("__init__.py");
    let manifest_path = plugin_dir.join("plugin.yaml");

    remove_marked_file(
        &init_path,
        &["Coffee CLI status forwarder", "pre_approval_request"],
    );
    remove_marked_file(&manifest_path, &["name: coffee-cli-status", "Coffee CLI"]);
    let _ = fs::remove_dir(&plugin_dir);

    if let Err(e) = strip_hermes_plugin_from_yaml(&hermes_home.join("config.yaml")) {
        eprintln!("[hook-installer] failed to clean Hermes config: {}", e);
    }

    let debug_copy = home
        .join(".coffee-cli")
        .join("hooks")
        .join("coffee-cli-hermes-plugin.py");
    remove_marked_file(
        &debug_copy,
        &["Coffee CLI status forwarder", "pre_approval_request"],
    );
}

fn remove_marked_file(path: &Path, markers: &[&str]) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    if !markers.iter().all(|marker| text.contains(marker)) {
        return;
    }
    if let Err(e) = fs::remove_file(path) {
        eprintln!(
            "[hook-installer] failed to remove {}: {}",
            path.display(),
            e
        );
    }
}

fn strip_hermes_plugin_from_yaml(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut plugins_indent: Option<usize> = None;
    let mut list_indent: Option<usize> = None;
    let mut changed = false;

    for line in lines {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let structural = !trimmed.is_empty() && !trimmed.starts_with('#');

        if structural {
            if let Some(base) = plugins_indent {
                if indent <= base && !trimmed.starts_with("plugins:") {
                    plugins_indent = None;
                    list_indent = None;
                }
            }
            if trimmed
                .strip_prefix("plugins:")
                .map(|rest| rest.trim().is_empty() || rest.trim_start().starts_with('#'))
                .unwrap_or(false)
            {
                plugins_indent = Some(indent);
                list_indent = None;
            } else if plugins_indent.is_some() {
                if let Some(base) = list_indent {
                    if indent <= base {
                        list_indent = None;
                    }
                }
                if let Some(rewritten) = strip_hermes_flow_list(line, "enabled")
                    .or_else(|| strip_hermes_flow_list(line, "disabled"))
                {
                    changed |= rewritten != line;
                    out.push(rewritten);
                    continue;
                }
                if ["enabled:", "disabled:"].iter().any(|key| {
                    trimmed
                        .strip_prefix(key)
                        .map(|rest| rest.trim().is_empty() || rest.trim_start().starts_with('#'))
                        .unwrap_or(false)
                }) {
                    list_indent = Some(indent);
                } else if list_indent.is_some() && is_hermes_plugin_list_item(trimmed) {
                    changed = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }

    if changed {
        let mut joined = out.join("\n");
        if text.ends_with('\n') {
            joined.push('\n');
        }
        fs::write(path, joined)?;
    }
    Ok(())
}

fn is_hermes_plugin_list_item(trimmed: &str) -> bool {
    let Some(value) = trimmed.strip_prefix('-') else {
        return false;
    };
    yaml_scalar_without_comment(value) == HERMES_PLUGIN_NAME
}

fn strip_hermes_flow_list(line: &str, key: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let open = rest.find('[')?;
    let close = rest[open + 1..].find(']')? + open + 1;
    let values = &rest[open + 1..close];
    let kept: Vec<&str> = values
        .split(',')
        .filter(|value| yaml_scalar_without_comment(value) != HERMES_PLUGIN_NAME)
        .collect();
    if kept.len() == values.split(',').count() {
        return Some(line.to_string());
    }
    let prefix_len = line.len() - trimmed.len();
    let suffix = &rest[close + 1..];
    Some(format!(
        "{}{}: [{}]{}",
        &line[..prefix_len],
        key,
        kept.iter().map(|v| v.trim()).collect::<Vec<_>>().join(", "),
        suffix
    ))
}

fn yaml_scalar_without_comment(value: &str) -> &str {
    value
        .split('#')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches(['\'', '"'])
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
    let Some(obj) = root.as_object_mut() else {
        return;
    };
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

/// Remove every Coffee CLI hook handler from `path` without touching any
/// user-owned key or sibling handler in the same hook group.
fn strip_coffee_hooks(path: &Path) -> anyhow::Result<()> {
    let text = fs::read_to_string(path)?;
    let mut root: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Ok(()), // unparseable user file — leave it alone
    };
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(());
    };

    let mut changed = false;
    let mut empty_events = Vec::new();
    for (event, slot) in hooks.iter_mut() {
        if let Some(arr) = slot.as_array_mut() {
            let mut emptied_groups = Vec::new();
            for (index, group) in arr.iter_mut().enumerate() {
                let Some(handlers) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                    continue;
                };
                let before = handlers.len();
                handlers.retain(|handler| !is_coffee_handler(handler));
                if handlers.len() != before {
                    changed = true;
                    if handlers.is_empty() {
                        emptied_groups.push(index);
                    }
                }
            }
            for index in emptied_groups.into_iter().rev() {
                arr.remove(index);
            }
            if arr.is_empty() {
                empty_events.push(event.clone());
            }
        }
    }
    for k in empty_events {
        hooks.remove(&k);
        changed = true;
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

    if changed {
        fs::write(path, serde_json::to_string_pretty(&root)?)?;
    }
    Ok(())
}

fn is_coffee_handler(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(|c| c.as_str())
        // Match the legacy Python command and the native `<exe> __hook`
        // command. Last-token matching avoids deleting a user command whose
        // path merely contains "__hook" (for example .__hooks/lint.sh).
        .map(|command| {
            command.contains(SCRIPT_FILENAME)
                || command.split_whitespace().last() == Some(HOOK_SUBCOMMAND)
        })
        .unwrap_or(false)
}

/// Token used by old Coffee entries in ~/.codex/hooks.json.
const CODEX_HOOK_SUBCOMMAND: &str = "__codex-hook";

/// A hook entry is ours iff its command's last whitespace-delimited token is
/// `__codex-hook` (the native subcommand). Uses the same last-token
/// match so a user hook whose path merely contains "__codex-hook"
/// is never misclassified as ours.
fn is_coffee_codex_entry(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(|c| c.as_str())
        .map(|s| s.split_whitespace().last() == Some(CODEX_HOOK_SUBCOMMAND))
        .unwrap_or(false)
}

/// Parse a `[hooks.state.'<key>']` / `[hooks.state."<key>"]` header line,
/// returning the raw key string. Returns None for the bare `[hooks.state]`
/// parent table or any other header.
fn parse_codex_state_header(line: &str) -> Option<String> {
    let t = line.trim();
    let rest = t.strip_prefix("[hooks.state.")?;
    if let Some(r) = rest.strip_prefix('\'') {
        // literal string — read until the next single quote
        let end = r.find('\'')?;
        let after = r[end + 1..].trim_start();
        if after.starts_with(']') {
            Some(r[..end].to_string())
        } else {
            None
        }
    } else if let Some(r) = rest.strip_prefix('"') {
        // basic string — read until an unescaped double quote
        let mut key = String::new();
        let mut chars = r.chars().peekable();
        let mut closed = false;
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(&n) = chars.peek() {
                    key.push(n);
                    chars.next();
                }
                continue;
            }
            if c == '"' {
                closed = true;
                break;
            }
            key.push(c);
        }
        if closed {
            Some(key)
        } else {
            None
        }
    } else {
        None
    }
}

fn is_toml_table_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

// ─── Kimi Code legacy hook cleanup ───────────────────────────────────────────

const KIMI_HOOK_SUBCOMMAND: &str = "__kimi-hook";

fn cleanup_kimi_hooks(home: &Path) {
    let config_path = home.join(".kimi-code").join("config.toml");
    if let Err(e) = strip_kimi_hooks(&config_path) {
        eprintln!(
            "[hook-installer] failed to clean {}: {}",
            config_path.display(),
            e
        );
    }
}

/// Remove Coffee-owned `[[hooks]]` tables while preserving all user TOML
/// byte-for-byte apart from blank lines adjacent to removed blocks.
fn strip_kimi_hooks(path: &Path) -> anyhow::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let existing = fs::read_to_string(path)?;

    // Strip our prior entries. A "block" is a `[[...]]` array-of-tables
    // header line plus the lines up to (not including) the next table
    // header; it's ours iff it contains a `command` line whose last token
    // is `__kimi-hook` (the same last-token discipline as the Claude cleanup,
    // user hook whose path merely *contains* the token is never stripped).
    let lines: Vec<&str> = existing.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start().starts_with("[[") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with('[') {
                j += 1;
            }
            if lines[i..j].iter().any(|l| is_coffee_kimi_command_line(l)) {
                changed = true;
            } else {
                kept.extend(&lines[i..j]);
            }
            i = j;
        } else {
            kept.push(lines[i]);
            i += 1;
        }
    }
    if !changed {
        return Ok(());
    }

    const COFFEE_HEADER: [&str; 3] = [
        "# Coffee CLI registered these hooks for the dynamic-island status",
        "# indicator. Safe to remove if you don't use Coffee CLI — the command",
        "# no-ops when COFFEE_CLI_* env vars aren't set.",
    ];
    kept.retain(|line| !COFFEE_HEADER.contains(&line.trim_end()));
    while kept
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        kept.pop();
    }

    if kept.is_empty() {
        fs::remove_file(path)?;
    } else {
        let mut out = kept.join("\n");
        out.push('\n');
        fs::write(path, out)?;
    }
    Ok(())
}

/// A `command = "..."` line is
/// ours iff the last whitespace-delimited token inside the quotes is
/// `__kimi-hook`. Uses last-token matching so a user
/// hook whose path merely *contains* "__kimi-hook" (e.g.
/// /home/u/.__kimi-hooks/lint.sh) is never misclassified as ours.
fn is_coffee_kimi_command_line(line: &str) -> bool {
    let t = line.trim();
    let Some(rest) = t.strip_prefix("command") else {
        return false;
    };
    let Some(value) = rest.trim_start().strip_prefix('=') else {
        return false;
    };
    let value = value.trim().trim_end_matches('"');
    value.split_whitespace().last() == Some(KIMI_HOOK_SUBCOMMAND)
}

// ─── Broken-bin repair (Windows) ────────────────────────────────────────────
//
// `opencode upgrade` re-runs `npm install -g opencode-ai` to rewrite the
// global bin. On Windows, if an opencode process is running (e.g. the one
// Coffee CLI launched), cmd.exe holds a lock on opencode.cmd — npm renames
// it to .opencode.cmd-<rand> as the first step of the rewrite, then fails
// to write the new file, leaving the orphan AND no usable bin. `where
// opencode` then fails with "not found".
//
// We can't prevent the upgrade (the user runs it themselves, outside our
// process). But at Coffee CLI launch — when opencode is almost certainly
// NOT running (the user just opened the app) — we can detect the broken
// state and re-run the install to rebuild the links. Idempotent and safe:
// if the bin is fine, we do nothing; if the package isn't npm-installed,
// we do nothing; if the binary is currently running, we skip (can't fix
// under the lock anyway — next launch will catch it).

#[cfg(target_os = "windows")]
const NPM_REPAIR_TARGETS: &[(&str, &str)] = &[
    // (binary_name we look for on PATH, npm global package that provides it)
    ("opencode", "opencode-ai"),
    // MiMo Code is an OpenCode fork with the same upgrade/bin-rewrite shape.
    // Its npm package name isn't confirmed across installs, so this entry is
    // best-effort — add the correct name here once verified.
    // ("mimo", "@mimo-ai/cli"),
];

#[cfg(target_os = "windows")]
pub fn repair_broken_npm_bins() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    for (bin, pkg) in NPM_REPAIR_TARGETS {
        // Bin still resolves? Nothing to do.
        if crate::server::binary_on_path(bin) {
            continue;
        }
        // FAST PATH — detect the breakage WITHOUT spawning npm. The signature
        // of a shattered bin is orphan files in npm's global bin dir: npm
        // renames `opencode.cmd` → `.opencode.cmd-<rand>` as the first step of
        // a rewrite, then fails to write the new file, leaving the orphan AND
        // no usable bin. If no such orphan exists, either the user never had
        // this tool, or the bin is gone for an unrelated reason — in both
        // cases an `npm install -g` won't help and would just waste ~1-2s of
        // boot time spawning npm for users who never installed opencode.
        let Some(npm_bin_dir) = npm_global_bin_dir() else {
            continue;
        };
        if !has_shattered_orphans(&npm_bin_dir, bin) {
            continue;
        }
        // Is the binary currently running? If so, a repair now would hit the
        // same file lock that broke it. tasklist /fi over the image name,
        // CREATE_NO_WINDOW. Skip on any error (better to try the repair than
        // to skip it because tasklist itself failed).
        if process_is_running(bin) {
            eprintln!(
                "[hook-installer] {} bin is broken but the process is running — \
                 skipping npm repair (would hit the file lock). It'll repair on a \
                 next launch where {} isn't running.",
                bin, bin
            );
            continue;
        }
        eprintln!(
            "[hook-installer] {} bin missing with orphan files in {} — repairing \
             the bin links with `npm install -g {}`",
            bin,
            npm_bin_dir.display(),
            pkg
        );
        // Re-run the install to rebuild the bin links. 120s timeout — npm
        // global install can be slow on a cold cache, but we don't want to
        // hang the app boot forever if something's wrong. cmd /c for the
        // same .cmd-shim reason as the ls above.
        let mut repair = Command::new("cmd");
        repair
            .args(["/c", "npm", "install", "-g", pkg])
            .creation_flags(0x08000000);
        match run_with_timeout(&mut repair, std::time::Duration::from_secs(120)) {
            Ok(true) => {
                eprintln!("[hook-installer] {} repair install finished", bin);
            }
            Ok(false) => {
                eprintln!("[hook-installer] {} repair install timed out", bin);
            }
            Err(e) => {
                eprintln!("[hook-installer] {} repair install failed: {}", bin, e);
            }
        }
    }
}

/// npm's global bin dir on Windows. npm prefix -g is normally
/// `%APPDATA%\npm` (where .cmd shims live). Derived from APPDATA rather than
/// spawning `npm prefix -g` so the orphan-check fast path stays spawn-free.
#[cfg(target_os = "windows")]
fn npm_global_bin_dir() -> Option<std::path::PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(std::path::PathBuf::from(appdata).join("npm"))
}

/// True iff npm's global bin dir contains a shattered-orphan file for `bin`:
/// a file named `.{bin}.cmd-<suffix>`, `.{bin}.ps1-<suffix>`, or
/// `.{bin}-<suffix>` (the temp-rename residue npm leaves when a bin rewrite
/// is interrupted). These only exist when a real install was shattered —
/// users who never installed the tool have no such files, so this is the
/// zero-spawn signal to skip the repair entirely.
#[cfg(target_os = "windows")]
fn has_shattered_orphans(npm_bin_dir: &std::path::Path, bin: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(npm_bin_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Orphans look like ".opencode.cmd-S0tGGhyQ", ".opencode.ps1-f0SU9OXr",
        // ".opencode-TbIJLj3H" — a leading dot, the bin name, then a suffix
        // after a '-' (the random rename token). The real bin has no leading
        // dot and no '-' suffix.
        if name.starts_with(&format!(".{}", bin)) && name.contains('-') {
            return true;
        }
    }
    false
}

#[cfg(target_os = "windows")]
fn process_is_running(image_name: &str) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    // tasklist filters by image name; the exe may be opencode.exe or
    // mimo.exe. Match the bare name (tasklist matches case-insensitively
    // and accepts with/without .exe).
    let filter = format!("imagename eq {}*", image_name);
    match Command::new("tasklist")
        .args(["/fi", &filter, "/nh", "/fo", "csv"])
        .creation_flags(0x08000000)
        .output()
    {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            // CSV rows for running processes start with the quoted image name.
            // No header (/nh), so any non-empty output line mentioning the
            // name means it's running.
            out.lines().any(|l| l.to_lowercase().contains(image_name))
        }
        Err(_) => false, // tasklist failed — assume not running so we still try
    }
}

#[cfg(target_os = "windows")]
fn run_with_timeout(
    cmd: &mut std::process::Command,
    dur: std::time::Duration,
) -> std::io::Result<bool> {
    // std::process::Command has no blocking-with-timeout; spawn and poll
    // try_wait until the deadline. On timeout, kill the child so a hung npm
    // doesn't stall boot. Returns Ok(true) if it exited, Ok(false) if killed.
    use std::time::Instant;
    let mut child = cmd.spawn()?;
    let deadline = Instant::now() + dur;
    loop {
        if let Some(_status) = child.try_wait()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        // Short sleep so we don't busy-wait; npm install is seconds-to-minutes,
        // a 100ms poll is fine-grained enough.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn fresh_codex_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("coffee-codex-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn claude_migration_removes_only_coffee_hooks_and_legacy_script() {
        let home = fresh_codex_dir("claude-title-migration");
        let claude_dir = home.join(".claude");
        let hook_dir = home.join(".coffee-cli").join("hooks");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::create_dir_all(&hook_dir).unwrap();

        let settings = json!({
            "theme": "dark",
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [
                        { "type": "command", "command": "& \"C:/Coffee CLI/coffee-cli.exe\" __hook" },
                        { "type": "command", "command": "/home/user/my-hook.sh" }
                    ]
                }],
                "Stop": [{
                    "hooks": [{ "type": "command", "command": "/home/user/stop.sh" }]
                }]
            }
        });
        let local_settings = json!({
            "hooks": {
                "Notification": [{
                    "hooks": [{ "type": "command", "command": "python ~/.coffee-cli/hooks/coffee-cli-hook.py" }]
                }]
            }
        });
        let settings_path = claude_dir.join("settings.json");
        let local_path = claude_dir.join("settings.local.json");
        let legacy_script = hook_dir.join(SCRIPT_FILENAME);
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();
        fs::write(
            &local_path,
            serde_json::to_string_pretty(&local_settings).unwrap(),
        )
        .unwrap();
        fs::write(&legacy_script, "legacy").unwrap();

        cleanup_claude(&home);

        let cleaned: Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let prompt_handlers = cleaned["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap();
        assert_eq!(prompt_handlers.len(), 1);
        assert_eq!(prompt_handlers[0]["command"], "/home/user/my-hook.sh");
        assert_eq!(
            cleaned["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/home/user/stop.sh"
        );
        assert_eq!(cleaned["theme"], "dark");

        let cleaned_local: Value =
            serde_json::from_str(&fs::read_to_string(&local_path).unwrap()).unwrap();
        assert!(cleaned_local.get("hooks").is_none());
        assert!(!legacy_script.exists());

        let first_cleanup = fs::read_to_string(&settings_path).unwrap();
        cleanup_claude(&home);
        assert_eq!(fs::read_to_string(&settings_path).unwrap(), first_cleanup);

        let _ = fs::remove_dir_all(&home);
    }

    // ─── Kimi Code config.toml `[[hooks]]` ──────────────────────────────────
    // Cargo.toml has no toml/toml_edit dependency, so these are text-level
    // assertions (per the line-editing design — a parsed round-trip would
    // defeat the comment-preservation goal anyway).

    fn fresh_kimi_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("coffee-kimi-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn kimi_cleanup_removes_coffee_blocks_and_keeps_user_content() {
        let dir = fresh_kimi_dir("cleanup");
        let cfg = dir.join("config.toml");
        let seeded = "# my kimi config\nmodel = \"k2\"\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"\\\"/old/path/coffee-cli.exe\\\" __kimi-hook\"\ntimeout = 30\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"echo user\"\ntimeout = 5\n";
        fs::write(&cfg, seeded).unwrap();

        strip_kimi_hooks(&cfg).unwrap();
        let after = fs::read_to_string(&cfg).unwrap();
        assert!(
            !after.contains("__kimi-hook"),
            "Coffee hook removed: {}",
            after
        );
        assert!(
            after.contains("# my kimi config"),
            "comment preserved: {}",
            after
        );
        assert!(
            after.contains("model = \"k2\""),
            "config preserved: {}",
            after
        );
        assert!(
            after.contains("command = \"echo user\""),
            "user hook preserved: {}",
            after
        );

        let first = after.clone();
        strip_kimi_hooks(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&cfg).unwrap(), first);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn kimi_cleanup_deletes_coffee_only_config() {
        let dir = fresh_kimi_dir("only-coffee");
        let cfg = dir.join("config.toml");
        let seeded = "# Coffee CLI registered these hooks for the dynamic-island status\n# indicator. Safe to remove if you don't use Coffee CLI — the command\n# no-ops when COFFEE_CLI_* env vars aren't set.\n[[hooks]]\nevent = \"Stop\"\ncommand = \"\\\"/old/coffee-cli.exe\\\" __kimi-hook\"\ntimeout = 30\n";
        fs::write(&cfg, seeded).unwrap();
        strip_kimi_hooks(&cfg).unwrap();
        assert!(!cfg.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hermes_yaml_cleanup_preserves_other_plugins() {
        let dir = fresh_kimi_dir("hermes-yaml");
        let cfg = dir.join("config.yaml");
        let seeded = "model: test\nplugins:\n  enabled:\n    - user-plugin\n    - coffee-cli-status # old Coffee plugin\n  disabled: [quiet-plugin, \"coffee-cli-status\"]\nother: true\n";
        fs::write(&cfg, seeded).unwrap();
        strip_hermes_plugin_from_yaml(&cfg).unwrap();
        let after = fs::read_to_string(&cfg).unwrap();
        assert!(
            !after.contains("coffee-cli-status"),
            "Coffee plugin removed: {}",
            after
        );
        assert!(after.contains("user-plugin"));
        assert!(after.contains("quiet-plugin"));
        assert!(after.contains("model: test"));
        assert!(after.contains("other: true"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_cleanup_removes_only_coffee_artifacts() {
        let home = fresh_codex_dir("codex-cleanup");
        let codex = home.join(".codex");
        let hook_dir = home.join(".coffee-cli").join("hooks");
        fs::create_dir_all(&codex).unwrap();
        fs::create_dir_all(&hook_dir).unwrap();
        let hooks_path = codex.join("hooks.json");
        let config_path = codex.join("config.toml");
        fs::write(
            &hooks_path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "Stop": [{ "hooks": [
                        { "type": "command", "command": "C:/Coffee/coffee-cli.exe __codex-hook" },
                        { "type": "command", "command": "echo user" }
                    ] }]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let source = hooks_path.to_string_lossy();
        fs::write(
            &config_path,
            format!(
                "notify = [\"python\", \"coffee-cli-codex-notify.py\"]\nmodel = \"gpt-5\"\n\n[hooks.state.'{}:stop:0:0']\nenabled = true\n\n[hooks.state.'C:/user/hooks.json:stop:0:0']\nenabled = true\n",
                source
            ),
        )
        .unwrap();
        let script = hook_dir.join(CODEX_NOTIFY_FILENAME);
        fs::write(&script, "# Coffee CLI — Codex Notify Forwarder\n").unwrap();

        cleanup_codex(&home);

        let hooks: Value = serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
        let handlers = hooks["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["command"], "echo user");
        let config = fs::read_to_string(&config_path).unwrap();
        assert!(!config.contains(CODEX_NOTIFY_FILENAME));
        assert!(!config.contains(&format!("{}:stop", source)));
        assert!(config.contains("C:/user/hooks.json:stop"));
        assert!(config.contains("model = \"gpt-5\""));
        assert!(!script.exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_cleanup_does_not_rewrite_user_only_hooks() {
        let home = fresh_codex_dir("codex-user-only");
        let codex = home.join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let hooks_path = codex.join("hooks.json");
        let original = "{\n  \"hooks\": {\"Stop\": [{\"hooks\": [{\"command\": \"echo user\"}]}]}\n}\n";
        fs::write(&hooks_path, original).unwrap();

        strip_codex_managed_hooks(&hooks_path).unwrap();

        assert_eq!(fs::read_to_string(&hooks_path).unwrap(), original);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn opencode_cleanup_preserves_user_replacement() {
        let home = fresh_codex_dir("opencode-cleanup");
        let plugins = home.join(".config").join("opencode").join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        let plugin = plugins.join(OPENCODE_PLUGIN_FILENAME);
        fs::write(
            &plugin,
            "// Coffee CLI\nexport const CoffeeCliIslandPlugin = 1;",
        )
        .unwrap();
        cleanup_opencode_plugin(&home, "opencode");
        assert!(!plugin.exists());

        fs::write(&plugin, "export const userPlugin = true;").unwrap();
        cleanup_opencode_plugin(&home, "opencode");
        assert!(plugin.exists());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn grok_cleanup_removes_all_coffee_json_and_keeps_user_hooks() {
        let dir = fresh_codex_dir("grok-cleanup");
        let hooks = dir.join("hooks");
        fs::create_dir_all(&hooks).unwrap();
        fs::write(
            hooks.join("coffee-cli-stop.json"),
            "{\"command\":\"coffee __grok-hook\"}",
        )
        .unwrap();
        fs::write(
            hooks.join("coffee-cli-user.json"),
            "{\"command\":\"echo user\"}",
        )
        .unwrap();
        fs::write(
            hooks.join("my-hook.json"),
            "{\"command\":\"coffee __grok-hook\"}",
        )
        .unwrap();

        cleanup_grok_hook_dir(&hooks);
        assert!(!hooks.join("coffee-cli-stop.json").exists());
        assert!(hooks.join("coffee-cli-user.json").exists());
        assert!(hooks.join("my-hook.json").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Grok Build legacy hook cleanup
// ──────────────────────────────────────────────────────────────────────────────

fn cleanup_grok_hooks(home: &Path) {
    let grok_home = std::env::var("GROK_HOME")
        .ok()
        .and_then(|s| {
            if s.is_empty() {
                None
            } else {
                Some(PathBuf::from(s))
            }
        })
        .unwrap_or_else(|| home.join(".grok"));
    let hooks_dir = grok_home.join("hooks");
    cleanup_grok_hook_dir(&hooks_dir);
}

fn cleanup_grok_hook_dir(hooks_dir: &Path) {
    let Ok(entries) = fs::read_dir(&hooks_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_json = path.extension().and_then(|ext| ext.to_str()) == Some("json");
        let coffee_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("coffee-cli-") || name == "coffee-cli-status.json")
            .unwrap_or(false);
        if is_json && coffee_name {
            remove_marked_file(&path, &["__grok-hook"]);
        }
    }
    let _ = fs::remove_dir(&hooks_dir);
}
