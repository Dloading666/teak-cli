//! MiMo Code — Xiaomi's OpenCode fork (`mimo` binary).
//!
//! Shares OpenCode's Drizzle SQLite schema, so history / heatmap / reading
//! reuse the OpenCode readers with a different db path:
//!   - `~/.local/share/mimocode/mimocode.db` (primary)
//!   - `~/.config/mimocode/mimocode.db` (fallback)
//! See server.rs `mimocode_db`, `find_drizzle_sessions_sqlite`, and
//! `read_mimocode_session`.
//!
//! Two deliberate divergences from OpenCode:
//!   - `has_hook_surface: false` — unlike OpenCode, MiMo Code adapts its TUI
//!     background on launch on its own, so it must stay OUT of the install
//!     dispatch (no `ensure_opencode_tui_theme_default` write). OpenCode still
//!     needs that override; MiMo Code does not.
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
    // See module doc — MiMo Code self-themes; keep it out of hook dispatch.
    has_hook_surface: false,
    // Same shape as OpenCode so the registry scan skips it (its SQLite
    // second pass in server.rs emits finished SavedSessions instead).
    history_shape: Some(HistoryShape::OpenCodeMixed {
        root_under_home: ".local/share/mimocode",
    }),
    default_args: &[],
};
