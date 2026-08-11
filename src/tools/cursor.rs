//! Cursor CLI (Anysphere) — third-class (T3) integration: brand icon + one-click
//! launch only.
//!
//! No session-history scanning and no hook surface (so no Dynamic Island /
//! live status dot — that stays Claude-only). Registered in `TOOLS` purely so
//! the launchpad gets its display name (`list_tools`), its PATH "installed"
//! probe, and the launch binary. `binary_name` is the command users run.
//!
//! The command is `agent` — NOT `cursor`. Cursor's CLI ships as `cursor-agent`
//! under the hood, but the installer (macOS/Linux `~/.local/bin/agent`,
//! Windows `%LocalAppData%\cursor-agent\agent.exe`, PATH set automatically)
//! always exposes BOTH `agent` (primary) and `cursor-agent` (legacy alias),
//! and the docs/installer tell users to run `agent`. We mirror the documented
//! primary name.

use super::ToolDescriptor;

pub static DESCRIPTOR: ToolDescriptor = ToolDescriptor {
    id: "cursor",
    display_name: "Cursor",
    binary_name: "agent",
    has_legacy_hook_artifacts: false,
    history_shape: None,
    default_args: &[],
};
