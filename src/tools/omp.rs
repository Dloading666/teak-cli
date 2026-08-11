//! Oh-My-Pi (`omp` binary, omp.sh) — third-class (T3) integration: brand icon +
//! one-click launch only.
//!
//! No session-history scanning and no hook surface (so no Dynamic Island /
//! live status dot — that stays Claude-only). Registered in `TOOLS` purely so
//! the launchpad gets its display name (`list_tools`), its PATH "installed"
//! probe, and the launch binary. `binary_name` is the command users run
//! (`bun install -g @oh-my-pi/pi-coding-agent` / omp.sh installer, binary `omp`).
//!
//! Distinct from the original Pi (`pi` binary, T2 with history) — Oh-My-Pi is a
//! fork with its own binary and data layout; it keeps the `omp` id to avoid
//! colliding with the existing `pi` tool.

use super::ToolDescriptor;

pub static DESCRIPTOR: ToolDescriptor = ToolDescriptor {
    id: "omp",
    display_name: "Oh-My-Pi",
    binary_name: "omp",
    has_legacy_hook_artifacts: false,
    history_shape: None,
    default_args: &[],
};
