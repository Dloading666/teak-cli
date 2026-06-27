// Coffee CLI — native hook forwarder
//
// Invoked as a Claude Code hook (`<exe> __hook`, event JSON on stdin) or a
// Codex `notify` target (`<exe> __codex-notify <json>`, payload as final
// argv). Maps the event to Coffee CLI's 3-state agent status and forwards a
// compact JSON line to the Rust hook server over loopback TCP.
//
// Replaces the two Python forwarders:
//   - scripts/coffee-cli-hook.py         (Claude, stdin protocol)
//   - scripts/coffee-cli-codex-notify.py (Codex, argv-tail protocol)
// which are now kept only as protocol-reference copies under
// ~/.coffee-cli/hooks/.
//
// Why native: the Python forwarder failed on Windows machines without
// Python — `python` resolved to the Microsoft Store alias stub, which
// prints "Python was not found…" to stderr and exits non-zero, so Claude
// Code surfaced a "UserPromptSubmit hook error" in the transcript on every
// prompt. The shipped binary is always present and needs no interpreter, so
// pointing the hook at ourselves removes the dependency entirely — the same
// "ship a native binary as the hook command" pattern CCometixLine uses for
// its statusline.
//
// Discipline mirrored from the Python scripts: every path exits 0. A flaky
// forwarder must never block the agent.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::{json, Value};

/// Env injected by Coffee CLI when spawning a tool in a tab (see
/// terminal.rs). Without `COFFEE_CLI_TAB_ID` + `COFFEE_CLI_HOOK_PORT` the
/// forwarder no-ops — that's the gate that keeps the globally-registered
/// Codex `notify` silent for sessions started outside Coffee CLI.
struct HookCtx {
    tab_id: String,
    port: u16,
    tool: String,
}

impl HookCtx {
    fn from_env() -> Option<HookCtx> {
        let tab_id = std::env::var("COFFEE_CLI_TAB_ID")
            .ok()
            .filter(|s| !s.is_empty())?;
        let port = std::env::var("COFFEE_CLI_HOOK_PORT")
            .ok()?
            .parse::<u16>()
            .ok()?;
        let tool = std::env::var("COFFEE_CLI_TOOL").unwrap_or_default();
        Some(HookCtx { tab_id, port, tool })
    }
}

/// `<exe> __hook` — Claude Code stdin hook protocol. Never returns.
pub fn run_claude_hook() -> ! {
    let _ = forward_claude();
    std::process::exit(0);
}

/// `<exe> __codex-notify <json>` — Codex `notify` argv-tail protocol.
/// Never returns.
pub fn run_codex_notify(args: &[String]) -> ! {
    let _ = forward_codex(args);
    std::process::exit(0);
}

fn forward_claude() -> Option<()> {
    let ctx = HookCtx::from_env()?;

    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    // Tolerate a leading UTF-8 BOM — some shells/redirects prepend one and it
    // would otherwise break JSON parsing.
    let buf = buf.trim_start_matches('\u{feff}');
    let data: Value = serde_json::from_str(buf).ok()?;

    let event = data
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let status = map_claude_status(&data, &event)?;

    post(ctx.port, &ctx.tab_id, &ctx.tool, &status, &event);
    Some(())
}

fn forward_codex(args: &[String]) -> Option<()> {
    // Codex appends the event JSON as the FINAL argv argument
    // (codex-rs/hooks/src/legacy_notify.rs). With our registered
    // `notify = ["<exe>", "__codex-notify"]`, argv is
    // [exe, "__codex-notify", "<json>"] so the payload is the last arg.
    // A malformed/absent payload simply fails to parse → no-op.
    let payload = args.last()?;
    let data: Value = serde_json::from_str(payload.trim_start_matches('\u{feff}')).ok()?;

    let ctx = HookCtx::from_env()?;
    // `notify` is global Codex config and fires for sessions started
    // outside Coffee CLI too — gate strictly on the tool tag.
    if ctx.tool != "codex" {
        return None;
    }

    let event = data
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let status = map_codex_status(&event)?;

    post(ctx.port, &ctx.tab_id, &ctx.tool, &status, &event);
    Some(())
}

/// Map a Claude Code hook event to a tab status. Mirrors
/// coffee-cli-hook.py exactly: Stop → idle, permission_prompt → wait_input,
/// idle_prompt → idle, everything else busy (working).
fn map_claude_status(data: &Value, event: &str) -> Option<String> {
    match event {
        "Stop" | "StopFailure" => Some("idle".to_string()),
        "Notification" => {
            // Claude has exposed the subtype under different keys across
            // versions — check all three the Python script checked.
            let ntype = data
                .get("notification_type")
                .and_then(|v| v.as_str())
                .or_else(|| data.get("type").and_then(|v| v.as_str()))
                .or_else(|| {
                    data.get("notification")
                        .and_then(|n| n.get("type"))
                        .and_then(|v| v.as_str())
                });
            match ntype {
                Some("permission_prompt") => Some("wait_input".to_string()),
                Some("idle_prompt") => Some("idle".to_string()),
                _ => None,
            }
        }
        // UserPromptSubmit / PreToolUse / PostToolUse / SubagentStart /
        // PreCompact / etc. (and a missing event name) → busy. One bucket,
        // one color.
        _ => Some("working".to_string()),
    }
}

/// Map a Codex `notify` event to a tab status. Codex only signals turn
/// completion (never turn start); unknown types are ignored, not guessed.
fn map_codex_status(event: &str) -> Option<String> {
    match event {
        "agent-turn-complete" => Some("idle".to_string()),
        _ => None,
    }
}

/// One TCP connection per event to the loopback hook server. Every error is
/// swallowed — the forwarder must never block the agent.
fn post(port: u16, tab_id: &str, tool: &str, status: &str, event: &str) {
    let payload = json!({
        "tab_id": tab_id,
        "tool": tool,
        "status": status,
        "event": event,
    });
    let _ = send(port, &payload);
}

fn send(port: u16, payload: &Value) -> std::io::Result<()> {
    let addr = format!("127.0.0.1:{}", port)
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "addr"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(format!("{}\n", payload).as_bytes())?;
    // Drain the server's tiny ack so it can close cleanly; ignore content.
    let mut ack = [0u8; 256];
    let _ = stream.read(&mut ack);
    Ok(())
}
