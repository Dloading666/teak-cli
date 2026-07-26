//! Grok Build (xAI) - `grok` binary.
//!
//! T2 tier - launchpad tile + resume + history + heatmap + changes, but NO
//! dynamic island. Closed-source login app; full T1 hook integration was
//! tried and rolled back - the hook forwarder blocked grok's startup (grok
//! fires `<exe> __grok-hook` on every event and waits, stalling the TUI).
//! Like codex/hermes/antigravity/qwen: a fake static-green island via
//! TAB_STATUS_TOOLS, no live status bus. `has_hook_surface: false` makes
//! hook_installer skip it entirely.
//!
//! Session storage (verified against on-disk files 2026-07-10): each session
//! lives in its own dir `~/.grok/sessions/<url-encoded-cwd>/<uuid>/` with a
//! `summary.json` index (title / cwd / timestamps / message counts) and a
//! `chat_history.jsonl` conversation log. `GROK_HOME` overrides the base dir.
//! This nested dir + summary-index layout doesn't fit GenericJsonl (flat
//! JSONL files) nor a SQLite shape, so it gets its own HistoryShape variant
//! and a bespoke second pass in server.rs (`find_grok_sessions` /
//! `collect_grok_heatmap_entries`), mirroring the Kimi index second pass.
//!
//! Resume: `grok --resume <uuid>` (UUIDv7). The token is sourced from the
//! history scanner (summary.json `info.id`), not PTY scraping. Coffee CLI
//! launches grok raw - no auth injection, no hook config; grok authenticates
//! itself via `~/.grok/auth.json` (native OAuth, verified working standalone).

use super::{HistoryShape, ToolDescriptor};

pub static DESCRIPTOR: ToolDescriptor = ToolDescriptor {
    id: "grok",
    display_name: "Grok Build",
    binary_name: "grok",
    skill_dir_relative: Some(".grok/skills"),
    has_hook_surface: false,
    // ~/.grok/sessions/<encoded-cwd>/<uuid>/{summary.json, chat_history.jsonl}
    // Bespoke second pass - bypasses the generic mtime-then-parse pipeline
    // (the session metadata is in summary.json, not the JSONL filename/mtime).
    history_shape: Some(HistoryShape::GrokSessions {
        root_under_home: ".grok/sessions",
    }),
    // `--minimal`: scrollback-native render mode. Grok's fullscreen TUI paints
    // its own 24-bit RGB background (GrokNight = pure black), bypassing the
    // xterm.js default bg — so Coffee CLI's theme/wallpaper never shows through
    // (same family as the OpenCode TUI transparency issue). In `--minimal` the
    // palette is `Theme::terminal_default()`: every bg_* field is `Color::Reset`,
    // so grok draws on the terminal's own background and Coffee CLI's Glass /
    // wallpaper shows through. Same fix as OpenCode's `lucent-orng` 4-bg-slot
    // theme, just expressed as a render mode.
    //
    // OSC 11 is NOT a path here: grok's `osc11.rs` returns `None` on Windows
    // (`#[cfg(not(unix))]` branch, same as codex), so an xterm-side OSC handler
    // is dead code on the primary platform. `--minimal` is the only source fix.
    //
    // Trade-off: minimal mode drops the interactive ScrollbackPane (fold /
    // in-app selection / mouse canvas) — but that pane's mouse interaction was
    // already impaired under WebView2/xterm.js (alt-screen mouse/WebGL smear
    // family, see PR #110), and the user can `/fullscreen` back per-session.
    // Compatible with `--resume` (verified in `grok --help`).
    default_args: &["--minimal"],
};
