//! Coffee CLI Skills — local skill store, surfaced into agent prompts.
//!
//! Two tiers under `~/.coffee-cli/`:
//! ```text
//!   skills/<name>/         ← enabled (offered in the Gambit skill picker)
//!   skills-library/<name>/ ← downloaded but disabled
//! ```
//!
//! We do NOT install skills into each CLI's native skills dir. Instead the
//! Gambit input attaches a skill as a pill; on send it expands to a
//! one-line instruction pointing the agent at the skill's on-disk
//! `SKILL.md` (e.g. `…/.coffee-cli/skills/hyperframes/SKILL.md`). Every
//! agent can read files, so this works uniformly with ZERO per-tool wiring
//! — no junctions, no per-CLI skill-dir conventions, no restart-to-apply.
//! `skills_list` returns each skill's absolute `path` for that expansion.
//!
//! Download model (deferred for future custom-skill upload flow): the
//! frontend would do HTTP fetches via `fetch()` and pipe bytes into
//! `skills_write_file`. No HTTP client in Rust deps.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

/// Coffee CLI's bundled skills, baked into the binary at compile time.
/// Currently 2 entries (`screenshot`, `vibeid`); both live under
/// `skills/` at the repo root and ship as part of Coffee CLI's
/// product (not as upstream-vendored content). Mostly markdown + small
/// scripts → rodata-cheap. Seeded into the user's
/// `~/.coffee-cli/skills-library/` on first launch (idempotent — files
/// already present are left alone, so user toggles aren't disturbed
/// by app upgrades).
///
/// The `screenshot` skill originated from `openai/skills` (MIT-licensed,
/// LICENSE.txt preserved in `skills/screenshot/`). Once vendored here
/// it's owned by Coffee CLI; upstream changes are not auto-tracked.
/// The full openai/skills repo and any other reference material live
/// under `reference/` (gitignored) — see `reference/README.md`.
static BUNDLED_SKILLS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/skills");

/// Phased rollout allowlist. The combined bundle (openai/skills .curated
/// + Coffee CLI's own skills) always ships every skill, but only the
/// names listed here are surfaced via seeding + UI. Enables "test 5,
/// ship 5, test next batch, ship next batch" without re-cutting a
/// release just to add more skill catalog entries.
///
/// Phased rollout — only names listed here are seeded + surfaced.
///
///   - `screenshot` — openai/skills (MIT, LICENSE.txt preserved).
///   - `vibeid` — Coffee CLI's own; ships scripts/ + matrix.json,
///     references CDN-hosted persona images, parses Claude Code's
///     session jsonl directly.
///   - `hyperframes` — heygen-com/hyperframes (Apache-2.0, LICENSE.txt
///     preserved). HTML → short-video framework. Vendored from
///     `skills/hyperframes/` of the upstream multi-skill repo; the
///     other 13 sibling skills (hyperframes-cli, gsap, etc.) are not
///     bundled — the main `hyperframes` skill is self-sufficient for
///     authoring compositions.
///   - `playwright` — Coffee CLI's own. Browser automation skill that
///     drives `@playwright/cli` (microsoft/playwright, MIT) via an
///     `npx`-based wrapper at `scripts/run.sh`, no global install
///     required. SKILL.md is creator-task oriented (scrape reviews,
///     batch screenshot, page → PDF, login-gated extraction) using the
///     snapshot-then-ref interaction model. Icon borrowed from upstream
///     playwright.dev for tool identification.
const VISIBLE_SKILLS: &[&str] = &["screenshot", "vibeid", "hyperframes", "playwright"];

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub name: String,
    pub enabled: bool,
    /// Raw SKILL.md content (UTF-8). Frontend parses YAML frontmatter to
    /// pull display name / description / category. Returns None when the
    /// skill folder exists but its SKILL.md is missing or unreadable.
    pub skill_md: Option<String>,
    /// `data:image/...;base64,...` URL for the skill's icon, if any.
    /// Probes `assets/<name>-small.svg` → `assets/<name>.svg` →
    /// `assets/<name>.png` and embeds the first match. None if no
    /// icon exists. Embedding (vs serving via asset:// protocol) keeps
    /// the IPC self-contained — frontend can `<img src={iconDataUrl}>`
    /// without a second round-trip.
    pub icon_data_url: Option<String>,
    /// Absolute on-disk path to the skill's directory. The Gambit skill
    /// pill expands to an instruction pointing the agent at
    /// `<path>/SKILL.md`. Native separators (backslash on Windows).
    pub path: String,
}

fn home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "No home directory".to_string())
}

pub fn skills_root() -> Result<PathBuf, String> {
    Ok(home()?.join(".coffee-cli").join("skills"))
}

pub fn library_root() -> Result<PathBuf, String> {
    Ok(home()?.join(".coffee-cli").join("skills-library"))
}

/// Make sure the canonical Coffee CLI skill dirs exist AND the bundled
/// skill catalog has been seeded. Safe to call on every app launch /
/// every Skills-page open — both phases are idempotent.
#[tauri::command]
pub fn skills_ensure_dirs() -> Result<(), String> {
    fs::create_dir_all(skills_root()?).map_err(|e| format!("create skills/: {}", e))?;
    fs::create_dir_all(library_root()?).map_err(|e| format!("create skills-library/: {}", e))?;
    seed_library_from_bundle()?;
    Ok(())
}

/// Files that always overwrite on every app launch — the canonical
/// "skill metadata" set. Other files (scripts/, assets/) only seed
/// once on first install so user-side patches survive Coffee CLI
/// upgrades. Updating these on every launch means a Coffee CLI
/// version bump that ships new SKILL.md frontmatter (rename, reword
/// description, add a locale) takes effect WITHOUT users having to
/// manually delete their library/skills dir.
const ALWAYS_REFRESH_FILENAMES: &[&str] = &["SKILL.md", "matrix.json"];

/// Walk both bundles and seed/refresh skill files into
/// `~/.coffee-cli/skills-library/` (and refresh metadata in
/// `~/.coffee-cli/skills/` for already-enabled skills).
///
/// Three-tier write rule:
///   1. New skills (not present in either dir) → full copy to library
///   2. Existing skills, ALWAYS_REFRESH_FILENAMES (SKILL.md / matrix.json)
///      → overwrite in whichever dir the skill lives (library or skills)
///   3. Existing skills, other files → leave alone (preserves user
///      patches to scripts/, assets/, etc.)
fn seed_library_from_bundle() -> Result<(), String> {
    let lib = library_root()?;
    let enabled = skills_root()?;
    seed_bundle(&BUNDLED_SKILLS, &lib, &enabled)?;
    Ok(())
}

fn seed_bundle(
    bundle: &include_dir::Dir<'_>,
    lib_root: &Path,
    enabled_root: &Path,
) -> Result<(), String> {
    for entry in bundle.entries() {
        let include_dir::DirEntry::Dir(skill_dir) = entry else { continue };

        // Bundle's top-level entries are individual skill dirs (skill_dir
        // path is single-segment, e.g. "vibeid"). Apply allowlist + figure
        // out where the skill currently lives on disk.
        let name = match skill_dir.path().file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.is_empty() && VISIBLE_SKILLS.contains(&n) => n,
            _ => continue,
        };

        let in_enabled = enabled_root.join(name);
        let in_lib = lib_root.join(name);
        let enabled_exists = in_enabled.exists();
        let lib_exists = in_lib.exists();

        if !enabled_exists && !lib_exists {
            // First-time seed → full copy into library/<name>/.
            full_copy_into(skill_dir, lib_root)?;
            continue;
        }

        // Skill exists somewhere — refresh metadata + add any files
        // the bundle introduced after first-seed (e.g. an icon added in
        // a later Coffee CLI release). Refresh both library and enabled
        // copies if both exist so they stay consistent.
        //
        // copy_missing_into never overwrites existing files — user
        // patches to scripts/, assets/, etc. survive upgrade.
        // Orphan files on disk that the bundle no longer ships are
        // intentionally left alone for the same reason.
        if enabled_exists {
            refresh_metadata(skill_dir, &in_enabled)?;
            copy_missing_into(skill_dir, enabled_root)?;
        }
        if lib_exists {
            refresh_metadata(skill_dir, &in_lib)?;
            copy_missing_into(skill_dir, lib_root)?;
        }
    }
    Ok(())
}

/// Recursively copy every file in `bundle` into `dest_root`, creating
/// parent dirs as needed. Used for the first-time seed of a new skill.
fn full_copy_into(bundle: &include_dir::Dir<'_>, dest_root: &Path) -> Result<(), String> {
    write_bundle(bundle, dest_root, /* skip_existing */ false)
}

/// Recursively copy files from `bundle` into `dest_root`, but skip any
/// file that already exists on disk. Used to add new files to an
/// already-seeded skill on Coffee CLI upgrade — picks up new icons,
/// new reference docs, new scripts without clobbering user edits.
fn copy_missing_into(bundle: &include_dir::Dir<'_>, dest_root: &Path) -> Result<(), String> {
    write_bundle(bundle, dest_root, /* skip_existing */ true)
}

fn write_bundle(
    bundle: &include_dir::Dir<'_>,
    dest_root: &Path,
    skip_existing: bool,
) -> Result<(), String> {
    for entry in bundle.entries() {
        match entry {
            include_dir::DirEntry::Dir(sub) => {
                write_bundle(sub, dest_root, skip_existing)?;
            }
            include_dir::DirEntry::File(file) => {
                let dest = dest_root.join(file.path());
                if skip_existing && dest.exists() {
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
                }
                fs::write(&dest, file.contents())
                    .map_err(|e| format!("seed write {}: {}", dest.display(), e))?;
            }
        }
    }
    Ok(())
}

/// Overwrite ALWAYS_REFRESH_FILENAMES in `target_skill_dir` from the
/// matching bundle entries. Skips files that aren't in the bundle (skill
/// might not have a matrix.json, etc.). Other bundle files are NOT
/// touched — user patches to scripts/, assets/, etc. survive upgrade.
///
/// Walks immediate children of `bundle_skill_dir` rather than calling
/// `get_file()` (whose path argument must be relative to the
/// include_dir root, not to the nested Dir handed to us — easy to get
/// wrong, easier to just enumerate).
fn refresh_metadata(
    bundle_skill_dir: &include_dir::Dir<'_>,
    target_skill_dir: &Path,
) -> Result<(), String> {
    for entry in bundle_skill_dir.entries() {
        let include_dir::DirEntry::File(file) = entry else { continue };
        let fname = match file.path().file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !ALWAYS_REFRESH_FILENAMES.contains(&fname) {
            continue;
        }
        let dest = target_skill_dir.join(fname);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        fs::write(&dest, file.contents())
            .map_err(|e| format!("refresh write {}: {}", dest.display(), e))?;
    }
    Ok(())
}

/// Reject anything that's not a single-segment ASCII identifier. Skill
/// names land in filesystem paths and slash-command syntax — keep them
/// boring.
fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!("Invalid skill name length: {}", name));
    }
    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(format!("Invalid char in skill name: {:?}", name));
        }
    }
    Ok(())
}

/// rel_path must be a forward-slash relative path inside one skill dir —
/// no `..`, no leading slash, no Windows drive letter, no NUL.
fn validate_rel_path(rel_path: &str) -> Result<(), String> {
    if rel_path.is_empty() || rel_path.len() > 255 {
        return Err(format!("Invalid rel_path length: {}", rel_path));
    }
    if rel_path.contains("..")
        || rel_path.starts_with('/')
        || rel_path.starts_with('\\')
        || rel_path.contains(':')
        || rel_path.contains('\0')
    {
        return Err(format!("Invalid rel_path: {}", rel_path));
    }
    Ok(())
}

/// Write one file into `~/.coffee-cli/skills-library/<name>/<rel_path>`.
///
/// Used by the frontend during the download flow: fetch SKILL.md / scripts /
/// assets via `fetch()` then call this for each. Always lands in the
/// disabled "library" tier — user must explicitly toggle to expose to CLIs.
#[tauri::command]
pub fn skills_write_file(name: String, rel_path: String, bytes: Vec<u8>) -> Result<(), String> {
    validate_skill_name(&name)?;
    validate_rel_path(&rel_path)?;
    let target = library_root()?.join(&name).join(&rel_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    fs::write(&target, &bytes).map_err(|e| format!("write {}: {}", target.display(), e))
}

/// List every skill Coffee CLI knows about, with enable status and raw
/// SKILL.md content. Frontend parses frontmatter for display.
///
/// If the same name exists in both `skills/` and `skills-library/` (a
/// pathological state that shouldn't happen in normal flow), the enabled
/// copy wins.
#[tauri::command]
pub fn skills_list() -> Result<Vec<SkillEntry>, String> {
    let mut out: Vec<SkillEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (root, enabled) in [(skills_root()?, true), (library_root()?, false)] {
        if !root.is_dir() {
            continue;
        }
        let entries =
            fs::read_dir(&root).map_err(|e| format!("read {}: {}", root.display(), e))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if seen.contains(&name) {
                continue;
            }
            seen.insert(name.clone());

            let skill_md = fs::read_to_string(path.join("SKILL.md")).ok();
            let icon_data_url = read_skill_icon(&path, &name);
            out.push(SkillEntry {
                path: path.to_string_lossy().to_string(),
                name,
                enabled,
                skill_md,
                icon_data_url,
            });
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Probe the skill's `assets/` folder for a usable icon, in priority order:
///   `<name>.png`         ← preferred: matches Codex's pick. The unsuffixed
///                            PNG is the skill's canonical hero icon —
///                            colourful, detailed, designed to be the
///                            "main" art. The `-small.svg` variant is
///                            literally named for thumbnail use, so it's
///                            visually thinner than what skill cards want.
///   `<name>.svg`         ← vector full-size, fallback when no PNG
///   `<name>-small.svg`   ← thumbnail SVG, last resort within the
///                            `<name>.<ext>` namespace
///   `icon.svg`/`icon.png` ← generic fallback for skills that don't
///                            follow openai/skills' naming convention
///
/// Returns a `data:image/...;base64,...` URL ready for `<img src=>`.
fn read_skill_icon(skill_dir: &Path, name: &str) -> Option<String> {
    let assets = skill_dir.join("assets");
    let candidates = [
        (format!("{}.png", name), "image/png"),
        (format!("{}.svg", name), "image/svg+xml"),
        (format!("{}-small.svg", name), "image/svg+xml"),
        ("icon.png".to_string(), "image/png"),
        ("icon.svg".to_string(), "image/svg+xml"),
    ];
    for (filename, mime) in candidates {
        let path = assets.join(&filename);
        if let Ok(bytes) = fs::read(&path) {
            // Cap embedded icons at 256 KiB. Above that, the IPC payload
            // bloat outweighs the convenience — frontend should fall back
            // to a generic glyph rather than carry a 2 MB image inline.
            if bytes.len() > 256 * 1024 {
                continue;
            }
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            return Some(format!("data:{};base64,{}", mime, b64));
        }
    }
    None
}

/// Toggle a skill between disabled (library/) and enabled (skills/).
///
/// `enable=true`: move `skills-library/<name>` → `skills/<name>`.
/// `enable=false`: reverse. Purely a tier move — no per-CLI mirroring;
/// the Gambit pill references the skill by absolute path at send time, so
/// nothing has to be installed into any tool's dir and no restart is
/// needed. Returns an (always-empty today) warnings vec for API stability.
#[tauri::command]
pub fn skills_toggle(name: String, enable: bool) -> Result<Vec<String>, String> {
    validate_skill_name(&name)?;
    let (from_root, to_root) = if enable {
        (library_root()?, skills_root()?)
    } else {
        (skills_root()?, library_root()?)
    };
    let src = from_root.join(&name);
    let dst = to_root.join(&name);

    if !src.exists() {
        return Err(format!("Skill not in {}: {}", from_root.display(), name));
    }
    if dst.exists() {
        return Err(format!("Destination already exists: {}", dst.display()));
    }

    // Make sure the destination's parent dir exists (defensive — they're
    // created by skills_ensure_dirs at boot, but a rogue rm could have
    // wiped them).
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }

    fs::rename(&src, &dst).map_err(|e| format!("rename: {}", e))?;

    // Enabling/disabling is just a tier move now — no per-CLI junctions.
    // The Gambit skill pill points the agent straight at the on-disk
    // SKILL.md, so nothing has to be mirrored into each tool's dir.
    Ok(Vec::new())
}

/// Permanently remove a skill from both tiers.
#[tauri::command]
pub fn skills_delete(name: String) -> Result<(), String> {
    validate_skill_name(&name)?;
    for root in [skills_root()?, library_root()?] {
        let p = root.join(&name);
        if p.exists() {
            fs::remove_dir_all(&p).map_err(|e| format!("rm {}: {}", p.display(), e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_name_validation() {
        assert!(validate_skill_name("documents").is_ok());
        assert!(validate_skill_name("cli-creator").is_ok());
        assert!(validate_skill_name("foo_bar_42").is_ok());
        assert!(validate_skill_name("").is_err());
        assert!(validate_skill_name("../etc").is_err());
        assert!(validate_skill_name("foo bar").is_err());
        assert!(validate_skill_name("foo/bar").is_err());
        assert!(validate_skill_name("foo:bar").is_err());
    }

    #[test]
    fn rel_path_validation() {
        assert!(validate_rel_path("SKILL.md").is_ok());
        assert!(validate_rel_path("scripts/run.sh").is_ok());
        assert!(validate_rel_path("assets/sub/icon.png").is_ok());
        assert!(validate_rel_path("").is_err());
        assert!(validate_rel_path("../escape").is_err());
        assert!(validate_rel_path("/abs/path").is_err());
        assert!(validate_rel_path("\\windows\\abs").is_err());
        assert!(validate_rel_path("C:/drive").is_err());
        assert!(validate_rel_path("null\0byte").is_err());
    }
}
