//! MiMo Code — Xiaomi's OpenCode fork (`mimo` binary).
//!
//! Shares OpenCode's Drizzle SQLite schema, so history / heatmap / reading
//! reuse the OpenCode readers with a different db path:
//!   - `~/.local/share/mimocode/mimocode.db` (primary)
//!   - `~/.config/mimocode/mimocode.db` (fallback)
//! See server.rs `mimocode_db`, `find_drizzle_sessions_sqlite`, and
//! `read_mimocode_session`.
//!
//! Divergences / notes vs OpenCode:
//!   - `has_hook_surface: true` — MiMo Code does NOT self-theme its TUI (an
//!     earlier assumption, disproven 2026-06-22: it ships the same opaque #000
//!     default canvas as OpenCode). So it joins the install dispatch purely to
//!     get the `ensure_opencode_tui_theme_default(home, "mimocode")` write that
//!     stamps `~/.config/mimocode/tui.json` with the transparent `lucent-orng`
//!     theme. It does not get the OpenCode island/notify plugin.
//!   - `binary_name: "mimo"` is a best-guess pending confirmation; correct it
//!     here if MiMo Code ships under a different command name.
//!
//! Registering here gives MiMo Code a display name (via `list_tools`) and wires
//! history scanning. The launchpad tile + resume are wired separately in the
//! frontend's hardcoded AGENT_CATALOG (CenterPanel) and terminal.rs
//! AGENT_PRESETS — all three now present, so MiMo Code is fully launchable.

use super::{HistoryShape, ToolDescriptor};

pub static DESCRIPTOR: ToolDescriptor = ToolDescriptor {
    id: "mimocode",
    display_name: "MiMo Code",
    binary_name: "mimo",
    // Skills mirror not wired for MiMo Code yet (would be
    // `.config/mimocode/skills` if it mirrors OpenCode's layout).
    skill_dir_relative: None,
    // See module doc — MiMo Code ships an opaque #000 TUI default just like
    // OpenCode, so it needs the tui.json transparency write via dispatch.
    has_hook_surface: true,
    // Same shape as OpenCode so the registry scan skips it (its SQLite
    // second pass in server.rs emits finished SavedSessions instead).
    history_shape: Some(HistoryShape::OpenCodeMixed {
        root_under_home: ".local/share/mimocode",
    }),
    default_args: &[],
};
