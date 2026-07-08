//! Windows PATH hydration from the registry.
//!
//! A GUI-launched process inherits a PATH that can be MISSING the per-user
//! dirs where CLI agents install — npm global (`%APPDATA%\npm`), pnpm, bun,
//! cargo, AND scoop shims, volta, nvm-windows, mise, the Anthropic native
//! installer in `~/.local/bin`, etc. The previous fix hardcoded 5 dirs; that
//! was a guessing treadmill — every new install method needed another entry
//! and we still missed scoop/volta/nvm. The registry is the source of truth a
//! fresh `cmd.exe` sees, so we read HKCU + HKLM `Path` (`REG_EXPAND_SZ`
//! values are auto-expanded by `RegGetValueW`) and merge those dirs into the
//! process PATH. Append-only + existence-gated: it can only make more tools
//! resolvable, never removes or shadows anything — a harmless no-op when the
//! inherited PATH was already complete.

#![cfg(target_os = "windows")]

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::WIN32_ERROR;
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ,
};

/// Read the real PATH (HKLM + HKCU `Path`) and merge any missing entries into
/// the process PATH. Append-only + existence-gated; no-op on error or when
/// PATH is already complete. Called once at GUI startup.
pub(crate) fn hydrate() {
    let candidates = real_path_dirs();
    if candidates.is_empty() {
        return;
    }
    let current = std::env::var("PATH").unwrap_or_default();
    let merged = merge_into_path(&current, &candidates);
    if merged != current {
        // `set_var` becomes `unsafe` in edition 2024 (cross-thread env races);
        // we run single-threaded at GUI startup before any spawn threads exist,
        // mirroring the `unsafe{}` wrapping in main.rs's GDK/PATH blocks.
        unsafe { std::env::set_var("PATH", merged); }
    }
}

/// Existing dirs from the real (registry) PATH — system entries first, then
/// user entries (matches a fresh `cmd.exe`'s merge order). Empty on error.
fn real_path_dirs() -> Vec<String> {
    let mut dirs: Vec<String> = Vec::new();
    dirs.extend(split_existing(&read_path_value(
        HKEY_LOCAL_MACHINE,
        w!("SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"),
        w!("Path"),
    )));
    dirs.extend(split_existing(&read_path_value(
        HKEY_CURRENT_USER,
        w!("Environment"),
        w!("Path"),
    )));
    dirs
}

/// Split a PATH string into existing dir strings (non-existent entries are
/// dropped, so stale registry entries never pollute the process PATH).
fn split_existing(path: &Option<String>) -> Vec<String> {
    match path {
        Some(s) if !s.is_empty() => s
            .split(';')
            .map(|e| e.trim())
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let p = PathBuf::from(e);
                if p.is_dir() {
                    Some(p.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Append-only merge: add `candidates` not already present in `current`.
/// Comparison is case-insensitive and trailing-`\`-trimmed (Windows PATH
/// semantics). Duplicates among `candidates` are deduped. Pure / testable.
fn merge_into_path(current: &str, candidates: &[String]) -> String {
    let mut present: HashSet<String> = current.split(';').map(normalize).collect();
    let mut additions: Vec<String> = Vec::new();
    for s in candidates {
        let n = normalize(s);
        if n.is_empty() || present.contains(&n) {
            continue;
        }
        present.insert(n);
        additions.push(s.clone());
    }
    if additions.is_empty() {
        return current.to_string();
    }
    let joined = additions.join(";");
    if current.is_empty() {
        joined
    } else {
        format!("{current};{joined}")
    }
}

/// Case-insensitive, trimmed, trailing-`\`-stripped key for PATH dedup.
fn normalize(entry: &str) -> String {
    entry.trim().trim_end_matches('\\').to_ascii_lowercase()
}

/// Read a registry string value (`REG_SZ` or `REG_EXPAND_SZ`, auto-expanded).
fn read_path_value(root: HKEY, subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    unsafe {
        // Accept REG_SZ or REG_EXPAND_SZ; the latter is auto-expanded by
        // RegGetValueW (RRF_NOEXPAND not set), so %USERPROFILE% etc. resolve.
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;

        // Phase 1: query byte size.
        let mut len: u32 = 0;
        let r: WIN32_ERROR =
            RegGetValueW(root, subkey, value, flags, None, None, Some(&mut len));
        if r.0 != 0 || len < 2 {
            return None;
        }

        // Phase 2: query data (len is in bytes; UTF-16LE → u16 count).
        let count = (len as usize) / 2;
        let mut buf = vec![0u16; count];
        let r: WIN32_ERROR = RegGetValueW(
            root,
            subkey,
            value,
            flags,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut len),
        );
        if r.0 != 0 {
            return None;
        }

        // Drop the trailing NUL terminator(s).
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(OsString::from_wide(&buf[..end]).to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_current_joins_all_candidates() {
        let merged = merge_into_path("", &["C:\\foo".into(), "D:\\bar".into()]);
        assert_eq!(merged, "C:\\foo;D:\\bar");
    }

    #[test]
    fn already_present_skipped_case_insensitive_and_trailing_slash() {
        // "C:\\Foo\\" normalizes to "c:\\foo" — candidate "c:\\foo" matches.
        let merged = merge_into_path("C:\\Foo\\", &["c:\\foo".into()]);
        assert_eq!(merged, "C:\\Foo\\");
    }

    #[test]
    fn duplicate_candidates_deduped() {
        let merged = merge_into_path("", &["D:\\bar".into(), "D:\\bar".into()]);
        assert_eq!(merged, "D:\\bar");
    }

    #[test]
    fn empty_candidates_returns_current_unchanged() {
        let merged = merge_into_path("C:\\Windows", &[]);
        assert_eq!(merged, "C:\\Windows");
    }

    #[test]
    fn mixed_present_and_new() {
        let merged =
            merge_into_path("C:\\Windows;C:\\foo", &["c:\\FOO".into(), "D:\\bar".into()]);
        assert_eq!(merged, "C:\\Windows;C:\\foo;D:\\bar");
    }

    #[test]
    fn blank_entries_in_current_dont_shadow_candidates() {
        // A stray ";" must not create a phantom "" entry that blocks a real dir.
        let merged = merge_into_path("C:\\Windows;", &["D:\\bar".into()]);
        assert_eq!(merged, "C:\\Windows;;D:\\bar");
    }

    #[test]
    fn real_path_dirs_reads_registry() {
        // End-to-end: the system PATH always exists on Windows and always
        // contains C:\Windows. If this fails, the registry subkey/value
        // names in `read_path_value` are wrong (silent None → no hydration).
        let dirs = real_path_dirs();
        assert!(!dirs.is_empty(), "registry PATH read returned nothing");
        assert!(
            dirs.iter().any(|d| d.to_ascii_lowercase().contains("\\windows")),
            "system PATH missing C:\\Windows — subkey path likely wrong"
        );
        // REG_EXPAND_SZ entries (e.g. %USERPROFILE%\...) must be expanded —
        // a literal '%' leaking through means RRF_NOEXPAND got set by mistake
        // or the value wasn't read as EXPAND_SZ.
        assert!(
            dirs.iter().all(|d| !d.contains('%')),
            "unexpanded env var in PATH — EXPAND_SZ auto-expansion failed"
        );
    }
}
