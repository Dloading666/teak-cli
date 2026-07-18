//! Per-tool integration registry — single source of truth for the
//! per-CLI facts Coffee CLI needs (binary name, skills dir, history
//! shape, hook surface, launch argv). Iterate `TOOLS` instead of
//! hardcoding lists in callers.
//!
//! Adding a new tool: create `src/tools/<id>.rs` with a `ToolDescriptor`
//! constant, register it in `TOOLS` below, and (if it has a hook
//! surface) add an arm to `hook_installer::dispatch_install`.

use std::path::{Path, PathBuf};

/// Where this tool stores its session history on disk and what
/// shape it lives in. Coffee CLI's history scanner (`server.rs`)
/// and message heatmap both consume this. Defaults are relative
/// to `$HOME` (`$USERPROFILE` on Windows); users override per-tool
/// via `~/.coffee-cli/tools.json` (`tool_config.history_path`).
///
/// Each variant maps to a different scanner / parser combination
/// in `server.rs`. New tool families (e.g. another SQLite-backed
/// CLI) get a new variant; CLIs whose layout matches an existing
/// family reuse the variant.
#[derive(Debug, Clone, Copy)]
pub enum HistoryShape {
    /// JSONL files at fixed scan depth, parsed by the generic
    /// `parse_agent_jsonl`. Used by Claude Code (depth 2 from
    /// `projects/`) and OpenClaw (depth 3 from `agents/`).
    GenericJsonl {
        root_under_home: &'static str,
        depth: u8,
    },

    /// Hermes Agent — flat directory of `session_*.json` files
    /// (JSON, not JSONL). Custom parser `parse_hermes_json`. No
    /// `root_under_home` because Hermes's data root is platform-
    /// dependent and runtime-overridable (`%LOCALAPPDATA%\hermes` on
    /// Windows, `~/.hermes` elsewhere, `$HERMES_HOME` if set) — see
    /// `crate::tools::hermes::hermes_home()`. `join_under` ignores
    /// its `home` argument for this variant.
    HermesFlatJson,

    /// Codex dated-rollout layout: `<YYYY>/<MM>/<DD>/rollout-*.jsonl`.
    /// Custom parser `parse_codex_session_jsonl`.
    CodexRollout {
        root_under_home: &'static str,
        depth: u8,
    },

    /// Qwen Code: `projects/<sanitized-cwd>/chats/<session>.jsonl`.
    /// Custom parser `parse_qwen_session_jsonl`.
    QwenProjects {
        root_under_home: &'static str,
        depth: u8,
    },

    /// Antigravity CLI (agy) — `tmp/<project-folder>/chats/session-*.jsonl`.
    /// Custom parser `parse_gemini_session_jsonl` (format inherited
    /// from the retired Gemini CLI; agy writes the same schema).
    /// Project-folder names resolve to real cwd via a sibling
    /// `projects.json` map (also Gemini-format, written by agy).
    AntigravityTmp {
        root_under_home: &'static str,
        depth: u8,
    },

    /// OpenCode: SQLite DB (`storage/db.sqlite`) plus legacy
    /// JSONL files. Walked by `find_opencode_sessions`, cannot be
    /// processed by the generic mtime-then-parse pipeline.
    OpenCodeMixed { root_under_home: &'static str },

    /// Kimi Code (`kimi` binary): NOT a dir of JSONL files and NOT SQLite.
    /// Sessions live under an index — `~/.kimi-code/session_index.jsonl`
    /// (one line per main session: `sessionId`/`sessionDir`/`workDir`) —
    /// with per-session metadata at `<sessionDir>/state.json` and the full
    /// conversation at `<sessionDir>/agents/main/wire.jsonl`. Data root is
    /// flat `~/.kimi-code/` on every OS (override `KIMI_CODE_HOME`); see
    /// `kimi_root` in server.rs. Like `OpenCodeMixed`, this bypasses the
    /// generic mtime-then-parse pipeline and is emitted by a bespoke
    /// second pass (`find_kimi_sessions` + `collect_kimi_heatmap_entries`).
    KimiIndex { root_under_home: &'static str },

    /// Grok Build (`grok` binary): per-session dirs at
    /// `~/.grok/sessions/<url-encoded-cwd>/<uuid>/`, each holding a
    /// `summary.json` index (title / cwd / timestamps / message counts)
    /// and a `chat_history.jsonl` conversation log. `GROK_HOME` overrides
    /// the base dir; see `grok_root` in server.rs. The session metadata
    /// lives in `summary.json` (not the JSONL filename/mtime), so this
    /// bypasses the generic mtime-then-parse pipeline and is emitted by a
    /// bespoke second pass (`find_grok_sessions` +
    /// `collect_grok_heatmap_entries`), mirroring `KimiIndex`.
    GrokSessions { root_under_home: &'static str },
}

impl HistoryShape {
    /// Default disk root for this tool's session history, relative
    /// to `$HOME`. Used by `tool_config::history_path_for` lookup.
    /// `None` for shapes whose root is not a `$HOME`-relative
    /// suffix (currently only `HermesFlatJson` — see `join_under`).
    pub fn root_under_home(&self) -> Option<&'static str> {
        match self {
            HistoryShape::GenericJsonl { root_under_home, .. }
            | HistoryShape::CodexRollout { root_under_home, .. }
            | HistoryShape::QwenProjects { root_under_home, .. }
            | HistoryShape::AntigravityTmp { root_under_home, .. }
            | HistoryShape::OpenCodeMixed { root_under_home }
            | HistoryShape::KimiIndex { root_under_home }
            | HistoryShape::GrokSessions { root_under_home } => Some(root_under_home),
            HistoryShape::HermesFlatJson => None,
        }
    }

    /// Resolve the shape's data root against a caller-provided home
    /// dir. Forward slashes in `root_under_home` are converted to
    /// the platform separator. For `HermesFlatJson` the `home`
    /// argument is ignored and `hermes::hermes_home().join("sessions")`
    /// is returned instead, since Hermes's root is platform-dependent
    /// (Windows uses `%LOCALAPPDATA%\hermes`, not `%USERPROFILE%\.hermes`).
    pub fn join_under(&self, home: &Path) -> PathBuf {
        match self {
            HistoryShape::HermesFlatJson => {
                crate::tools::hermes::hermes_home().join("sessions")
            }
            _ => {
                // Safe to unwrap: every other variant carries a literal.
                join_relative(home, self.root_under_home().unwrap_or(""))
            }
        }
    }

    /// JSONL scan depth, when the shape uses the mtime-then-parse
    /// pipeline. `None` for shapes that bypass it (HermesFlatJson
    /// uses a flat-dir collector; OpenCodeMixed uses SQLite; KimiIndex
    /// uses a session-index second pass).
    pub fn jsonl_depth(&self) -> Option<u8> {
        match self {
            HistoryShape::GenericJsonl { depth, .. }
            | HistoryShape::CodexRollout { depth, .. }
            | HistoryShape::QwenProjects { depth, .. }
            | HistoryShape::AntigravityTmp { depth, .. } => Some(*depth),
            HistoryShape::HermesFlatJson
            | HistoryShape::OpenCodeMixed { .. }
            | HistoryShape::KimiIndex { .. }
            | HistoryShape::GrokSessions { .. } => None,
        }
    }
}

/// Join a forward-slash-relative path under `home`, converting to the
/// platform separator. Use for any registry-derived path — Windows APIs
/// mostly tolerate mixed separators, but normalising at construction
/// time avoids surprises in display strings, glob comparisons, and
/// downstream string-matching.
pub(crate) fn join_relative(home: &Path, rel: &str) -> PathBuf {
    if std::path::MAIN_SEPARATOR == '/' {
        home.join(rel)
    } else {
        home.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}

/// One static fact-bundle per supported AI CLI. Pure data; behaviours
/// (hook installation, history parsing, …) live in dedicated modules
/// below — each tool gets its own file under `src/tools/<id>.rs`.
///
/// Adding a new tool = create `src/tools/<id>.rs`, write its
/// `ToolDescriptor` constant, add it to `TOOLS` below.
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    /// Stable internal id used in protocol payloads and frontend
    /// `ToolType` discriminants. Must match the user-visible CLI
    /// name (claude → "claude", openclaw → "openclaw").
    pub id: &'static str,

    /// Display name shown in launchpad cards / tool pickers /
    /// history rows. Frontend pulls these via the `list_tools`
    /// IPC; see `src-ui/src/lib/tool-info.ts`. Always required —
    /// pseudo-tools without a brand name (terminal / remote) are
    /// not registered here and use locale-specific labels in i18n.
    pub display_name: &'static str,

    /// Binary name to look up via `where` (Windows) / `which`
    /// (Unix). Single source of truth for "is this tool on PATH".
    pub binary_name: &'static str,

    /// Where this tool's enabled skills should be junctioned, as a
    /// path relative to the user's home directory (forward-slash).
    /// Three layout families exist (dotdir / XDG / workspace-nested);
    /// each tool encodes its own. `None` = tool doesn't have a
    /// skills concept yet. Always resolve via [`Self::skill_dir`]
    /// instead of `home.join(skill_dir_relative)` so platform-
    /// specific home overrides (Hermes Agent on Windows uses
    /// `%LOCALAPPDATA%\hermes` instead of `~/.hermes`) are honored.
    #[allow(dead_code)] // read only by skill_dir() below, which isn't wired to a call site yet
    pub skill_dir_relative: Option<&'static str>,

    /// `true` if Coffee CLI installs a status-indicator hook for
    /// this tool. Drives `hook_installer::dispatch_install`. Tools
    /// without a hook surface (Antigravity / Qwen / OpenClaw today)
    /// still participate in ChangesBoard because the snapshot diff is
    /// tool-agnostic — only the live tab status dot is unavailable.
    pub has_hook_surface: bool,

    /// Shape of this tool's on-disk session history. `None` =
    /// tool doesn't expose a scannable history (no entries on
    /// the History board, no contributions in the heatmap).
    /// Currently every registered CLI has a history; field is
    /// optional for future tools that may not.
    pub history_shape: Option<HistoryShape>,

    /// Argv prepended to every spawn of this tool *before* any
    /// multi-agent flags or user-configured `extra_args`. Used
    /// for CLIs whose primary REPL is a subcommand of the binary
    /// — e.g. OpenClaw's TUI is `openclaw tui`, not bare
    /// `openclaw`. Most tools have an empty list.
    pub default_args: &'static [&'static str],
}

impl ToolDescriptor {
    /// Absolute path to this tool's skills directory, or `None` if
    /// the tool has no skills concept.
    ///
    /// For most tools this is simply `<home>/<skill_dir_relative>`.
    /// Hermes Agent diverges: its home is `%LOCALAPPDATA%\hermes`
    /// on Windows (set by `install.ps1`) and `~/.hermes` elsewhere,
    /// so we route through `tools::hermes::hermes_home()` and ignore
    /// the caller-supplied `home`. `$HERMES_HOME` overrides both
    /// when set.
    ///
    /// Always prefer this over `home.join(t.skill_dir_relative?)`
    /// at call sites — that pattern silently breaks on Windows for
    /// Hermes.
    #[allow(dead_code)] // documented ToolDescriptor API; not wired to a call site yet
    pub fn skill_dir(&self, home: &Path) -> Option<PathBuf> {
        let rel = self.skill_dir_relative?;
        if self.id == hermes::DESCRIPTOR.id {
            // `rel` is the source of truth even on Hermes — we just swap
            // the root from `home` to `hermes_home()`. If the upstream
            // layout ever moves the skills dir, updating
            // `skill_dir_relative` in hermes.rs is enough.
            Some(hermes::hermes_home().join(rel))
        } else {
            Some(join_relative(home, rel))
        }
    }
}

mod antigravity;
mod claude;
mod codex;
mod grok;
pub mod hermes;
mod mimocode;
mod openclaw;
mod opencode;
mod qwen;
// Lower-tier tools — see each module's doc comment for its tier (Pi is T2
// with history; Aider/Crush/Goose/Copilot are T3 launch-only, no
// history/hook surface). Kimi Code was promoted to T1 (hook-wired).
mod aider;
mod copilot;
mod crush;
mod goose;
mod kimicode;
mod pi;

/// All supported AI CLIs. Order matches launchpad layout (claude
/// first, then codex, …). Iterate this when you need to do
/// something for every tool — don't hardcode lists in callers.
pub static TOOLS: &[&ToolDescriptor] = &[
    &claude::DESCRIPTOR,
    &codex::DESCRIPTOR,
    &grok::DESCRIPTOR,
    &opencode::DESCRIPTOR,
    &antigravity::DESCRIPTOR,
    &qwen::DESCRIPTOR,
    &openclaw::DESCRIPTOR,
    &hermes::DESCRIPTOR,
    // MiMo Code is fully wired (launchpad tile in CenterPanel's AGENT_CATALOG,
    // resume preset in terminal.rs, history/heatmap second pass in server.rs).
    // Order here doesn't affect the launchpad (that list is hardcoded in the
    // frontend); it only needs to be in the registry for list_tools + scanning.
    &mimocode::DESCRIPTOR,
    // Pi is T2 (history + heatmap + changes + resume; see its module doc).
    // Kimi Code is T1 — hook-wired via ~/.kimi-code/config.toml `[[hooks]]`
    // (install_kimi), same T2 features as Pi otherwise. The other four —
    // Crush / Aider / Goose / Copilot — are T3 launch-only: display name +
    // PATH probe + launch binary, history_shape: None and
    // has_hook_surface: false.
    &pi::DESCRIPTOR,
    &crush::DESCRIPTOR,
    &aider::DESCRIPTOR,
    &kimicode::DESCRIPTOR,
    &goose::DESCRIPTOR,
    &copilot::DESCRIPTOR,
];

/// Lookup by id. `None` if the id isn't registered. Used by hook
/// dispatch (where the `tool` field arrives as a string from a
/// Python/JS forwarder) and by the launchpad's per-tool actions.
pub fn find(id: &str) -> Option<&'static ToolDescriptor> {
    TOOLS.iter().find(|t| t.id == id).copied()
}

/// Frontend-facing summary of a registered tool. Returned by the
/// `list_tools` IPC so the UI can pull display names off the registry
/// instead of hardcoding label tables in every component.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub id: &'static str,
    pub display_name: &'static str,
}

#[tauri::command]
pub fn list_tools() -> Vec<ToolInfo> {
    TOOLS
        .iter()
        .map(|t| ToolInfo {
            id: t.id,
            display_name: t.display_name,
        })
        .collect()
}
