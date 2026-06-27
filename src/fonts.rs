// fonts.rs — enumerate installed font families for the terminal font picker.
//
// Pure-Rust (ttf-parser): scan the OS font directories, parse each face for
// its family name + monospace flag, dedupe by family. No system libraries
// (unlike font-kit's fontconfig/freetype on Linux), so it builds cleanly on
// every CI target. Runs on demand when the user opens Settings, not a hot
// path — we read each font file once and don't cache (a few hundred ms).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize)]
pub struct FontInfo {
    pub family: String,
    /// True if any face of this family reports fixed-pitch — surfaced so the
    /// picker can list/flag monospace faces (terminals want them).
    pub monospace: bool,
}

/// Platform font directories to scan.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Ok(windir) = std::env::var("WINDIR") {
            dirs.push(PathBuf::from(windir).join("Fonts"));
        }
        if let Some(local) = dirs::data_local_dir() {
            dirs.push(local.join("Microsoft").join("Windows").join("Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
        if let Some(h) = dirs::home_dir() {
            dirs.push(h.join("Library").join("Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        if let Some(h) = dirs::home_dir() {
            dirs.push(h.join(".fonts"));
            dirs.push(h.join(".local").join("share").join("fonts"));
        }
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                dirs.push(PathBuf::from(xdg).join("fonts"));
            }
        }
    }
    dirs
}

/// Recursively collect font files under `dir` (depth-capped to avoid symlink
/// loops / pathological trees).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        // Skip symlinked dirs — avoids loops and walking into huge unrelated
        // trees if a font dir is symlinked somewhere big.
        let is_symlink = entry.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        if p.is_dir() {
            if !is_symlink {
                collect_files(&p, out, depth + 1);
            }
        } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
                out.push(p);
            }
        }
    }
}

/// Best human family name for a face: prefer the typographic/preferred family
/// (name id 16), fall back to the legacy family (name id 1).
fn family_name(face: &ttf_parser::Face) -> Option<String> {
    let mut family: Option<String> = None;
    let mut typographic: Option<String> = None;
    for name in face.names() {
        let Some(s) = name.to_string() else { continue };
        let s = s.trim().to_string();
        if s.is_empty() {
            continue;
        }
        match name.name_id {
            ttf_parser::name_id::TYPOGRAPHIC_FAMILY if typographic.is_none() => {
                typographic = Some(s);
            }
            ttf_parser::name_id::FAMILY if family.is_none() => {
                family = Some(s);
            }
            _ => {}
        }
    }
    typographic.or(family)
}

/// Scan all font dirs and return deduped families, monospace-first then
/// alphabetical.
pub fn list_fonts() -> Vec<FontInfo> {
    let mut files: Vec<PathBuf> = Vec::new();
    for d in font_dirs() {
        collect_files(&d, &mut files, 0);
    }

    // family -> monospace (OR across all faces of the family).
    let mut map: BTreeMap<String, bool> = BTreeMap::new();
    for f in files {
        // Skip absurdly large files (corrupt / mislabeled) before reading them
        // fully into memory.
        if std::fs::metadata(&f)
            .map(|m| m.len() > 64 * 1024 * 1024)
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(data) = std::fs::read(&f) else { continue };
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        for i in 0..count {
            let Ok(face) = ttf_parser::Face::parse(&data, i) else { continue };
            if let Some(name) = family_name(&face) {
                // Skip hidden/system faces: '.'-prefixed (e.g. macOS ".SF NS")
                // and '@'-prefixed (Windows vertical-writing CJK aliases).
                if name.starts_with('.') || name.starts_with('@') {
                    continue;
                }
                let mono = face.is_monospaced();
                let e = map.entry(name).or_insert(false);
                *e = *e || mono;
            }
        }
    }

    let mut out: Vec<FontInfo> = map
        .into_iter()
        .map(|(family, monospace)| FontInfo { family, monospace })
        .collect();
    // monospace first; BTreeMap already gave alphabetical order within each group.
    out.sort_by(|a, b| {
        b.monospace
            .cmp(&a.monospace)
            .then_with(|| a.family.to_lowercase().cmp(&b.family.to_lowercase()))
    });
    out
}

#[tauri::command]
pub async fn list_system_fonts() -> Vec<FontInfo> {
    // The scan reads + parses hundreds of font files — run it off the IPC
    // thread so opening Settings never freezes the UI.
    tokio::task::spawn_blocking(list_fonts).await.unwrap_or_default()
}
