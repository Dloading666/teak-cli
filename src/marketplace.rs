//! Codex-compatible plugin marketplaces.
//!
//! Coffee CLI consumes any git repo that follows the Codex plugin-market
//! rule: a `.agents/plugins/marketplace.json` at the repo root listing
//! plugins, each with its own `.codex-plugin/plugin.json` manifest.
//!
//! Our model is deliberately dumb: we git-clone the repo, READ the rules
//! (name / description / icon / path), render a card, and — when the user
//! attaches a plugin in Gambit — hand the agent the plugin's on-disk PATH.
//! What the plugin actually is behind that path (SKILL.md, MCP connector,
//! whatever) is none of our business and not something we can run; the
//! agent reads the path and does what it can. No MCP wiring, no OAuth, no
//! per-tool install — just clone, display, inject path.
//!
//! Layout:
//! ```text
//!   ~/.coffee-cli/marketplace/<repo>/.agents/plugins/marketplace.json
//!   ~/.coffee-cli/marketplace/<repo>/plugins/<plugin>/.codex-plugin/plugin.json
//!   ~/.coffee-cli/marketplace/.enabled.json   ← which plugins show in the
//!                                                Gambit picker
//! ```
//! Management is manual: `open_marketplace_dir` opens the folder so the
//! user deletes a repo to remove its market. Simple.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "No home directory".to_string())
}

pub fn marketplace_root() -> Result<PathBuf, String> {
    Ok(home()?.join(".coffee-cli").join("marketplace"))
}

fn enabled_file() -> Result<PathBuf, String> {
    Ok(marketplace_root()?.join(".enabled.json"))
}

// ─── marketplace.json / plugin.json (only the fields we render) ──────────────

#[derive(Deserialize)]
struct MarketplaceManifest {
    #[serde(default)]
    interface: ManifestInterface,
    #[serde(default)]
    plugins: Vec<ManifestPlugin>,
}

#[derive(Deserialize, Default)]
struct ManifestInterface {
    #[serde(rename = "displayName", default)]
    display_name: String,
}

#[derive(Deserialize)]
struct ManifestPlugin {
    #[serde(default)]
    name: String,
    #[serde(default)]
    source: PluginSource,
}

#[derive(Deserialize, Default)]
struct PluginSource {
    #[serde(default)]
    path: String,
}

#[derive(Deserialize, Default)]
struct PluginManifest {
    #[serde(default)]
    interface: PluginInterface,
}

#[derive(Deserialize, Default)]
struct PluginInterface {
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "shortDescription", default)]
    short_description: String,
    #[serde(rename = "composerIcon", default)]
    composer_icon: String,
    #[serde(default)]
    logo: String,
}

// ─── Payload returned to the frontend ───────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MarketplacePlugin {
    /// Stable key = "<marketplaceId>::<pluginName>". Used for the enabled set.
    pub key: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub icon_data_url: Option<String>,
    /// Absolute on-disk path to the plugin directory — what the Gambit pill
    /// injects into the agent prompt.
    pub path: String,
    pub enabled: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Marketplace {
    /// Repo dir name under ~/.coffee-cli/marketplace (also the remove unit).
    pub id: String,
    pub display_name: String,
    pub plugins: Vec<MarketplacePlugin>,
}

// ─── Enabled set (which plugins appear in the Gambit picker) ─────────────────

fn read_enabled() -> Vec<String> {
    let Ok(path) = enabled_file() else { return Vec::new() };
    let Ok(raw) = fs::read_to_string(&path) else { return Vec::new() };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_enabled(keys: &[String]) -> Result<(), String> {
    let path = enabled_file()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    let raw = serde_json::to_string(keys).map_err(|e| format!("serialize: {}", e))?;
    fs::write(&path, raw).map_err(|e| format!("write enabled.json: {}", e))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Derive the local dir name from a git URL: last path segment minus `.git`.
/// `https://cnb.cool/echobird/codex-wps.git` → `codex-wps`. Sanitised so a
/// crafted URL can't escape the marketplace dir.
fn repo_name_from_url(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let last = last.strip_suffix(".git").unwrap_or(last);
    last.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect::<String>()
        .trim_matches('.')
        .to_string()
}

/// Resolve a manifest-relative asset path (`./assets/icon.svg`) under
/// `base`, read it, and return a base64 data URL. None if missing / too big.
fn icon_data_url(base: &Path, rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    let rel_clean = rel.trim_start_matches("./").replace('\\', "/");
    // Reject path traversal — assets must live under the plugin dir.
    if rel_clean.split('/').any(|seg| seg == "..") {
        return None;
    }
    let path = base.join(&rel_clean);
    let bytes = fs::read(&path).ok()?;
    if bytes.len() > 256 * 1024 {
        return None;
    }
    let mime = if rel_clean.ends_with(".svg") {
        "image/svg+xml"
    } else if rel_clean.ends_with(".png") {
        "image/png"
    } else if rel_clean.ends_with(".jpg") || rel_clean.ends_with(".jpeg") {
        "image/jpeg"
    } else if rel_clean.ends_with(".webp") {
        "image/webp"
    } else {
        return None;
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{};base64,{}", mime, b64))
}

// ─── Commands ────────────────────────────────────────────────────────────────

/// Clone (or refresh) a Codex-compatible marketplace repo. Runs git on a
/// blocking thread so the IPC dispatcher returns immediately. Errors if git
/// isn't installed, the URL is bad, or the repo lacks the marketplace rule.
#[tauri::command]
pub async fn add_marketplace(git_url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || add_marketplace_blocking(&git_url))
        .await
        .map_err(|e| format!("marketplace task join failed: {e}"))?
}

fn add_marketplace_blocking(git_url: &str) -> Result<(), String> {
    let url = git_url.trim();
    if url.is_empty() {
        return Err("Empty git URL".to_string());
    }
    let name = repo_name_from_url(url);
    if name.is_empty() {
        return Err(format!("Could not derive a folder name from: {}", url));
    }
    let root = marketplace_root()?;
    fs::create_dir_all(&root).map_err(|e| format!("mkdir marketplace: {}", e))?;
    let dest = root.join(&name);

    let run_git = |args: &[&str]| -> Result<(), String> {
        let mut cmd = std::process::Command::new("git");
        cmd.args(args);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let out = cmd
            .output()
            .map_err(|e| format!("git not available: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() {
                format!("git {:?} failed", args)
            } else {
                err
            });
        }
        Ok(())
    };

    if dest.join(".git").is_dir() {
        // Already added — refresh to latest.
        run_git(&["-C", &dest.to_string_lossy(), "pull", "--ff-only"])?;
    } else {
        // Fresh clone (shallow — we only need current files).
        run_git(&["clone", "--depth", "1", url, &dest.to_string_lossy()])?;
    }

    // Validate the Codex marketplace rule so a wrong URL surfaces clearly
    // instead of silently adding an empty card.
    if !dest.join(".agents/plugins/marketplace.json").is_file() {
        return Err(format!(
            "Cloned, but {} has no .agents/plugins/marketplace.json — not a Codex-compatible marketplace.",
            name
        ));
    }
    Ok(())
}

/// Enumerate every cloned marketplace and its plugins for display.
#[tauri::command]
pub fn list_marketplaces() -> Result<Vec<Marketplace>, String> {
    let root = marketplace_root()?;
    let mut out: Vec<Marketplace> = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    let enabled = read_enabled();
    let entries = fs::read_dir(&root).map_err(|e| format!("read {}: {}", root.display(), e))?;
    for entry in entries.flatten() {
        let repo = entry.path();
        if !repo.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.starts_with('.') {
            continue; // skip .enabled.json's parent artifacts etc.
        }
        let manifest_path = repo.join(".agents/plugins/marketplace.json");
        let Ok(raw) = fs::read_to_string(&manifest_path) else { continue };
        let Ok(manifest) = serde_json::from_str::<MarketplaceManifest>(&raw) else { continue };

        let display_name = if manifest.interface.display_name.is_empty() {
            id.clone()
        } else {
            manifest.interface.display_name.clone()
        };

        let mut plugins: Vec<MarketplacePlugin> = Vec::new();
        for p in &manifest.plugins {
            if p.name.is_empty() || p.source.path.is_empty() {
                continue;
            }
            let rel = p.source.path.trim_start_matches("./").replace('\\', "/");
            if rel.split('/').any(|seg| seg == "..") {
                continue;
            }
            let plugin_dir = repo.join(&rel);
            if !plugin_dir.is_dir() {
                continue;
            }
            // Per-plugin manifest (name / description / icon). Missing manifest
            // → fall back to the marketplace.json plugin name.
            let pm: PluginManifest = fs::read_to_string(plugin_dir.join(".codex-plugin/plugin.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let display = if pm.interface.display_name.is_empty() {
                p.name.clone()
            } else {
                pm.interface.display_name.clone()
            };
            let icon = icon_data_url(&plugin_dir, &pm.interface.composer_icon)
                .or_else(|| icon_data_url(&plugin_dir, &pm.interface.logo));
            let key = format!("{}::{}", id, p.name);
            let is_enabled = enabled.iter().any(|k| k == &key);
            plugins.push(MarketplacePlugin {
                enabled: is_enabled,
                path: plugin_dir.to_string_lossy().to_string(),
                key,
                name: p.name.clone(),
                display_name: display,
                description: pm.interface.short_description.clone(),
                icon_data_url: icon,
            });
        }

        out.push(Marketplace { id, display_name, plugins });
    }
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    Ok(out)
}

/// Enable / disable a plugin (whether it appears in the Gambit skill picker).
#[tauri::command]
pub fn set_marketplace_plugin_enabled(key: String, enabled: bool) -> Result<(), String> {
    let mut keys = read_enabled();
    let present = keys.iter().any(|k| k == &key);
    if enabled && !present {
        keys.push(key);
    } else if !enabled && present {
        keys.retain(|k| k != &key);
    } else {
        return Ok(());
    }
    write_enabled(&keys)
}

/// Open the marketplace folder in the OS file manager so the user can delete
/// a repo to remove its market. Opens the dir itself (not select).
#[tauri::command]
pub fn open_marketplace_dir() -> Result<(), String> {
    let root = marketplace_root()?;
    fs::create_dir_all(&root).map_err(|e| format!("mkdir marketplace: {}", e))?;
    let dir = root.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(dir.replace('/', "\\"))
            .spawn()
            .map_err(|e| format!("open explorer: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("open finder: {e}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("xdg-open: {e}"))?;
    }
    Ok(())
}
