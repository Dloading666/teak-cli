//! Kimi Code — third-class (T3) integration: brand icon + one-click launch only.
//!
//! No session-history scanning and no hook surface (so no Dynamic Island /
//! live status dot — that stays Claude-only). Registered in `TOOLS` purely so
//! the launchpad gets its display name (`list_tools`), its PATH "installed"
//! probe, and the launch binary. `binary_name` is the command users run.

use super::ToolDescriptor;

pub static DESCRIPTOR: ToolDescriptor = ToolDescriptor {
    id: "kimicode",
    display_name: "Kimi Code",
    binary_name: "kimi",
    skill_dir_relative: None,
    has_hook_surface: false,
    history_shape: None,
    default_args: &[],
};
