// git.rs — git-backed "changes" panel (replaces the session-snapshot diff).
//
// P1 surface (read-only): list working-tree changes grouped into
// staged / unstaged / untracked, and produce a per-file unified diff.
// Stage / unstage / commit / init land in P2; history / branches in P3.
//
// Every git call goes through `git_output`, modeled on marketplace.rs's
// `git()` helper (CREATE_NO_WINDOW on Windows so no console window flashes).
// All repo queries run with the working dir pinned to the repository ROOT —
// resolved once via `rev-parse --show-toplevel` — so reported paths and diff
// pathspecs are consistently repo-root-relative even when the tab's folder is
// a subdirectory (matches how an IDE's Source Control shows the whole repo).

use std::collections::HashMap;
use std::process::Command;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Run `git -C <dir> <args>`, capturing stdout. Err on a missing binary or a
/// non-zero exit (carrying stderr). Display commands like `diff` exit 0 even
/// when there are differences, so this is the right strictness for them.
fn git_output(dir: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().map_err(|e| format!("git not available: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() { format!("git {:?} failed", args) } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True if a `git` binary is on PATH. Not cached — the changes panel only
/// calls it on folder / agent-status ticks, never on a hot path.
fn git_on_path() -> bool {
    let mut cmd = Command::new("git");
    cmd.arg("--version");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.output().map(|o| o.status.success()).unwrap_or(false)
}

#[derive(serde::Serialize)]
pub struct GitFileEntry {
    /// Absolute, forward-slashed path (repo_root + "/" + rel).
    pub path: String,
    /// Repo-relative path exactly as git reports it; diff queries use this.
    pub rel: String,
    /// Single-letter status: M(odified) A(dded) D(eleted) R(enamed)
    /// C(opied) U(nmerged) ?(untracked).
    pub status: String,
    pub added: u32,
    pub deleted: u32,
}

/// Discriminated by `state` so the frontend can branch on no-git / not-a-repo
/// without sentinel values. `Ok` carries the three change groups.
#[derive(serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GitChanges {
    /// No `git` binary on PATH → panel shows the "install git" prompt.
    NoGit,
    /// git present, but `folder` is not inside a work tree → "not a repo"
    /// prompt (+ an "init here" affordance in P2).
    NotRepo,
    Ok {
        /// Absolute, forward-slashed repository root. The frontend passes this
        /// back to `git_show_file` so diff pathspecs resolve from the top.
        repo_root: String,
        /// Current branch name, e.g. "main" / "feature/x". A detached HEAD
        /// reports as "(<short-sha>)" so the header still shows something.
        branch: String,
        staged: Vec<GitFileEntry>,
        unstaged: Vec<GitFileEntry>,
        untracked: Vec<GitFileEntry>,
    },
}

/// added/deleted counts keyed by repo-relative path, from a `numstat` stream.
///
/// MUST use `-z`: without it git applies `core.quotepath` and octal-escapes
/// non-ASCII paths (e.g. a CJK filename becomes `"\344\270\255…"`), which then
/// never matches the RAW path we parse from `status --porcelain -z` — so every
/// non-ASCII-named file would show a "+0 -0" badge. `-z` emits raw, NUL-
/// separated records so the keys line up. Binary files report "-\t-" → 0/0;
/// rename rows (path field empty under -z) simply don't match and degrade to a
/// 0/0 badge. Counts are best-effort by design.
fn numstat_map(repo_root: &str, cached: bool) -> HashMap<String, (u32, u32)> {
    let args: &[&str] = if cached {
        &["diff", "--numstat", "-z", "--cached"]
    } else {
        &["diff", "--numstat", "-z"]
    };
    let mut map = HashMap::new();
    let Ok(out) = git_output(repo_root, args) else { return map; };
    for field in out.split('\0') {
        if field.is_empty() {
            continue;
        }
        let mut it = field.splitn(3, '\t');
        let (Some(a), Some(d), Some(p)) = (it.next(), it.next(), it.next()) else { continue; };
        if p.is_empty() {
            continue; // rename record under -z carries the path in later fields
        }
        map.insert(
            p.to_string(),
            (a.parse::<u32>().unwrap_or(0), d.parse::<u32>().unwrap_or(0)),
        );
    }
    map
}

/// Absolute, forward-slashed key with an upper-cased Windows drive letter —
/// the same normalization `compute_folder_stats` used, so the Explorer file
/// tree keeps matching these paths against its `list_directory` entries.
/// Current branch name. `symbolic-ref --short HEAD` returns the branch even on
/// an UNBORN branch (a fresh `git init` with no commits yet → "main"), which
/// `rev-parse --abbrev-ref` does not (it yields the literal "HEAD" there).
/// Falls through on a detached HEAD to the short SHA as "(abc1234)" so the
/// header always has a label.
fn git_branch(repo_root: &str) -> String {
    if let Ok(s) = git_output(repo_root, &["symbolic-ref", "--short", "HEAD"]) {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    let sha = git_output(repo_root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let sha = sha.trim();
    if sha.is_empty() { "HEAD".to_string() } else { format!("({sha})") }
}

/// Line count of a file, used as the "+N" for untracked files (which have no
/// git blob to numstat). Text-only and size-capped; returns 0 on any failure
/// or a binary/oversized file so it degrades to a blank badge, never a hang.
const UNTRACKED_COUNT_CAP: usize = 200;
const UNTRACKED_MAX_BYTES: u64 = 1_000_000;
fn count_lines(path: &str) -> u32 {
    let Ok(meta) = std::fs::metadata(path) else { return 0; };
    if !meta.is_file() || meta.len() > UNTRACKED_MAX_BYTES {
        return 0;
    }
    let Ok(bytes) = std::fs::read(path) else { return 0; };
    if bytes.is_empty() || bytes[..bytes.len().min(8192)].contains(&0u8) {
        return 0; // empty or binary
    }
    let mut lines = bytes.iter().filter(|&&b| b == b'\n').count();
    if *bytes.last().unwrap() != b'\n' {
        lines += 1; // final line without a trailing newline still counts
    }
    lines.min(u32::MAX as usize) as u32
}

fn join_abs(repo_root: &str, rel: &str) -> String {
    crate::server::normalize_path_key(&format!("{}/{}", repo_root.trim_end_matches('/'), rel))
}

/// List the active folder's git working-tree changes. One IPC call; the
/// frontend polls it on the same agent-status / fs-refresh triggers the old
/// `compute_folder_stats` used.
#[tauri::command]
pub fn git_changes(folder: String) -> GitChanges {
    if !git_on_path() {
        return GitChanges::NoGit;
    }
    // Repo detection + canonical root in one shot (run from the tab folder,
    // which may be a subdir). Failure here = not a work tree.
    let repo_root = match git_output(&folder, &["rev-parse", "--show-toplevel"]) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return GitChanges::NotRepo,
    };

    let branch = git_branch(&repo_root);
    let staged_counts = numstat_map(&repo_root, true);
    let unstaged_counts = numstat_map(&repo_root, false);

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    // Porcelain v1, NUL-separated, every untracked file listed. Each record is
    // "XY <path>" where X = index/staged status, Y = worktree/unstaged status.
    // With -z the path is raw (no C-quoting). A rename/copy (X or Y in {R,C})
    // appends ONE extra NUL field — the origin path — which we consume so it
    // isn't mis-parsed as its own entry.
    let porcelain = git_output(
        &repo_root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .unwrap_or_default();
    let fields: Vec<&str> = porcelain.split('\0').collect();
    let mut i = 0;
    while i < fields.len() {
        let rec = fields[i];
        i += 1;
        if rec.len() < 4 {
            continue; // shorter than "XY p" → trailing empty field, skip
        }
        let bytes = rec.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let rel = rec[3..].to_string();
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            i += 1; // skip the origin-path follow-up field
        }

        if x == '?' && y == '?' {
            untracked.push(GitFileEntry {
                path: join_abs(&repo_root, &rel),
                rel,
                status: "?".into(),
                added: 0,
                deleted: 0,
            });
            continue;
        }
        if x != ' ' && x != '?' {
            let (a, d) = staged_counts.get(&rel).copied().unwrap_or((0, 0));
            staged.push(GitFileEntry {
                path: join_abs(&repo_root, &rel),
                rel: rel.clone(),
                status: x.to_string(),
                added: a,
                deleted: d,
            });
        }
        if y != ' ' && y != '?' {
            let (a, d) = unstaged_counts.get(&rel).copied().unwrap_or((0, 0));
            unstaged.push(GitFileEntry {
                path: join_abs(&repo_root, &rel),
                rel,
                status: y.to_string(),
                added: a,
                deleted: d,
            });
        }
    }

    // Untracked files have no git blob to numstat; show their line count as
    // additions so the badge/header aren't a meaningless "+0 -0". Bounded:
    // skip when there are many untracked (a fresh repo can list thousands) and
    // cap per-file size, keeping the polled call cheap.
    if untracked.len() <= UNTRACKED_COUNT_CAP {
        for e in untracked.iter_mut() {
            e.added = count_lines(&e.path);
        }
    }

    GitChanges::Ok { repo_root, branch, staged, unstaged, untracked }
}

/// Content of a file at a git revision, e.g. `git show HEAD:src/a.ts` or
/// `git show :src/a.ts` (the staged/index blob). `spec` is "<ref>:<rel>" with
/// `rel` repo-root-relative. Returns None when the path doesn't exist at that
/// revision (a newly-added file has no HEAD blob) — the frontend treats None
/// as an empty side, so the file renders as all-additions.
///
/// This is the ONLY data the DiffPanel needs: it feeds the returned old/new
/// blobs straight into the existing jsdiff + Shiki pipeline, so the diff
/// rendering (folding, syntax colors, size guards) is reused unchanged. The
/// staged/unstaged old↔new mapping lives in the frontend:
///   • unstaged tracked: old = `:rel` (index)   new = working file on disk
///   • staged   tracked: old = `HEAD:rel`        new = `:rel` (index)
///   • untracked:        old = ""                new = working file on disk
#[tauri::command]
pub fn git_show_file(repo_root: String, spec: String) -> Option<String> {
    git_output(&repo_root, &["show", &spec]).ok()
}

/// `git init` the given folder so the not-a-repo state's "initialize here"
/// button can turn an ordinary folder into a tracked workspace in one click.
#[tauri::command]
pub fn git_init(folder: String) -> Result<(), String> {
    git_output(&folder, &["init"]).map(|_| ())
}
