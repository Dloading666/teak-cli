//! Shell detection + selection.
//!
//! Replaces the old hardcoded `binary_on_path("pwsh.exe") ? "pwsh.exe" :
//! "powershell.exe"` fallback in `server.rs` with an explicit, user-visible
//! shell picker. The picker is fed by `ShellCapabilities` (probed once on
//! app start via `detect_shells`) and the chosen `ShellId` is resolved to a
//! concrete `(program, args)` at spawn time by `resolve_shell`.
//!
//! Why this module exists beyond the old probe:
//!   - **Execution Alias trap**: `pwsh.exe` installed from the Microsoft
//!     Store leaves a 0-byte App Execution Alias reparse point on PATH.
//!     `where pwsh.exe` finds it (so the old `binary_on_path` probe said
//!     "installed"), but `CreateProcessW` on that stub returns
//!     `ERROR_ACCESS_DENIED` — the spawned shell never starts. We now
//!     resolve to a real absolute path and treat 0-byte aliases as
//!     "not installed", so the user either gets a working pwsh or an
//!     honest "not detected" — never a dead tab.
//!   - **Git Bash lives off PATH**: most Git-for-Windows installs that
//!     don't tick "add to PATH" put bash.exe only under Program Files or
//!     %LOCALAPPDATA%\Programs\Git. `where bash.exe` misses those, so we
//!     probe the known install locations directly (mirrors the existing
//!     SHELL-env hint in `terminal.rs`).
//!   - **Decoupled detection** (borrowed from a reference terminal app):
//!     the picker consumes capability flags, never paths. So a future
//!     remote-shell feature can answer the same flags from a different
//!     source without touching the picker UI.
//!
//! WSL detection is probed here too (`wsl_available`) but intentionally
//! NOT exposed in the picker yet — launching a WSL shell needs cwd
//! translation (`\\wsl.localhost\<distro>\` ↔ `/mnt/...`) and a distro
//! list that this first pass doesn't wire up. Left as a capability flag
//! so the second-phase UI can flip on without re-probing.

#![allow(dead_code)] // wsl_available + some arms are second-phase / per-platform

use serde::Serialize;

/// User-facing shell identity. Stored as a string id (the persisted form
/// under `cc-default-shell`), resolved to a concrete program at spawn.
/// `Auto` keeps the historical fallback behavior (pwsh → powershell →
/// cmd on Windows; `$SHELL` → bash on Unix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellId {
    Auto,
    #[cfg(target_os = "windows")]
    Pwsh,
    #[cfg(target_os = "windows")]
    Powershell,
    #[cfg(target_os = "windows")]
    GitBash,
    #[cfg(target_os = "windows")]
    Cmd,
    #[cfg(not(target_os = "windows"))]
    Zsh,
    #[cfg(not(target_os = "windows"))]
    Bash,
    #[cfg(not(target_os = "windows"))]
    Fish,
    #[cfg(not(target_os = "windows"))]
    Sh,
}

impl ShellId {
    /// Parse the persisted string id. `None` / empty / unrecognized →
    /// `Auto`, so a stale `cc-default-shell` from a removed shell falls
    /// back to the safe default instead of spawning something missing.
    pub fn from_opt(s: &Option<String>) -> Self {
        match s.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => Self::Auto,
            #[cfg(target_os = "windows")]
            Some("pwsh") => Self::Pwsh,
            #[cfg(target_os = "windows")]
            Some("powershell") => Self::Powershell,
            #[cfg(target_os = "windows")]
            Some("git-bash") => Self::GitBash,
            #[cfg(target_os = "windows")]
            Some("cmd") => Self::Cmd,
            #[cfg(not(target_os = "windows"))]
            Some("zsh") => Self::Zsh,
            #[cfg(not(target_os = "windows"))]
            Some("bash") => Self::Bash,
            #[cfg(not(target_os = "windows"))]
            Some("fish") => Self::Fish,
            #[cfg(not(target_os = "windows"))]
            Some("sh") => Self::Sh,
            _ => Self::Auto,
        }
    }
}

/// Probed availability of the optional shells. Inbox shells
/// (`powershell.exe` / `cmd.exe` on Windows) are assumed present and NOT
/// probed, so the picker always offers them. On Unix every candidate is
/// probed via `which` — zsh/bash/sh are near-universal but fish is not
/// installed by default on macOS, so we only show what's actually there.
/// Only the "might genuinely be absent" ones get a real probe.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ShellCapabilities {
    /// PowerShell 7 (`pwsh.exe`) resolved to a real absolute path (0-byte
    /// App Execution Aliases don't count). `false` when only the inbox
    /// 5.1 `powershell.exe` is available.
    #[cfg(target_os = "windows")]
    pub pwsh_available: bool,
    /// Exact version string of PowerShell 7, e.g. "7.4.6" (from
    /// `pwsh --version`). Surfaced in the picker so users see which build
    /// they're picking — most want 7 over 5, and seeing the real version
    /// builds confidence the detection is live, not a guess. `None` when
    /// pwsh isn't installed or the version probe failed.
    #[cfg(target_os = "windows")]
    pub pwsh_version: Option<String>,
    /// Exact version string of inbox PowerShell 5.1, e.g. "5.1.22621.4116"
    /// (from `$PSVersionTable.PSVersion`). `None` if the probe failed —
    /// the card still shows (5.1 is always present on supported Windows)
    /// but without a version suffix.
    #[cfg(target_os = "windows")]
    pub powershell_version: Option<String>,
    /// Absolute path to Git-for-Windows `bash.exe`, when found at any of
    /// the standard install locations. `None` if Git Bash isn't installed
    /// (or is installed somewhere we don't look — accepted gap; `Auto`
    /// still works, and a custom-path override can come in a later phase).
    #[cfg(target_os = "windows")]
    pub git_bash_path: Option<String>,
    /// `wsl.exe` present on PATH. Probed now so the second-phase WSL UI
    /// can flip on without re-plumbing detection; NOT exposed in the
    /// picker this phase.
    pub wsl_available: bool,
    /// Resolved pwsh absolute path (when `pwsh_available`), kept so
    /// `resolve_shell` can spawn the real file instead of the alias.
    /// Not serialized to the frontend — the picker only needs the bool.
    #[cfg(target_os = "windows")]
    #[serde(skip)]
    pub pwsh_path: Option<String>,
    /// Unix candidate shells resolved by `which`. Each is `true` when
    /// present, so the picker only offers shells the user can actually
    /// spawn — fish is absent by default on macOS, so we hide it instead
    /// of offering a dead card. bash is near-universal so `Auto`'s
    /// fallback still works even when zsh isn't on PATH.
    #[cfg(not(target_os = "windows"))]
    pub zsh_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub bash_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub fish_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub sh_available: bool,
}

/// JSON shape shipped to the frontend — a mirror of `ShellCapabilities`
/// minus the private path fields (the picker shows booleans only).
#[derive(Debug, Clone, Serialize)]
pub struct ShellCapabilitiesJson {
    #[cfg(target_os = "windows")]
    pub pwsh_available: bool,
    #[cfg(target_os = "windows")]
    pub pwsh_version: Option<String>,
    #[cfg(target_os = "windows")]
    pub powershell_version: Option<String>,
    #[cfg(target_os = "windows")]
    pub git_bash_available: bool,
    pub wsl_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub zsh_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub bash_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub fish_available: bool,
    #[cfg(not(target_os = "windows"))]
    pub sh_available: bool,
}

impl From<&ShellCapabilities> for ShellCapabilitiesJson {
    fn from(c: &ShellCapabilities) -> Self {
        Self {
            #[cfg(target_os = "windows")]
            pwsh_available: c.pwsh_available,
            #[cfg(target_os = "windows")]
            pwsh_version: c.pwsh_version.clone(),
            #[cfg(target_os = "windows")]
            powershell_version: c.powershell_version.clone(),
            #[cfg(target_os = "windows")]
            git_bash_available: c.git_bash_path.is_some(),
            wsl_available: c.wsl_available,
            #[cfg(not(target_os = "windows"))]
            zsh_available: c.zsh_available,
            #[cfg(not(target_os = "windows"))]
            bash_available: c.bash_available,
            #[cfg(not(target_os = "windows"))]
            fish_available: c.fish_available,
            #[cfg(not(target_os = "windows"))]
            sh_available: c.sh_available,
        }
    }
}

/// Probe the host for optional shells. Cheap (a few `where`/`exists`
/// checks); safe to call once on app start and cache for the process
/// lifetime — re-install-during-run is rare, and `Auto` always works.
/// Does NOT probe exact versions — that's a slower step (powershell.exe
/// takes ~1-2s to report $PSVersionTable) gated to `populate_versions()`,
/// called only when the Settings picker opens, so app startup and every
/// terminal spawn stay fast.
pub fn detect_capabilities() -> ShellCapabilities {
    let mut caps = ShellCapabilities::default();
    #[cfg(target_os = "windows")]
    {
        if let Some(path) = find_pwsh_real_path() {
            caps.pwsh_available = true;
            caps.pwsh_path = Some(path);
        }
        caps.git_bash_path = find_git_bash();
        caps.wsl_available = crate::server::check_tool_windows("wsl.exe");
    }
    #[cfg(not(target_os = "windows"))]
    {
        // WSL is Windows-only. Probe the four Unix candidate shells via
        // `which` so the picker hides absent ones (fish isn't installed by
        // default on macOS). Cheap, runs once on settings open.
        caps.wsl_available = false;
        caps.zsh_available = crate::server::check_tool_unix("zsh");
        caps.bash_available = crate::server::check_tool_unix("bash");
        caps.fish_available = crate::server::check_tool_unix("fish");
        caps.sh_available = crate::server::check_tool_unix("sh");
    }
    caps
}

/// Fill in exact version strings for the Windows PowerShell shells. Called
/// only from `detect_shells` (Settings picker open), NOT from startup —
/// `powershell.exe -Command $PSVersionTable` takes ~1-2s, which would
/// stall app boot and every terminal spawn if it ran in
/// `detect_capabilities`. Each probe is CREATE_NO_WINDOW so opening
/// Settings doesn't flash console windows.
#[cfg(target_os = "windows")]
pub fn populate_versions(caps: &mut ShellCapabilities) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // PowerShell 7: `pwsh --version` prints "PowerShell 7.4.6" and exits
    // fast (~150ms, single-file .NET app). Only probe if we already found a
    // real pwsh path (avoids re-triggering the App Execution Alias trap).
    if caps.pwsh_available {
        if let Some(path) = caps.pwsh_path.as_ref() {
            caps.pwsh_version = Command::new(path)
                .arg("--version")
                .creation_flags(0x08000000)
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        // "PowerShell 7.4.6" → take the last whitespace token.
                        s.split_whitespace().last().map(|v| v.to_string())
                    } else {
                        None
                    }
                });
        }
    }

    // PowerShell 5.1: `$PSVersionTable.PSVersion.ToString()` prints
    // "5.1.22621.4116". powershell.exe cold-starts slowly, so this is the
    // probe that motivated gating versions out of startup. Always probe
    // (5.1 is inbox on every supported Windows) — a failure just leaves
    // the version None and the card shows without a suffix.
    caps.powershell_version = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$PSVersionTable.PSVersion.ToString()",
        ])
        .creation_flags(0x08000000)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            } else {
                None
            }
        });
}

#[cfg(not(target_os = "windows"))]
pub fn populate_versions(_caps: &mut ShellCapabilities) {
    // Unix shells don't carry a meaningful single "version" in the picker —
    // zsh/bash/fish versions are rarely actionable for the user. No-op so
    // the Settings command can call this unconditionally across platforms.
}

/// `(program, args)` handed to `terminal::spawn`. The program is an
/// absolute path on Windows (defeating App Execution Aliases) and a
/// resolved bare name / path on Unix. Args match each shell's interactive
/// convention.
pub fn resolve_shell(id: ShellId, caps: &ShellCapabilities) -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        match id {
            ShellId::Pwsh => {
                // Caller only offers pwsh when pwsh_available, so the
                // resolved path is present. Fall back to bare name if the
                // capability somehow went stale — never panic on spawn.
                let p = caps.pwsh_path.clone().unwrap_or_else(|| "pwsh.exe".to_string());
                (p, vec!["-NoExit".to_string()])
            }
            ShellId::Powershell => {
                // Inbox 5.1 — resolve to its well-known absolute location so
                // a stripped PATH (issue #30) can't break it.
                let p = system_powershell_path();
                (p, vec!["-NoExit".to_string()])
            }
            ShellId::GitBash => {
                let p = caps.git_bash_path.clone().unwrap_or_else(|| "bash.exe".to_string());
                (p, vec!["--login".to_string(), "-i".to_string()])
            }
            ShellId::Cmd => {
                let p = system_cmd_path();
                (p, vec![])
            }
            ShellId::Auto => auto_windows(caps),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        match id {
            ShellId::Zsh => ("zsh".to_string(), vec!["-l".to_string(), "-i".to_string()]),
            ShellId::Bash => ("bash".to_string(), vec!["-l".to_string(), "-i".to_string()]),
            ShellId::Fish => ("fish".to_string(), vec!["-l".to_string(), "-i".to_string()]),
            ShellId::Sh => ("sh".to_string(), vec![]),
            ShellId::Auto => auto_unix(),
        }
    }
}

// ── Windows probes ─────────────────────────────────────────────────────
#[cfg(target_os = "windows")]
fn auto_windows(caps: &ShellCapabilities) -> (String, Vec<String>) {
    // pwsh 7 (resolved real path) → inbox powershell 5.1. Mirrors the
    // pre-feature behavior but with an absolute path that sidesteps the
    // App Execution Alias trap.
    if caps.pwsh_available {
        let p = caps.pwsh_path.clone().unwrap_or_else(|| "pwsh.exe".to_string());
        return (p, vec!["-NoExit".to_string()]);
    }
    (system_powershell_path(), vec!["-NoExit".to_string()])
}

#[cfg(target_os = "windows")]
fn system_powershell_path() -> String {
    // %SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe is present
    // on every supported Windows; resolving it explicitly avoids a bare
    // "powershell.exe" PATH lookup failing on a stripped PATH (issue #30).
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let p = format!(r"{}\System32\WindowsPowerShell\v1.0\powershell.exe", sysroot);
    if std::path::Path::new(&p).exists() {
        p
    } else {
        "powershell.exe".to_string()
    }
}

#[cfg(target_os = "windows")]
fn system_cmd_path() -> String {
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    let p = format!(r"{}\System32\cmd.exe", sysroot);
    if std::path::Path::new(&p).exists() {
        p
    } else {
        "cmd.exe".to_string()
    }
}

/// Find PowerShell 7 (`pwsh.exe`) as a REAL executable, skipping the
/// 0-byte App Execution Alias reparse point the Microsoft Store leaves
/// on PATH. Strategy: run `where pwsh.exe`, take each returned path,
/// keep the first whose file size is > 0 (aliases are 0 bytes). Returns
/// the absolute path, or `None` when only the alias is present (treated
/// as "not installed" so the picker doesn't offer a dead shell).
#[cfg(target_os = "windows")]
fn find_pwsh_real_path() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("where")
        .arg("pwsh.exe")
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        let p = line.trim();
        if p.is_empty() {
            continue;
        }
        // Alias stubs are 0-byte reparse points; a real pwsh is several MB.
        // `metadata().len()` follows the reparse point for a normal file
        // but returns 0 for the alias stub, so >0 filters them out.
        if let Ok(meta) = std::fs::metadata(p) {
            if meta.is_file() && meta.len() > 0 {
                return Some(p.to_string());
            }
        }
    }
    None
}

/// Locate Git-for-Windows `bash.exe`. Avoids `where bash.exe` because many
/// Git installs don't add Git to PATH. Probes the three standard install
/// roots (Program Files x64/x86 + %LOCALAPPDATA%\Programs\Git) plus a
/// PATH scan for any `Git`/`PortableGit` directory's `bin\bash.exe`.
/// Mirrors the SHELL-hint candidates in `terminal.rs` and generalizes.
#[cfg(target_os = "windows")]
fn find_git_bash() -> Option<String> {
    // Fixed well-known locations (same set terminal.rs uses for the
    // SHELL env hint — keep them in sync if one changes).
    let mut candidates: Vec<String> = vec![
        r"C:\Program Files\Git\bin\bash.exe".to_string(),
        r"C:\Program Files (x86)\Git\bin\bash.exe".to_string(),
    ];
    if let Ok(la) = std::env::var("LOCALAPPDATA") {
        candidates.push(format!(r"{}\Programs\Git\bin\bash.exe", la));
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        candidates.push(format!(r"{}\Git\bin\bash.exe", pf));
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        candidates.push(format!(r"{}\Git\bin\bash.exe", pf86));
    }
    for c in &candidates {
        if std::path::Path::new(c).exists() {
            return Some(c.clone());
        }
    }
    // Last resort: walk PATH segments looking for a dir whose name contains
    // "Git" or "PortableGit" and has bin\bash.exe — covers scoop / custom
    // install roots without hardcoding them.
    if let Some(path) = std::env::var_os("PATH") {
        for seg in std::env::split_paths(&path) {
            let Some(name) = seg.file_name().and_then(|n| n.to_str()) else { continue };
            let lower = name.to_ascii_lowercase();
            if !lower.contains("git") && !lower.contains("portablegit") {
                continue;
            }
            let bash = seg.join("bin").join("bash.exe");
            if bash.exists() {
                return Some(bash.to_string_lossy().to_string());
            }
        }
    }
    None
}

// ── Unix auto ───────────────────────────────────────────────────────────
#[cfg(not(target_os = "windows"))]
fn auto_unix() -> (String, Vec<String>) {
    // Same logic the old server.rs fallback used, kept verbatim so
    // existing Unix users see no behavior change unless they pick a
    // specific shell in settings.
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .filter(|s| std::path::Path::new(s).exists())
        .unwrap_or_else(|| "bash".to_string());
    let basename = std::path::Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut args = vec!["-l".to_string(), "-i".to_string()];
    // OSC 7 cwd reporting hook for fish — bash gets it via PROMPT_COMMAND
    // (injected in terminal.rs); zsh has no clean flag hook. Preserved
    // from the old server.rs branch so fish users keep cd-follow.
    if basename == "fish" {
        args.push("-C".to_string());
        args.push(
            r#"function __coffee_osc7 --on-variable PWD; printf '\033]7;file://%s%s\033\\' (hostname) "$PWD"; end; __coffee_osc7"#
                .to_string(),
        );
    }
    (shell, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_default_for_unknown_string() {
        assert_eq!(ShellId::from_opt(&None), ShellId::Auto);
        assert_eq!(ShellId::from_opt(&Some(String::new())), ShellId::Auto);
        assert_eq!(ShellId::from_opt(&Some("nonsense".into())), ShellId::Auto);
        assert_eq!(ShellId::from_opt(&Some("  ".into())), ShellId::Auto);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn parses_windows_ids() {
        assert_eq!(ShellId::from_opt(&Some("pwsh".into())), ShellId::Pwsh);
        assert_eq!(ShellId::from_opt(&Some("powershell".into())), ShellId::Powershell);
        assert_eq!(ShellId::from_opt(&Some("git-bash".into())), ShellId::GitBash);
        assert_eq!(ShellId::from_opt(&Some("cmd".into())), ShellId::Cmd);
        // A Unix id on Windows is unrecognized → Auto (cross-OS prefs don't apply).
        assert_eq!(ShellId::from_opt(&Some("zsh".into())), ShellId::Auto);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn parses_unix_ids() {
        assert_eq!(ShellId::from_opt(&Some("zsh".into())), ShellId::Zsh);
        assert_eq!(ShellId::from_opt(&Some("bash".into())), ShellId::Bash);
        assert_eq!(ShellId::from_opt(&Some("fish".into())), ShellId::Fish);
        assert_eq!(ShellId::from_opt(&Some("sh".into())), ShellId::Sh);
        assert_eq!(ShellId::from_opt(&Some("pwsh".into())), ShellId::Auto);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn json_mirror_hides_paths() {
        let caps = ShellCapabilities {
            pwsh_available: true,
            git_bash_path: Some(r"C:\Git\bin\bash.exe".into()),
            wsl_available: false,
            pwsh_path: Some(r"C:\pwsh\pwsh.exe".into()),
        };
        let json = ShellCapabilitiesJson::from(&caps);
        assert!(json.pwsh_available);
        assert!(json.git_bash_available);
        assert!(!json.wsl_available);
        // Serialize to confirm no path leaks into the payload.
        let s = serde_json::to_string(&json).unwrap();
        assert!(!s.contains("bash.exe"));
        assert!(!s.contains("pwsh.exe"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_cmd_uses_system_path() {
        let caps = ShellCapabilities::default();
        let (prog, args) = resolve_shell(ShellId::Cmd, &caps);
        assert!(prog.ends_with("cmd.exe"));
        assert!(args.is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_powershell_is_absolute() {
        let caps = ShellCapabilities::default();
        let (prog, _args) = resolve_shell(ShellId::Powershell, &caps);
        // Absolute path (drive letter or resolved system path), never a
        // bare "powershell.exe" that re-introduces the PATH-lookup risk.
        assert!(prog.contains('\\'), "powershell path should be absolute: {prog}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_gitbash_falls_back_when_absent() {
        // No git_bash_path → resolve still returns something spawnable
        // (bare bash.exe) rather than panicking; the picker simply won't
        // offer the option when the capability is None.
        let caps = ShellCapabilities::default();
        let (prog, _args) = resolve_shell(ShellId::GitBash, &caps);
        assert_eq!(prog, "bash.exe");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn resolve_unix_explicit_shells() {
        let caps = ShellCapabilities::default();
        let (p, _) = resolve_shell(ShellId::Zsh, &caps);
        assert_eq!(p, "zsh");
        let (p, _) = resolve_shell(ShellId::Bash, &caps);
        assert_eq!(p, "bash");
        let (p, _) = resolve_shell(ShellId::Fish, &caps);
        assert_eq!(p, "fish");
        let (p, a) = resolve_shell(ShellId::Sh, &caps);
        assert_eq!(p, "sh");
        assert!(a.is_empty());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn json_unix_carries_four_bools_no_paths() {
        let caps = ShellCapabilities {
            wsl_available: false,
            zsh_available: true,
            bash_available: true,
            fish_available: false,
            sh_available: true,
        };
        let json = ShellCapabilitiesJson::from(&caps);
        assert!(json.zsh_available);
        assert!(json.bash_available);
        assert!(!json.fish_available);
        assert!(json.sh_available);
        let s = serde_json::to_string(&json).unwrap();
        assert!(s.contains("zsh_available"));
        assert!(s.contains("sh_available"));
        // No filesystem paths leak into the frontend payload.
        assert!(!s.contains("/bin/"));
        assert!(!s.contains("/usr/"));
    }
}
