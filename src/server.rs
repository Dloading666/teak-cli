use crate::terminal;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{State, Manager, Emitter};
use tauri_plugin_dialog::DialogExt;

/// Shared app state
pub struct AppState {
    pub terminal_session: terminal::SharedSession,
    /// Loopback port of the hook TCP server (set once during setup).
    /// 0 means the hook server failed to start; env var injection is skipped in that case.
    pub hook_port: std::sync::atomic::AtomicU16,
    /// Active OS fs watcher (one per app instance). Some(...) while a
    /// workspace folder is open; None otherwise. Swapping this Mutex'd
    /// Option replaces the watcher atomically on folder switch.
    pub fs_watcher: Mutex<Option<crate::fs_watcher::FsWatcher>>,
}


#[tauri::command]
fn window_minimize(window: tauri::Window) {
    let _ = window.minimize();
}

#[tauri::command]
fn window_maximize(window: tauri::Window) {
    let is_max = window.is_maximized().unwrap_or(false);
    if is_max { let _ = window.unmaximize(); } else { let _ = window.maximize(); }
}

#[tauri::command]
fn window_close(window: tauri::Window, app: tauri::AppHandle) {
    let label = window.label().to_string();
    if label == "main" {
        // Main window: close entire application (including all detached windows)
        app.exit(0);
    } else {
        // Detached window: just close this one
        let _ = window.close();
    }
}

#[tauri::command]
fn show_main_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[tauri::command]
async fn pick_folder(app: tauri::AppHandle) -> Result<String, String> {
    let folder = app
        .dialog()
        .file()
        .blocking_pick_folder();

    match folder {
        Some(path) => Ok(path.to_string()),
        None => Err("cancelled".to_string()),
    }
}

// ─── Tool Availability Detection ─────────────────────────────────────────────

/// PATH lookup wrapper. Used by hook_installer + agent_mcp_config to
/// gate config-file writes so we don't materialize stray `~/.codex/`,
/// `~/.config/opencode/`, etc. on machines where the user hasn't
/// installed the upstream CLI yet.
pub(crate) fn binary_on_path(bin: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        check_tool_windows(bin)
    }
    #[cfg(not(target_os = "windows"))]
    {
        check_tool_unix(bin)
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn check_tool_windows(bin: &str) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("where")
        .arg(bin)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn check_tool_unix(bin: &str) -> bool {
    std::process::Command::new("which")
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install hook scripts + upstream config patches for a single tool.
/// Called from the launchpad's focus-rescan when `check_tools_installed`
/// flips a CLI from not-installed → installed, so users who install a
/// CLI while Coffee CLI is running pick up tab status indicators
/// without restarting. No-op for tools the hook installer doesn't
/// manage. Idempotent.
#[tauri::command]
fn install_hook_for_tool(tool: String) {
    crate::hook_installer::install_for_tool(&tool);
}

#[tauri::command]
fn check_tools_installed() -> std::collections::HashMap<String, bool> {
    let mut result = std::collections::HashMap::new();
    for tool in crate::tools::TOOLS {
        result.insert(tool.id.to_string(), binary_on_path(tool.binary_name));
    }
    // `terminal` (system shell) and `remote` (SSH) have no binary to
    // probe — always available, not registered.
    result.insert("terminal".to_string(), true);
    result
}

// ─── File System Live Watcher ────────────────────────────────────────────────
//
// Start/stop a recursive fs watcher on the workspace folder so changes
// made by external tools (terminal CLIs, editors, git, etc.) propagate
// into the Explorer tree immediately. See fs_watcher.rs for mechanics.

#[tauri::command]
fn start_fs_watcher(
    path: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let root = PathBuf::from(&path);
    let watcher = crate::fs_watcher::FsWatcher::start(app, root)?;
    // Replace atomically; dropping the old FsWatcher stops its OS handle.
    let mut guard = state.fs_watcher.lock().map_err(|e| format!("lock: {}", e))?;
    *guard = Some(watcher);
    Ok(())
}

#[tauri::command]
fn stop_fs_watcher(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.fs_watcher.lock().map_err(|e| format!("lock: {}", e))?;
    // Drop releases the OS watcher handle.
    *guard = None;
    Ok(())
}

// ─── File System Browsing API ────────────────────────────────────────────────

/// Information about a single directory entry (file or folder)
#[derive(Serialize)]
struct DirEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
}

/// List the immediate children of a directory.
/// Returns files and subdirectories sorted: directories first, then files, both alphabetical.
#[tauri::command]
fn list_directory(path: String) -> Result<Vec<DirEntry>, String> {
    let dir = std::path::Path::new(&path);
    if !dir.is_dir() {
        return Err(format!("Not a directory: {}", path));
    }

    let mut entries: Vec<DirEntry> = Vec::new();

    let read_dir = std::fs::read_dir(dir).map_err(|e| format!("Cannot read directory: {}", e))?;

    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // Skip unreadable entries
        };
        let name = entry.file_name().to_string_lossy().to_string();

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue, // Skip unreadable entries
        };

        entries.push(DirEntry {
            name,
            path: entry.path().to_string_lossy().to_string(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }

    // Sort: directories first, then files, both alphabetical (case insensitive)
    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return if a.is_dir { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }
        a.name.to_lowercase().cmp(&b.name.to_lowercase())
    });

    Ok(entries)
}


fn stats_is_text(bytes: &[u8]) -> bool {
    // Same heuristic git uses: any null byte in first 8 KB → treat as binary.
    !bytes[..bytes.len().min(8192)].contains(&0u8)
}


/// Canonical form of a path used as a snapshot map key. Forward-slashes
/// always; on Windows the drive letter is forced to uppercase. Reason:
/// `start_folder_snapshot` walks the dir the user picked (typically
/// uppercase `D:\…`) and writes keys like `D:/Coffee-CLI/…`, but
/// Claude Code's PostToolUse hook reports `tool_input.file_path` with
/// whatever casing the model chose — often lowercase `d:\…`. HashMap
/// is case-sensitive, so without this normalization every per-call
/// hook event misses the baseline and the audit list fills up with
/// bogus "+N -0" rows from the no-baseline fall-through branch.
pub(crate) fn normalize_path_key(path: &str) -> String {
    let s = path.replace('\\', "/");
    #[cfg(windows)]
    {
        let bytes = s.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            let mut out = String::with_capacity(s.len());
            out.push((bytes[0] as char).to_ascii_uppercase());
            out.push_str(&s[1..]);
            return out;
        }
    }
    s
}


/// Read a text file from disk as UTF-8 string. `None` when the file doesn't
/// exist, can't be read, or fails the text-vs-binary heuristic. Pairs with
/// `get_baseline_content` to feed the right-side Diff panel: baseline +
/// current = both sides of the diff.
#[tauri::command]
fn read_text_file(path: String) -> Option<String> {
    let bytes = std::fs::read(&path).ok()?;
    if !stats_is_text(&bytes) { return None; }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}


/// Save a base64-encoded clipboard image to a temp file.
/// Used by the Gambit compose window so pasted screenshots can be referenced
/// by path when forwarded to AI CLI agents (Claude Code, etc.).
///
/// Guards:
/// - Extension whitelisted to common raster formats
/// - Hard 25 MB size cap to prevent runaway base64 payloads filling the disk
/// - Filename uses pid + atomic counter so two concurrent paste calls (same
///   millisecond) can never collide and truncate each other's file
#[tauri::command]
fn save_clipboard_image(data_base64: String, extension: String) -> Result<String, String> {
    use base64::{Engine as _, engine::general_purpose};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const MAX_BYTES: usize = 25 * 1024 * 1024; // 25 MB

    // Only allow common web image formats. Block anything that could
    // execute or exploit a path-traversal quirk in the extension.
    let ext = match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => extension,
        _ => return Err(format!("Unsupported image extension: {}", extension)),
    };

    let bytes = general_purpose::STANDARD
        .decode(&data_base64)
        .map_err(|e| format!("base64 decode: {}", e))?;

    if bytes.len() > MAX_BYTES {
        return Err(format!(
            "Image too large: {} bytes (max {})",
            bytes.len(),
            MAX_BYTES
        ));
    }

    let tmp_dir = std::env::temp_dir().join("coffee-cli").join("pasted-images");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("mkdir: {}", e))?;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = tmp_dir.join(format!("clip-{}-{}-{}.{}", stamp, pid, seq, ext));

    let mut file = std::fs::File::create(&path)
        .map_err(|e| format!("create image file: {}", e))?;
    file.write_all(&bytes)
        .map_err(|e| format!("write image bytes: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

// ─── File System Operations ───────────────────────────────────────────────────

/// Open the native file explorer and highlight / reveal the given path.
#[tauri::command]
fn show_in_folder(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // explorer /select, highlights the item in its parent folder.
        // The frontend normalizes paths to forward slashes, but explorer.exe
        // requires backslashes — forward slashes cause it to silently open Desktop.
        let win_path = path.replace('/', "\\");
        std::process::Command::new("explorer")
            .arg("/select,")
            .arg(&win_path)
            .spawn()
            .map_err(|e| format!("Failed to open Explorer: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        let p = std::path::Path::new(&path);
        std::process::Command::new("open")
            .arg("-R") // Reveal in Finder
            .arg(p)
            .spawn()
            .map_err(|e| format!("Failed to open Finder: {e}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        let p = std::path::Path::new(&path);
        // Open the parent directory; most Linux file managers don't support select
        let dir = if p.is_dir() { p.to_path_buf() } else { p.parent().unwrap_or(p).to_path_buf() };
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map_err(|e| format!("Failed to open file manager: {e}"))?;
    }
    Ok(())
}

/// Validate that a path is safe to operate on:
/// - Canonicalizes the path (resolves `..` and symlinks)
/// - Rejects paths with fewer than 3 components (drive root, OS dirs, etc.)
fn validate_fs_path(path: &str) -> Result<std::path::PathBuf, String> {
    let canonical = std::path::Path::new(path)
        .canonicalize()
        .map_err(|e| format!("Invalid path: {e}"))?;
    // Require at least 3 components, e.g. C:\Users\foo or /home/user
    // This blocks C:\, C:\Windows, /, /etc, /usr, etc.
    if canonical.components().count() < 3 {
        return Err("Operation rejected: path is too shallow (system-level directory)".to_string());
    }
    Ok(canonical)
}

/// Delete a file or directory permanently (no recycle bin).
#[tauri::command]
fn fs_delete(path: String) -> Result<(), String> {
    let p = validate_fs_path(&path)?;
    if p.is_dir() {
        std::fs::remove_dir_all(&p).map_err(|e| format!("Delete failed: {e}"))
    } else {
        std::fs::remove_file(&p).map_err(|e| format!("Delete failed: {e}"))
    }
}

/// Rename / move a path to a new name within the same parent directory.
#[tauri::command]
fn fs_rename(path: String, new_name: String) -> Result<(), String> {
    let src = validate_fs_path(&path)?;
    let dest = src.parent()
        .ok_or_else(|| "No parent directory".to_string())?
        .join(&new_name);
    std::fs::rename(&src, dest).map_err(|e| format!("Rename failed: {e}"))
}

/// Paste (copy or move) a file/directory into a target directory.
/// `action` is either "copy" or "cut".
#[tauri::command]
fn fs_paste(action: String, src_path: String, target_dir: String) -> Result<(), String> {
    let src = validate_fs_path(&src_path)?;
    // target_dir may not exist yet for copy — validate its parent instead
    let target_canonical = std::path::Path::new(&target_dir)
        .canonicalize()
        .map_err(|e| format!("Invalid target directory: {e}"))?;
    if target_canonical.components().count() < 3 {
        return Err("Operation rejected: target is a system-level directory".to_string());
    }
    let file_name = src.file_name().ok_or("Invalid source path")?;
    let dest = target_canonical.join(file_name);

    match action.as_str() {
        "cut" => {
            std::fs::rename(&src, &dest).map_err(|e| format!("Move failed: {e}"))
        }
        "copy" => {
            if src.is_dir() {
                copy_dir_all(&src, &dest).map_err(|e| format!("Copy dir failed: {e}"))
            } else {
                std::fs::copy(&src, &dest).map(|_| ()).map_err(|e| format!("Copy failed: {e}"))
            }
        }
        _ => Err(format!("Unknown action: {action}")),
    }
}

/// Recursively copy a directory and all its contents.
fn copy_dir_all(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dest.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

// ─── Tier Terminal API ────────────────────────────────────────────────────────

#[tauri::command]
async fn tier_terminal_start(
    session_id: String,
    tool: Option<String>,
    tool_data: Option<String>,
    cols: u16,
    rows: u16,
    theme_mode: Option<String>,
    locale: Option<String>,
    cwd: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Offload the whole spawn sequence to a blocking thread so the Tauri
    // command dispatcher returns immediately. Without this, Windows was
    // paying ~cmd.exe boot + Defender AV scan + Node startup on the command
    // thread, stalling every other IPC call (resize, theme, terminal I/O)
    // until the spawn returned. Running in the terminal directly avoids
    // this because no IPC layer is involved — the shell forks directly.
    let terminal_session = state.terminal_session.clone();
    tauri::async_runtime::spawn_blocking(move || {
        tier_terminal_start_blocking(
            session_id, tool, tool_data, cols, rows,
            theme_mode, locale, cwd, app, terminal_session,
        )
    })
    .await
    .map_err(|e| format!("Spawn task join failed: {e}"))?
}

fn tier_terminal_start_blocking(
    session_id: String,
    tool: Option<String>,
    tool_data: Option<String>,
    cols: u16,
    rows: u16,
    theme_mode: Option<String>,
    locale: Option<String>,
    cwd: Option<String>,
    app: tauri::AppHandle,
    terminal_session: terminal::SharedSession,
) -> Result<(), String> {
    // CWD resolution order (first non-empty wins):
    //   1. cwd passed from the frontend (launchpad's folder picker / per-tab cwd)
    //   2. tool_config.default_cwd from ~/.coffee-cli/tools.json (WSL-type users
    //      who want a fixed launch dir regardless of launchpad selection)
    //   3. empty → spawn process inherits Coffee CLI's own cwd
    //
    // The launchpad picker dominates because it's the per-launch user choice;
    // tool_config.default_cwd is the always-on fallback for users who don't
    // want to pick each time (or whose launchpad-side path can't address the
    // tool's actual workspace, e.g. WSL).
    let frontend_cwd = cwd.unwrap_or_default();
    let dir = if !frontend_cwd.is_empty() {
        std::path::PathBuf::from(frontend_cwd)
    } else if let Some(name) = tool.as_deref() {
        let cfg_cwd = crate::tool_config::get(name).default_cwd;
        if cfg_cwd.is_empty() {
            std::path::PathBuf::default()
        } else {
            std::path::PathBuf::from(cfg_cwd)
        }
    } else {
        std::path::PathBuf::default()
    };

    // Resolve the binary + default args from the tool registry. `remote`
    // and the fallback shell are not in the registry — `remote` parses
    // tool_data at runtime, the shell is platform-derived.
    let registry_descriptor = tool.as_deref().and_then(crate::tools::find);
    let (cmd, args): (String, Vec<String>) = match (tool.as_deref(), registry_descriptor) {
        (Some(_id), Some(descriptor)) => {
            let a: Vec<String> = descriptor
                .default_args
                .iter()
                .map(|s| s.to_string())
                .collect();
            (descriptor.binary_name.to_string(), a)
        }
        (Some("remote"), _) => {
            // Parse connection info from toolData JSON
            let data = tool_data.as_deref().unwrap_or("{}");
            let conn: serde_json::Value = serde_json::from_str(data)
                .map_err(|e| format!("Invalid remote connection data: {}", e))?;

            let protocol = conn["protocol"].as_str().unwrap_or("ssh");
            let host = conn["host"].as_str().unwrap_or("localhost");
            let port = conn["port"].as_u64().unwrap_or(if protocol == "ssh" { 22 } else { 7681 });
            let username = conn["username"].as_str().unwrap_or("root");
            let _password = conn["password"].as_str().unwrap_or("");

            if protocol == "ssh" {
                // Build SSH command — user will type password interactively in PTY
                let mut ssh_args = vec![
                    "-o".to_string(),
                    "StrictHostKeyChecking=no".to_string(),
                    "-p".to_string(),
                    port.to_string(),
                    format!("{}@{}", username, host),
                ];

                // If password is provided, try to use sshpass for auto-login
                // Otherwise user types password in terminal
                if !_password.is_empty() {
                    // Check if sshpass is available
                    let has_sshpass = if cfg!(target_os = "windows") {
                        false // sshpass not typically available on Windows
                    } else {
                        std::process::Command::new("which")
                            .arg("sshpass")
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false)
                    };

                    if has_sshpass {
                        let mut full_args = vec![
                            "-p".to_string(),
                            _password.to_string(),
                            "ssh".to_string(),
                        ];
                        full_args.append(&mut ssh_args);
                        ("sshpass".to_string(), full_args)
                    } else {
                        ("ssh".to_string(), ssh_args)
                    }
                } else {
                    ("ssh".to_string(), ssh_args)
                }
            } else {
                // WebSocket protocol — not handled by PTY backend
                // Frontend will handle this via xterm.js AttachAddon directly
                return Err("ws".to_string());
            }
        },

        _ => if cfg!(target_os = "windows") {
            ("powershell.exe".to_string(), vec!["-NoExit".to_string()])
        } else {
            ("bash".to_string(), vec!["-l".to_string(), "-i".to_string()])
        }
    };

    // ── User-configurable launch overrides ─────────────────────────────────
    // ~/.coffee-cli/tools.json lets users say e.g. "always launch claude with
    // --dangerously-skip-permissions" or "run codex through `docker exec mybox`".
    // `remote` is excluded by design: its argv is protocol-derived from
    // runtime tool_data, not configurable.
    let (cmd, args) = match tool.as_deref() {
        Some(name) if name == "terminal" || crate::tools::find(name).is_some() => {
            let entry = crate::tool_config::get(name);
            let (cmd, mut args) = (cmd, args);
            let (cmd, args) = if let (Some(bin), prefix_args) =
                crate::tool_config::parse_command(&entry.command)
            {
                // User overrode the binary. Prepend any prefix args
                // (e.g. for `wsl claude`, prefix_args = ["claude"]) so
                // the original built-in args (--mcp-config / --append-
                // system-prompt / etc) follow them.
                let mut new_args = prefix_args;
                new_args.append(&mut args);
                (bin, new_args)
            } else {
                (cmd, args)
            };
            // Append user's extra_args after the built-in flags so
            // they take precedence (e.g. user can override --approval-
            // mode by adding their own at the end).
            let mut args = args;
            args.extend(entry.extra_args.iter().cloned());
            (cmd, args)
        }
        // Synthetic / pane-internal tools (remote / multi-agent)
        // intentionally bypass user override.
        _ => (cmd, args),
    };

    // If a session with the same ID already exists (e.g. restart-in-place),
    // forcefully kill and remove it before spawning a fresh one.
    {
        let mut lock = terminal_session.lock().unwrap();
        if let Some(old_session) = lock.remove(&session_id) {
            eprintln!("[Tier Terminal] Killing existing session {} for restart", session_id);
            let _ = old_session.kill_tx.send(());
            // Brief pause to let the OS reclaim PTY resources
            drop(lock);
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    // Determine the CWD to pass to the Agent:
    // 1. If workspace has an explicit dir (from open-folder or resume) → use it
    // 2. Otherwise default to user's home dir (matches most agents' default)
    let spawn_cwd = if dir.as_os_str().is_empty() || !dir.is_dir() {
        dirs::home_dir().map(|p| p.to_string_lossy().to_string())
    } else {
        Some(dir.to_string_lossy().to_string())
    };

    let tool_name = tool.clone();
    let actual_cwd = spawn_cwd.clone().unwrap_or_default();

    eprintln!("[Tier Terminal] Starting tool={:?}, cmd={}, args={:?}, cwd={:?}", tool, cmd, args, spawn_cwd);

    // Per-pane env overrides (reserved). Independent-split and
    // single-terminal panes spawn with no extra env today.
    let extra_env: Vec<(String, String)> = Vec::new();

    terminal::spawn(
        app.clone(),
        session_id.clone(),
        terminal_session.clone(),
        cmd,
        args,
        spawn_cwd,
        locale.clone().unwrap_or_else(|| "en".to_string()),
        cols,
        rows,
        tool_name.clone(),
        theme_mode,
        locale,
        extra_env,
    ).map_err(|e| format!("Failed to spawn PTY: {}", e))?;

    // Emit the initial CWD to the frontend so the left panel can map immediately.
    // On Windows, cmd.exe does not emit OSC 7, and full-screen agents enter alt-screen
    // before any shell prompt appears. This one-time emit bridges the gap.
    if !actual_cwd.is_empty() {
        #[derive(serde::Serialize, Clone)]
        struct CwdPayload { id: String, cwd: String }
        let _ = app.emit("tier-terminal-cwd", CwdPayload {
            id: session_id,
            cwd: actual_cwd,
        });
    }

    Ok(())
}


#[tauri::command]
fn tier_terminal_input(
    session_id: String,
    data: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Step 1: grab Arc handles while holding the map lock (cheap clones, no IO)
    let (writer_arc, activity_arc) = {
        let map = state.terminal_session.lock().unwrap();
        match map.get(&session_id) {
            Some(s) => (s.writer_lock.clone(), s.activity.clone()),
            None => return Err(format!("No active terminal session for id: {}", session_id)),
        }
    };
    // Map lock released — other tabs can now proceed concurrently

    // Step 2: PTY write (syscall, may block under back-pressure)
    use std::io::Write;
    let mut w = writer_arc.lock().map_err(|e| format!("Writer lock poisoned: {}", e))?;
    w.write_all(data.as_bytes()).map_err(|e| format!("Write failed: {}", e))?;
    w.flush().map_err(|e| format!("Flush failed: {}", e))?;
    drop(w);

    // Step 3: Dual-signal — detect user prompt submission
    // Only trigger "working" when user presses Enter while agent is at prompt.
    // System-generated input (auto-skip) uses tier_terminal_raw_write instead.
    if data.contains('\r') || data.contains('\n') {
        if let Ok(mut act) = activity_arc.lock() {
            if act.last_status == "wait_input" {
                act.user_submitted_at = Some(std::time::Instant::now());
            }
        }
    }

    Ok(())
}

/// Raw write to PTY without triggering agent-status detection.
/// Used for system-generated input like auto-skip Enter for Claude trust prompt.
#[tauri::command]
fn tier_terminal_raw_write(
    session_id: String,
    data: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Grab writer Arc, release map lock before PTY I/O
    let writer_arc = {
        let map = state.terminal_session.lock().unwrap();
        match map.get(&session_id) {
            Some(s) => s.writer_lock.clone(),
            None => return Err(format!("No active terminal session for id: {}", session_id)),
        }
    };

    use std::io::Write;
    let mut w = writer_arc.lock().map_err(|e| format!("Writer lock poisoned: {}", e))?;
    w.write_all(data.as_bytes()).map_err(|e| format!("Write failed: {}", e))?;
    w.flush().map_err(|e| format!("Flush failed: {}", e))?;
    Ok(())
}

/// Toggle the global background-throttle flag. Called by the frontend
/// from a `document.visibilitychange` listener: when the OS hides the
/// Coffee CLI window (other Space, app switched away, minimized) we
/// widen every per-session worker's polling cadence so the app drops
/// to near-zero CPU instead of running its full foreground loop.
#[tauri::command]
fn set_background_mode(hidden: bool) {
    crate::terminal::BACKGROUND_MODE
        .store(hidden, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
fn tier_terminal_kill(session_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let map = state.terminal_session.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        let _ = session.kill_tx.send(());
    }
    Ok(())
}

#[tauri::command]
fn tier_terminal_resize(
    session_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use portable_pty::PtySize;
    let map = state.terminal_session.lock().unwrap();
    if let Some(session) = map.get(&session_id) {
        let master_guard = session._master.lock().unwrap();
        if let Some(ref master) = *master_guard {
            let size = PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            };
            master.resize(size).map_err(|e| format!("Resize failed: {}", e))?;
        }
    }
    Ok(())
}

// ─── Session Resume API ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct SavedSession {
    id: String,
    name: String,
    tool: String,
    cwd: String,
    session_token: Option<String>,
    saved_at: String,
    file_path: Option<String>,
    turn_count: Option<u32>,
}

fn sessions_file_path() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".coffee-cli");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("sessions.json")
}

/// XML-style tags injected into the user message stream by Claude /
/// Codex when integrated with an IDE or shell. These are not things
/// the user typed — filtering them out of the history title extractor
/// keeps the sidebar readable (no more "<ide_opened_file>The user
/// opened the..." or "# AGENTS.md instructions for ..." cards).
const SYSTEM_INJECTION_TAGS: &[&str] = &[
    "<environment_context>",
    "<ide_opened_file>",
    "<ide_closed_file>",
    "<ide_selection>",
    "<system-reminder>",
    "<command-message>",
    "<command-name>",
    // Codex injects the contents of `AGENTS.md` (project) and any
    // pre-v1.5 Coffee-CLI workspace pointer as a synthetic user
    // message at session start.
    "# AGENTS.md",
    // Retained for orphan Gemini CLI sessions (see
    // parse_gemini_session_jsonl) — Gemini's IDE integration injected
    // the contents of `GEMINI.md` as a synthetic user message at
    // session start, and we still filter those out when extracting
    // titles for the history list. Coffee CLI no longer ships a
    // Gemini tool tile, but legacy session files keep this constant
    // relevant.
    "# GEMINI.md",
];

fn is_system_injected(text: &str) -> bool {
    let t = text.trim();
    SYSTEM_INJECTION_TAGS.iter().any(|tag| t.starts_with(tag))
}

fn parse_agent_jsonl(file_path: &std::path::Path, tool_name: &str) -> Option<SavedSession> {
    use std::io::BufRead;
    let file = std::fs::File::open(file_path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut session_id = file_path.file_stem()?.to_string_lossy().to_string();
    let mut cwd = String::new();
    let mut updated_at = String::new();
    let mut title = String::new();
    let mut total_messages = 0;

    for line in reader.lines().map_while(Result::ok) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(s) = value.get("sessionId").and_then(|v| v.as_str()) {
                if !s.is_empty() { session_id = s.to_string(); }
            }
            if let Some(c) = value.get("cwd").and_then(|v| v.as_str()) {
                if cwd.is_empty() && !c.is_empty() { cwd = c.to_string(); }
            }
            let mut maybe_msg_obj = value.get("message").and_then(|v| v.as_object());
            if maybe_msg_obj.is_none() {
                if let Some(payload) = value.get("payload").and_then(|v| v.as_object()) {
                    if let Some(ptype) = payload.get("type").and_then(|v| v.as_str()) {
                        if ptype == "message" {
                            maybe_msg_obj = Some(payload);
                        }
                    }
                }
            }

            if let Some(msg_obj) = maybe_msg_obj {
                if let Some(role) = msg_obj.get("role").and_then(|v| v.as_str()) {
                    if role == "user" || role == "assistant" {
                        total_messages += 1;
                    }
                    if role == "user" && title.is_empty() {
                        if let Some(content_str) = msg_obj.get("content").and_then(|v| v.as_str()) {
                            // Skip whole-message IDE/system injections so the
                            // next real user line becomes the title.
                            if !is_system_injected(content_str) {
                                let content_safe = content_str.replace('\n', " ");
                                let mut chars = content_safe.chars();
                                let t: String = chars.by_ref().take(40).collect();
                                title = if chars.next().is_some() { format!("{}...", t) } else { t };
                            }
                        } else if let Some(content_arr) = msg_obj.get("content").and_then(|v| v.as_array()) {
                            // Extract text from object array
                            for block in content_arr {
                                if let Some(t) = block.get("type").and_then(|v| v.as_str()) {
                                    if t == "text" || t == "input_text" {
                                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                            if is_system_injected(text) {
                                                continue; // skip IDE / system-injected prompts
                                            }
                                            let safe_text = text.replace('\n', " ");
                                            let mut chars = safe_text.chars();
                                            let chunk: String = chars.by_ref().take(40).collect();
                                            title = if chars.next().is_some() { format!("{}...", chunk) } else { chunk };
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Fallback: If cwd is still empty, derive it from the parent project folder name
    // e.g., "D--Coffee-Code" -> "D:\Coffee-Code", "C--Users--..." -> "C:\Users\..."
    if cwd.is_empty() {
        if let Some(parent) = file_path.parent() {
            if let Some(folder_name) = parent.file_name().and_then(|n| n.to_str()) {
                if folder_name.contains("--") {
                    let mut parts = folder_name.split("--");
                    if let Some(drive) = parts.next() {
                        let rest: Vec<&str> = parts.collect();
                        let decoded_path = if cfg!(target_os = "windows") {
                            format!("{}:\\{}", drive, rest.join("\\"))
                        } else {
                            format!("/{}/{}", drive, rest.join("/"))
                        };
                        cwd = decoded_path;
                    }
                }
            }
        }
    }
    let turn_count = if total_messages > 0 { std::cmp::max(1, (total_messages + 1) / 2) } else { 0 };
    
    // Fallback date from file metadata
    if let Ok(meta) = std::fs::metadata(file_path) {
        if let Ok(mod_time) = meta.modified() {
            if let Ok(dur) = mod_time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                updated_at = dur.as_millis().to_string();
            }
        }
    }

    if title.is_empty() {
        let mut chars = tool_name.chars();
        let cap_name = match chars.next() {
            None => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        };
        title = format!("{} Session", cap_name);
    }

    Some(SavedSession {
        id: format!("{}_native_{}", tool_name, session_id),
        name: title,
        tool: tool_name.to_string(),
        cwd,
        session_token: Some(session_id),
        saved_at: updated_at,
        file_path: Some(file_path.to_string_lossy().into_owned()),
        turn_count: Some(turn_count),
    })
}

/// Codex CLI sessions live at
/// `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<uuid>.jsonl`.
/// Schema:
///   - first row: `{type: "session_meta", payload: {id, cwd, originator: "codex-tui", ...}}`
///   - subsequent rows: `{type: "response_item", payload: {type: "message", role, content: [{type: "input_text", text}]}}`
///     (also `user_message`, `event_msg`, `turn_context`, etc. — we ignore the non-message ones)
fn parse_codex_session_jsonl(file_path: &std::path::Path) -> Option<SavedSession> {
    use std::io::BufRead;
    let file = std::fs::File::open(file_path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut session_id = file_path.file_stem()?.to_string_lossy().to_string();
    let mut cwd = String::new();
    let mut updated_at = String::new();
    let mut title = String::new();
    let mut total_messages = 0;

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let row_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = match value.get("payload") {
            Some(p) => p,
            None => continue,
        };

        // Session meta: pull id + cwd off the first row.
        if row_type == "session_meta" {
            if let Some(id) = payload.get("id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    session_id = id.to_string();
                }
            }
            if let Some(c) = payload.get("cwd").and_then(|v| v.as_str()) {
                if !c.is_empty() {
                    cwd = c.to_string();
                }
            }
            continue;
        }

        // Message rows: response_item with payload.type=message, or
        // the dedicated user_message row type. Both wrap content as
        // an array of `{type: "input_text", text}` blocks.
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let is_msg = (row_type == "response_item" && payload_type == "message")
            || row_type == "user_message";
        if !is_msg {
            continue;
        }
        let role = payload.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if role == "user" || role == "assistant" {
            total_messages += 1;
        }
        if !title.is_empty() || role != "user" {
            continue;
        }
        let Some(content_arr) = payload.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for block in content_arr {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if block_type != "input_text" && block_type != "text" {
                continue;
            }
            let Some(text) = block.get("text").and_then(|v| v.as_str()) else {
                continue;
            };
            if is_system_injected(text) {
                continue; // skip AGENTS.md / environment_context wrappers
            }
            let safe = text.replace('\n', " ");
            let mut chars = safe.chars();
            let chunk: String = chars.by_ref().take(40).collect();
            title = if chars.next().is_some() { format!("{}...", chunk) } else { chunk };
            break;
        }
    }

    if let Ok(meta) = std::fs::metadata(file_path) {
        if let Ok(mod_time) = meta.modified() {
            if let Ok(dur) = mod_time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                updated_at = dur.as_millis().to_string();
            }
        }
    }
    if title.is_empty() {
        title = "Codex Session".to_string();
    }
    let turn_count = if total_messages > 0 { std::cmp::max(1, (total_messages + 1) / 2) } else { 0 };

    Some(SavedSession {
        id: format!("codex_native_{}", session_id),
        name: title,
        tool: "codex".to_string(),
        cwd,
        session_token: Some(session_id),
        saved_at: updated_at,
        file_path: Some(file_path.to_string_lossy().into_owned()),
        turn_count: Some(turn_count),
    })
}

/// Reader for `~/.gemini/tmp/<project>/chats/session-*.jsonl`.
///
/// Format origin is the (now-retired-as-launchpad-tile) Gemini CLI,
/// but the **agy** binary writes the exact same schema to the exact
/// same directory — verified on populated 2026-05-20 sessions.
/// Older Gemini sessions and newer Antigravity sessions are
/// indistinguishable by content (no app/version field), so Coffee
/// CLI labels everything in this dir as `tool="antigravity"`. Gemini
/// CLI as a separate product is retiring (consumer access ends
/// 2026-06-18), so the unified label matches user expectations
/// after they've moved to agy.
///
/// Schema:
///   - first row: `{sessionId, projectHash, startTime, lastUpdated, kind: "main"}`
///   - subsequent rows: `{id, timestamp, type: "user"|"gemini", content}`
///     where user content is `[{text}]` and gemini content is a string.
///   - interleaved `{$set: {lastUpdated}}` rows that we just skip.
///
/// `cwd` isn't recorded in the file. We resolve it from
/// `~/.gemini/projects.json` which maps absolute cwd → short folder
/// name, so we reverse-lookup short-name → cwd. Falls back to the
/// short folder name itself if the projects.json mapping is missing.
fn parse_gemini_session_jsonl(
    file_path: &std::path::Path,
    project_short_to_cwd: &std::collections::HashMap<String, String>,
) -> Option<SavedSession> {
    use std::io::BufRead;
    let file = std::fs::File::open(file_path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut session_id = file_path.file_stem()?.to_string_lossy().to_string();
    let mut cwd = String::new();
    let mut updated_at = String::new();
    let mut title = String::new();
    let mut total_messages = 0;

    if let Some(short) = file_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        if let Some(real) = project_short_to_cwd.get(short) {
            cwd = real.clone();
        } else {
            cwd = short.to_string();
        }
    }

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(s) = value.get("sessionId").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                session_id = s.to_string();
            }
        }
        let row_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if row_type == "user" || row_type == "gemini" {
            total_messages += 1;
        }
        if !title.is_empty() || row_type != "user" {
            continue;
        }
        let Some(content_arr) = value.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for block in content_arr {
            let Some(text) = block.get("text").and_then(|v| v.as_str()) else {
                continue;
            };
            if is_system_injected(text) {
                continue;
            }
            let safe = text.replace('\n', " ");
            let mut chars = safe.chars();
            let chunk: String = chars.by_ref().take(40).collect();
            title = if chars.next().is_some() { format!("{}...", chunk) } else { chunk };
            break;
        }
    }

    if let Ok(meta) = std::fs::metadata(file_path) {
        if let Ok(mod_time) = meta.modified() {
            if let Ok(dur) = mod_time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                updated_at = dur.as_millis().to_string();
            }
        }
    }
    if title.is_empty() {
        title = "Antigravity Session".to_string();
    }
    let turn_count = if total_messages > 0 { std::cmp::max(1, (total_messages + 1) / 2) } else { 0 };

    Some(SavedSession {
        id: format!("antigravity_native_{}", session_id),
        name: title,
        tool: "antigravity".to_string(),
        cwd,
        session_token: Some(session_id),
        saved_at: updated_at,
        file_path: Some(file_path.to_string_lossy().into_owned()),
        turn_count: Some(turn_count),
    })
}

/// Antigravity / Gemini project-hash → cwd map. Reads `~/.gemini/projects.json`
/// — same file both CLIs maintain (Gemini-format, written by agy too).
/// Returns empty on any error (missing file, invalid JSON, permission
/// denied) — sessions just fall back to using the short folder name
/// as the cwd display.
fn load_gemini_project_map() -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut map = HashMap::new();
    let Some(home) = dirs::home_dir() else { return map };
    let path = home.join(".gemini").join("projects.json");
    let Ok(text) = std::fs::read_to_string(path) else { return map };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { return map };
    let Some(projects) = value.get("projects").and_then(|v| v.as_object()) else { return map };
    for (cwd, short) in projects {
        if let Some(short_str) = short.as_str() {
            map.insert(short_str.to_string(), cwd.clone());
        }
    }
    map
}

/// Parse a Qwen Code session jsonl. Layout:
///   `~/.qwen/projects/<sanitized-cwd>/chats/<session>.jsonl`
/// Each line: `{uuid, type: 'user'|'assistant'|'tool_result'|'system',
///   sessionId, cwd, timestamp, message: {role, parts: [{text|functionCall|...}]}}`
/// Qwen Code is descended from the (now-retired-as-launchpad-tile)
/// Gemini CLI but its layout differs from upstream:
///   • cwd is on every row (no separate projects.json reverse map needed)
///   • text lives in `message.parts[].text`, not the top-level `content[]`
///   • assistant rows use `type: 'assistant'`, not `'gemini'`
fn parse_qwen_session_jsonl(file_path: &std::path::Path) -> Option<SavedSession> {
    use std::io::BufRead;
    let file = std::fs::File::open(file_path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut session_id = file_path.file_stem()?.to_string_lossy().to_string();
    let mut cwd = String::new();
    let mut updated_at = String::new();
    let mut title = String::new();
    let mut total_messages = 0;

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(s) = value.get("sessionId").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                session_id = s.to_string();
            }
        }
        if cwd.is_empty() {
            if let Some(c) = value.get("cwd").and_then(|v| v.as_str()) {
                cwd = c.to_string();
            }
        }
        let row_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if row_type == "user" || row_type == "assistant" {
            total_messages += 1;
        }
        // Title: first user message, first 40 chars.
        if !title.is_empty() || row_type != "user" {
            continue;
        }
        let Some(parts) = value
            .get("message")
            .and_then(|m| m.get("parts"))
            .and_then(|v| v.as_array())
        else {
            continue;
        };
        for block in parts {
            let Some(text) = block.get("text").and_then(|v| v.as_str()) else {
                continue;
            };
            if is_system_injected(text) {
                continue;
            }
            let safe = text.replace('\n', " ");
            let mut chars = safe.chars();
            let chunk: String = chars.by_ref().take(40).collect();
            title = if chars.next().is_some() { format!("{}...", chunk) } else { chunk };
            break;
        }
    }

    if let Ok(meta) = std::fs::metadata(file_path) {
        if let Ok(mod_time) = meta.modified() {
            if let Ok(dur) = mod_time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                updated_at = dur.as_millis().to_string();
            }
        }
    }
    if title.is_empty() {
        title = "Qwen Session".to_string();
    }
    let turn_count = if total_messages > 0 { std::cmp::max(1, (total_messages + 1) / 2) } else { 0 };

    Some(SavedSession {
        id: format!("qwen_native_{}", session_id),
        name: title,
        tool: "qwen".to_string(),
        cwd,
        session_token: Some(session_id),
        saved_at: updated_at,
        file_path: Some(file_path.to_string_lossy().into_owned()),
        turn_count: Some(turn_count),
    })
}

#[tauri::command]
fn read_native_session(file_path: String) -> Result<String, String> {
    let path = std::path::Path::new(&file_path);

    // Only allow .jsonl / .json files
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext != "jsonl" && ext != "json" {
        return Err("Only .jsonl and .json files are allowed".to_string());
    }

    // Canonicalize to resolve any `..` or symlink traversal
    let canonical_raw = path.canonicalize().map_err(|e| format!("Invalid path: {e}"))?;
    // On Windows, canonicalize() prepends \\?\ (UNC extended-length prefix).
    // Strip it so that starts_with() comparisons against plain home-dir paths work.
    #[cfg(windows)]
    let canonical = {
        let s = canonical_raw.to_string_lossy();
        if s.starts_with(r"\\?\") {
            std::path::PathBuf::from(s[4..].to_string())
        } else {
            canonical_raw
        }
    };
    #[cfg(not(windows))]
    let canonical = canonical_raw;

    // Must reside under a known agent data directory.
    //
    // Built-in defaults cover the standard install locations; any
    // additional paths the user configured via tool_config.history_path
    // are also allowed (otherwise the WSL-redirected scanner would find
    // sessions but reading them back would 403). Path canonicalization
    // already resolved symlinks, so this is a pure prefix check.
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    // Hermes Agent's data root is platform-dependent (`%LOCALAPPDATA%\hermes`
    // on Windows, `~/.hermes` elsewhere, or `$HERMES_HOME` if set). See
    // tools/hermes.rs::hermes_home. We also push the legacy `~/.hermes` when
    // it's distinct from the resolved root so a user who exports HERMES_HOME
    // mid-life can still read previously-collected sessions still sitting at
    // the dotdir path.
    let hermes_root = crate::tools::hermes::hermes_home();
    let hermes_legacy = home.join(".hermes");
    let mut allowed: Vec<std::path::PathBuf> = vec![
        home.join(".claude"),
        hermes_root.clone(),
        home.join(".codex").join("sessions"),
        home.join(".qwen").join("projects"),
        home.join(".local").join("share").join("opencode"),
        home.join(".openclaw").join("agents"),
        // Antigravity CLI lives under `.gemini/antigravity-cli/` (shares
        // namespace with retiring Gemini CLI). `~/.antigravitycli/` is
        // an unrelated stale placeholder some installers leave behind.
        home.join(".gemini").join("antigravity-cli"),
        // Antigravity / legacy Gemini session dir — both CLIs write
        // session JSONL under `~/.gemini/tmp/<project>/chats/`. Sessions
        // surface in the history list tagged as Antigravity; ChatReader
        // walks file_path through this gate to load them.
        home.join(".gemini").join("tmp"),
    ];
    if hermes_legacy != hermes_root {
        allowed.push(hermes_legacy);
    }
    for tool in ["claude", "hermes", "codex", "antigravity", "qwen", "opencode", "openclaw"] {
        let cfg = crate::tool_config::get(tool).history_path;
        if !cfg.is_empty() {
            allowed.push(crate::tool_config::expand_path(&cfg));
        }
    }
    if !allowed.iter().any(|prefix| canonical.starts_with(prefix)) {
        return Err("Access denied: path is outside allowed agent data directories".to_string());
    }

    std::fs::read_to_string(&canonical).map_err(|e| e.to_string())
}

// ─── OpenCode Session Reader ─────────────────────────────────────────────────
//
// OpenCode stores chat history in two layouts depending on version:
//   • SQLite (current):  `~/.local/share/opencode/opencode.db`
//                         tables `message` (role + metadata) + `part`
//                         (text/tool blocks), joined by message_id.
//   • JSON  (legacy):    `~/.local/share/opencode/storage/message/<sid>/*.json`
//                         one file per message, content blocks inline.
//
// Both are normalized to the same JSONL shape that ChatReader.tsx already
// understands (Claude Code shape with `{message:{role, content[]}}`):
//
//   {"message": {"role": "user", "content": [{"type":"text","text":"..."}]}}
//   {"message": {"role": "assistant", "content": [{"type":"text","text":"..."}]}}
//
// One line per message. Tool calls / patches / snapshots are dropped — the
// preview only cares about the text the user/assistant said. If the session
// has zero text turns, returns an empty string and the frontend renders the
// "no readable conversation records" empty state.

fn read_opencode_sqlite_session(
    db_path: &std::path::Path,
    session_id: &str,
) -> Result<String, String> {
    use rusqlite::Connection;
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open opencode.db: {e}"))?;

    // 1. Pull all messages for the session (ordered by creation time).
    let mut msg_stmt = conn
        .prepare(
            "SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created ASC",
        )
        .map_err(|e| format!("prepare message query: {e}"))?;
    let msg_rows = msg_stmt
        .query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("query messages: {e}"))?;

    // (message_id, role)
    let mut messages: Vec<(String, String)> = Vec::new();
    for row in msg_rows.flatten() {
        let (id, data) = row;
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(role) = parsed.get("role").and_then(|v| v.as_str()) {
                messages.push((id, role.to_string()));
            }
        }
    }
    if messages.is_empty() {
        return Ok(String::new());
    }

    // 2. Pull all parts for the session in one query, then bucket by message_id.
    let mut part_stmt = conn
        .prepare(
            "SELECT message_id, data FROM part WHERE session_id = ?1 ORDER BY time_created ASC",
        )
        .map_err(|e| format!("prepare part query: {e}"))?;
    let part_rows = part_stmt
        .query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("query parts: {e}"))?;

    use std::collections::HashMap;
    let mut parts_by_msg: HashMap<String, Vec<String>> = HashMap::new();
    for row in part_rows.flatten() {
        let (msg_id, data) = row;
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
            // Only TextPart contributes to the preview transcript.
            if parsed.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(text) = parsed.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        parts_by_msg.entry(msg_id).or_default().push(text.to_string());
                    }
                }
            }
        }
    }

    // 3. Emit one JSONL row per message, joining its text parts.
    let mut out = String::new();
    for (msg_id, role) in messages {
        let text_blocks: Vec<serde_json::Value> = parts_by_msg
            .get(&msg_id)
            .map(|v| {
                v.iter()
                    .map(|t| {
                        serde_json::json!({ "type": "text", "text": t })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if text_blocks.is_empty() {
            continue;
        }
        let line = serde_json::json!({
            "message": { "role": role, "content": text_blocks }
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Ok(out)
}

fn read_opencode_json_dir(message_dir: &std::path::Path) -> Result<String, String> {
    // Legacy layout: storage/message/<sid>/<msg-id>.json. Each file is the
    // full message info JSON (including role + content blocks). Sort by file
    // name so chronological order of message creation is preserved (OpenCode
    // file names are time-prefixed IDs).
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(message_dir)
        .map_err(|e| format!("read message dir: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();

    let mut out = String::new();
    for path in files {
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let role = match parsed.get("role").and_then(|v| v.as_str()) {
            Some(r) => r.to_string(),
            None => continue,
        };
        let mut text_blocks: Vec<serde_json::Value> = Vec::new();
        if let Some(arr) = parsed.get("content").and_then(|v| v.as_array()) {
            for block in arr {
                if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            text_blocks.push(serde_json::json!({ "type": "text", "text": text }));
                        }
                    }
                }
            }
        }
        if text_blocks.is_empty() {
            continue;
        }
        let line = serde_json::json!({
            "message": { "role": role, "content": text_blocks }
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Ok(out)
}

#[tauri::command]
fn read_opencode_session(session_id: String) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;

    // Honor user-configured OpenCode history path from
    // ~/.coffee-cli/tools.json (same source the listing pass uses
    // at line ~2129). Falls through to the platform default
    // ~/.local/share/opencode when the user hasn't customized it.
    let opencode_root = crate::tool_config::history_path_for(
        "opencode",
        home.join(".local").join("share").join("opencode"),
    );

    // Prefer current SQLite layout.
    let db_path = opencode_root.join("opencode.db");
    if db_path.is_file() {
        return read_opencode_sqlite_session(&db_path, &session_id);
    }

    // Fall back to legacy JSON layout. Message dir name == session_id.
    // Canonicalize and assert containment so a crafted session_id like
    // "../../../etc" can't escape the OpenCode storage root. SQLite
    // branch above is safe because session_id is only bound as a SQL
    // parameter, not joined into a path.
    let message_root = opencode_root.join("storage").join("message");
    let message_dir = message_root.join(&session_id);
    if message_dir.is_dir() {
        let canonical_dir = std::fs::canonicalize(&message_dir)
            .map_err(|e| format!("canonicalize session dir: {e}"))?;
        let canonical_root = std::fs::canonicalize(&message_root)
            .unwrap_or(message_root.clone());
        if !canonical_dir.starts_with(&canonical_root) {
            return Err("Access denied: session_id escapes OpenCode storage root".to_string());
        }
        return read_opencode_json_dir(&canonical_dir);
    }

    Err("OpenCode session storage not found".to_string())
}

/// Resolve MiMo Code's SQLite db (Xiaomi's OpenCode fork — identical Drizzle
/// schema). Primary path is `~/.local/share/mimocode/mimocode.db`, with the
/// older/atypical `~/.config/mimocode/mimocode.db` as fallback. None when
/// neither exists.
fn mimocode_db(home: &std::path::Path) -> Option<std::path::PathBuf> {
    // Primary root honors any tools.json history-path override (the field
    // ToolConfigModal surfaces for MiMo Code), defaulting to the descriptor's
    // declared `.local/share/mimocode` root — same contract as opencode_root /
    // hermes_state_db. `.config/mimocode` stays a secondary fallback for
    // atypical installs that the single override path can't express.
    let primary_root = crate::tools::find("mimocode")
        .and_then(|tool| {
            tool.history_shape
                .as_ref()
                .map(|shape| crate::tool_config::history_path_for(tool.id, shape.join_under(home)))
        })
        .unwrap_or_else(|| home.join(".local").join("share").join("mimocode"));
    let candidates = [
        primary_root.join("mimocode.db"),
        home.join(".config").join("mimocode").join("mimocode.db"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Read one MiMo Code session transcript. Same schema as OpenCode, so this
/// just points `read_opencode_sqlite_session` at `mimocode.db`.
#[tauri::command]
fn read_mimocode_session(session_token: String) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let db = mimocode_db(&home).ok_or("MiMo Code session storage not found")?;
    read_opencode_sqlite_session(&db, &session_token)
}

fn collect_jsonl_paths_with_mtime(
    dir: std::path::PathBuf,
    depth: u8,
    tool: &'static str,
    out: &mut Vec<(std::time::SystemTime, std::path::PathBuf, &'static str)>,
) {
    if depth == 0 || !dir.is_dir() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    let mtime = entry.metadata().ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    out.push((mtime, path, tool));
                }
            } else if path.is_dir() {
                collect_jsonl_paths_with_mtime(path, depth - 1, tool, out);
            }
        }
    }
}

fn parse_hermes_json(file_path: &std::path::Path) -> Option<SavedSession> {
    let content = std::fs::read_to_string(file_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Hermes session naming: legacy files were `session_<id>.json`, the
    // `.jsonl` rewrite drops the prefix. If the JSON carries a `session_id`
    // field, use it as-is (already bare). Otherwise fall back to the file
    // stem and strip the legacy `session_` prefix — Hermes's `--resume`
    // wants the bare `<YYYYMMDD>_<HHMMSS>_<hex>`, never the prefixed form.
    let session_id = value.get("session_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
            stem.strip_prefix("session_").unwrap_or(stem).to_string()
        });

    let mut title = String::new();
    let mut total_messages = 0u32;

    if let Some(messages) = value.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role == "user" || role == "assistant" {
                total_messages += 1;
            }
            if role == "user" && title.is_empty() {
                if let Some(content_str) = msg.get("content").and_then(|v| v.as_str()) {
                    let s = content_str.trim();
                    // Skip internal/system messages
                    if !s.is_empty() && !s.starts_with("[Note:") && !s.starts_with("[CONTEXT") {
                        let safe = s.replace('\n', " ");
                        let mut chars = safe.chars();
                        let t: String = chars.by_ref().take(40).collect();
                        title = if chars.next().is_some() { format!("{}...", t) } else { t };
                    }
                }
            }
        }
    }

    if title.is_empty() {
        title = "Hermes Agent Session".to_string();
    }

    let turn_count = if total_messages > 0 { std::cmp::max(1, (total_messages + 1) / 2) } else { 0 };

    let mut saved_at = String::new();
    if let Ok(meta) = std::fs::metadata(file_path) {
        if let Ok(mod_time) = meta.modified() {
            if let Ok(dur) = mod_time.duration_since(std::time::SystemTime::UNIX_EPOCH) {
                saved_at = dur.as_millis().to_string();
            }
        }
    }

    Some(SavedSession {
        id: format!("hermes_native_{}", session_id),
        name: title,
        tool: "hermes".to_string(),
        cwd: String::new(),
        session_token: Some(session_id),
        saved_at,
        file_path: Some(file_path.to_string_lossy().into_owned()),
        turn_count: Some(turn_count),
    })
}

fn parse_opencode_session(file_path: &std::path::Path, message_dir: &std::path::Path) -> Option<SavedSession> {
    let content = std::fs::read_to_string(file_path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;

    let id = value.get("id").and_then(|v| v.as_str())?.to_string();
    let title = value.get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("OpenCode Session")
        .to_string();
    let cwd = value.get("directory").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let saved_at = value.get("time")
        .and_then(|t| t.get("updated"))
        .and_then(|v| v.as_u64())
        .map(|ms| ms.to_string())
        .unwrap_or_default();

    // Count message files to estimate turn count
    let msg_dir = message_dir.join(&id);
    let msg_count = if msg_dir.is_dir() {
        std::fs::read_dir(&msg_dir)
            .map(|entries| entries.flatten().filter(|e| e.path().is_file()).count() as u32)
            .unwrap_or(0)
    } else {
        0
    };
    let turn_count = std::cmp::max(1, msg_count / 2);

    Some(SavedSession {
        id: format!("opencode_native_{}", id),
        name: title,
        tool: "opencode".to_string(),
        cwd,
        session_token: Some(id),
        saved_at,
        file_path: Some(file_path.to_string_lossy().into_owned()),
        turn_count: Some(turn_count),
    })
}

fn find_opencode_sessions(base_dir: std::path::PathBuf, result: &mut Vec<SavedSession>) {
    // Prefer SQLite DB (current OpenCode format) over legacy JSON files
    let db_path = base_dir.join("opencode.db");
    if db_path.is_file() {
        find_drizzle_sessions_sqlite(&db_path, "opencode", "OpenCode Session", result);
        return;
    }

    // Fallback: legacy JSON layout — storage/session/<project-id>/ses_*.json
    let session_dir = base_dir.join("storage").join("session");
    let message_dir = base_dir.join("storage").join("message");
    if !session_dir.is_dir() { return; }

    if let Ok(projects) = std::fs::read_dir(&session_dir) {
        for project_entry in projects.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() { continue; }
            if let Ok(sessions) = std::fs::read_dir(&project_path) {
                for session_entry in sessions.flatten() {
                    let path = session_entry.path();
                    if !path.is_file() { continue; }
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.starts_with("ses_") && name.ends_with(".json") {
                        if let Some(session) = parse_opencode_session(&path, &message_dir) {
                            result.push(session);
                        }
                    }
                }
            }
        }
    }
}

/// Read sessions from a Drizzle-schema SQLite db (`session` + `message`
/// tables). Shared by OpenCode and its forks — MiMo Code uses the identical
/// schema, so it passes its own `tool_id` / `default_title` and db path.
fn find_drizzle_sessions_sqlite(
    db_path: &std::path::Path,
    tool_id: &str,
    default_title: &str,
    result: &mut Vec<SavedSession>,
) {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Query sessions with message count, skip archived ones
    let query = "SELECT s.id, s.title, s.directory, s.time_updated, \
                 COUNT(m.id) as msg_count \
                 FROM session s \
                 LEFT JOIN message m ON m.session_id = s.id \
                 WHERE s.time_archived IS NULL \
                 GROUP BY s.id \
                 ORDER BY s.time_updated DESC \
                 LIMIT 30";

    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(_) => return,
    };

    let sessions_iter = stmt.query_map([], |row| {
        let id: String = row.get(0)?;
        let title: String = row.get::<_, Option<String>>(1)
            .unwrap_or(None)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_title.to_string());
        let directory: String = row.get::<_, Option<String>>(2)
            .unwrap_or(None)
            .unwrap_or_default();
        let time_updated: i64 = row.get(3).unwrap_or(0);
        let msg_count: i64 = row.get(4).unwrap_or(0);
        let turn_count = std::cmp::max(1, msg_count / 2) as u32;

        Ok(SavedSession {
            id: format!("{}_native_{}", tool_id, id),
            name: title,
            tool: tool_id.to_string(),
            cwd: directory,
            session_token: Some(id),
            saved_at: time_updated.to_string(),
            // Surface the shared SQLite DB path so the ChatReader copy-path
            // button has a target for OpenCode sessions too. Granularity
            // mismatch is OpenCode's own design choice — they bundle every
            // session into ONE opencode.db (vs Claude/Codex/Qwen/Hermes
            // jsonl-per-session) — so we expose the path that exists rather
            // than hide the button. Users who paste it into a file manager
            // land on the actual artifact that holds this conversation,
            // even if it also holds the others. Doesn't affect the read
            // path (ChatReader gates on tool==opencode + session_token,
            // not on file_path, so readOpencodeSession still owns parsing).
            file_path: Some(db_path.to_string_lossy().into_owned()),
            turn_count: Some(turn_count),
        })
    });

    if let Ok(iter) = sessions_iter {
        for session in iter.flatten() {
            result.push(session);
        }
    }
}

fn collect_hermes_paths_with_mtime(
    dir: std::path::PathBuf,
    out: &mut Vec<(std::time::SystemTime, std::path::PathBuf, &'static str)>,
) {
    if !dir.is_dir() {
        return;
    }
    // Newer Hermes Agent keeps the authoritative session store in
    // `<sessions>/state.db` (SQLite); the per-session `session_*.json` files
    // are a legacy/export layout that may be absent. When the db is present
    // AND actually yields sessions, `find_hermes_sessions_sqlite` is the
    // source of truth — skip the JSON candidates so the two paths can't
    // double-count. But if the db is empty / locked / corrupt / wrong-schema,
    // keep scanning JSON: suppressing it unconditionally would silently empty
    // the History board for a user whose legacy sessions are still on disk.
    let state_db = dir.join("state.db");
    if state_db.is_file() && hermes_db_has_sessions(&state_db) {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Only session_*.json files — skip request_dump_* and state.db
            if name.starts_with("session_") && name.ends_with(".json") {
                let mtime = entry.metadata().ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                out.push((mtime, path, "hermes"));
            }
        }
    }
}

/// Resolve `<HERMES_HOME>/sessions/state.db` (honouring any `tools.json`
/// history-path override), or None if Hermes Agent isn't in the registry.
/// Mirrors `opencode_root` so the history + heatmap passes resolve the
/// identical db path.
fn hermes_state_db(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let tool = crate::tools::find("hermes")?;
    let shape = tool.history_shape.as_ref()?;
    let dir = crate::tool_config::history_path_for(tool.id, shape.join_under(home));
    Some(dir.join("state.db"))
}

/// Cheap probe: does `state.db` open and hold at least one non-archived
/// session row? Drives the decision to suppress the legacy `session_*.json`
/// scan — if the db is missing, empty, locked, corrupt, or has an unexpected
/// schema this returns false and the JSON path stays as a fallback, so Hermes
/// history never silently empties out from under a usable legacy store.
fn hermes_db_has_sessions(db_path: &std::path::Path) -> bool {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    conn.query_row("SELECT 1 FROM sessions WHERE archived = 0 LIMIT 1", [], |_| Ok(()))
        .is_ok()
}

/// Normalize a Hermes `started_at` to epoch SECONDS. Hermes is documented to
/// store float seconds, but the unit is unverified across versions; treat any
/// value past ~year-5138-in-seconds (i.e. a millisecond timestamp) as ms so the
/// history sort and the seconds-based heatmap stay correct either way.
fn hermes_started_at_secs(raw: f64) -> f64 {
    if raw > 1e11 { raw / 1000.0 } else { raw }
}

/// Read Hermes Agent sessions from the SQLite `state.db`. Newer Hermes
/// stores everything here (sessions + messages + FTS5 search); the
/// `session_*.json` files our legacy path reads may be absent.
///
/// Schema (sessions table): `id`, `title`, `cwd`, `started_at` (epoch
/// SECONDS, REAL), `message_count`, `archived`. Best-effort: any error
/// (missing table, renamed column, locked db) yields zero rows and the
/// caller falls back to the JSON scan — no regression for older Hermes.
///
/// `file_path` is intentionally None: these sessions live in the shared db,
/// not a per-session file, so ChatReader routes them through
/// `read_hermes_session` instead of `read_native_session`.
fn find_hermes_sessions_sqlite(db_path: &std::path::Path, result: &mut Vec<SavedSession>) {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };
    let query = "SELECT id, title, cwd, started_at, message_count \
                 FROM sessions \
                 WHERE archived = 0 \
                 ORDER BY started_at DESC \
                 LIMIT 200";
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(_) => return,
    };
    let iter = stmt.query_map([], |row| {
        // id is the resume token (TEXT). Read tolerantly: if a Hermes version
        // stores it as INTEGER rowid, fall back to stringifying it rather than
        // erroring the row (which `?` would, silently dropping EVERY session).
        let id: String = row
            .get::<_, String>(0)
            .or_else(|_| row.get::<_, i64>(0).map(|n| n.to_string()))?;
        let title: Option<String> = row.get::<_, Option<String>>(1).unwrap_or(None);
        let cwd: String = row.get::<_, Option<String>>(2).unwrap_or(None).unwrap_or_default();
        // started_at → epoch ms for the saved_at string the frontend sorts on.
        // hermes_started_at_secs normalizes a seconds-or-ms value to seconds.
        let started_at: f64 = row.get(3).unwrap_or(0.0);
        let msg_count: i64 = row.get(4).unwrap_or(0);
        // Title fallback: explicit title → cwd basename → placeholder.
        let name = title
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                std::path::Path::new(&cwd)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Hermes Agent Session".to_string())
            });
        let turn_count = if msg_count > 0 { std::cmp::max(1, (msg_count / 2) as u32) } else { 0 };
        Ok(SavedSession {
            id: format!("hermes_native_{}", id),
            name,
            tool: "hermes".to_string(),
            cwd,
            session_token: Some(id),
            saved_at: ((hermes_started_at_secs(started_at) * 1000.0) as i64).to_string(),
            file_path: None,
            turn_count: Some(turn_count),
        })
    });
    if let Ok(iter) = iter {
        for session in iter.flatten() {
            result.push(session);
        }
    }
}

/// Heatmap second pass for Hermes `state.db` — one entry per session
/// (started_at seconds + message_count). Mirrors
/// `collect_opencode_heatmap_entries`. Best-effort.
fn collect_hermes_heatmap_entries(
    db_path: &std::path::Path,
    cutoff_secs: i64,
    out: &mut Vec<HeatmapEntry>,
) {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };
    let query = "SELECT started_at, message_count FROM sessions \
                 WHERE archived = 0 AND started_at >= ?1";
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(_) => return,
    };
    let rows = match stmt.query_map([cutoff_secs as f64], |row| {
        let started_at: f64 = row.get(0)?;
        let count: i64 = row.get(1).unwrap_or(0);
        // Normalize seconds-or-ms → seconds (HeatmapEntry.ts is seconds; the
        // frontend does `new Date(ts*1000)`). The WHERE filter above uses the
        // raw value, so an ms store just over-fetches slightly — still correct.
        Ok((hermes_started_at_secs(started_at) as i64, count))
    }) {
        Ok(r) => r,
        Err(_) => return,
    };
    for row in rows.flatten() {
        let (ts, count) = row;
        if count > 0 {
            out.push(HeatmapEntry { ts, count: count.clamp(0, u32::MAX as i64) as u32 });
        }
    }
}

/// Decode one Hermes `messages.content` cell. Plain text is stored raw;
/// structured / multimodal content is `"\x00json:"` + JSON (Hermes'
/// `_encode_content`). Strip the marker and pull the text back out.
fn hermes_decode_content(raw: &str) -> String {
    let Some(json_part) = raw.strip_prefix("\u{0}json:") else {
        return raw.to_string();
    };
    match serde_json::from_str::<serde_json::Value>(json_part) {
        Ok(v) => hermes_extract_text(&v),
        Err(_) => raw.to_string(),
    }
}

/// Best-effort text extraction from a decoded Hermes content value — a bare
/// string, `{text: ...}`, or an array of `{type, text}` blocks. Anything
/// else collapses to compact JSON so the turn still shows something.
fn hermes_extract_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|it| it.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(_) => v
            .get("text")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| v.to_string()),
        _ => v.to_string(),
    }
}

/// Read one Hermes session's transcript from `state.db` for the ChatReader
/// preview. Emits the same newline-delimited `{"message":{role,content}}`
/// shape `read_native_session` returns for JSONL tools, so the existing
/// frontend parser handles it unchanged.
///
/// Schema (messages table): `session_id`, `role`, `content`, ordered by
/// rowid (insertion order). Best-effort.
#[tauri::command]
fn read_hermes_session(session_token: String) -> Result<String, String> {
    let home = dirs::home_dir().ok_or_else(|| "no home dir".to_string())?;
    let db = hermes_state_db(&home).ok_or_else(|| "hermes not in registry".to_string())?;
    let conn = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open hermes state.db: {e}"))?;
    let mut stmt = conn
        .prepare("SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY rowid")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([&session_token], |row| {
            let role: String = row.get(0)?;
            let content: String =
                row.get::<_, Option<String>>(1).unwrap_or(None).unwrap_or_default();
            Ok((role, content))
        })
        .map_err(|e| e.to_string())?;
    let mut out = String::new();
    for r in rows.flatten() {
        let (role, content) = r;
        let line = serde_json::json!({
            "message": { "role": role, "content": hermes_decode_content(&content) }
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Ok(out)
}

#[tauri::command]
async fn get_native_history() -> Result<Vec<SavedSession>, String> {
    // Async command + spawn_blocking so the file I/O runs on a dedicated
    // blocking thread pool and never blocks the Tauri command dispatcher.
    // Other IPC calls (resize, theme switches, etc.) stay responsive while
    // history is being scanned on app startup.
    tauri::async_runtime::spawn_blocking(load_native_history_blocking)
        .await
        .map_err(|e| format!("History task join failed: {e}"))?
}

fn load_native_history_blocking() -> Result<Vec<SavedSession>, String> {
    // Cap history to the N most recent entries. Keeps UI responsive when users
    // have hundreds of sessions — parsing a full jsonl/json file is expensive,
    // so we pre-select candidates by file mtime and only parse the top N.
    // 200 covers months of daily use; the frontend renders 30 at a time and
    // pages in the rest progressively as the user scrolls (HistoryBoard).
    const HISTORY_LIMIT: usize = 200;

    let mut file_candidates: Vec<(std::time::SystemTime, std::path::PathBuf, &'static str)> = Vec::new();
    let mut result: Vec<SavedSession> = Vec::new();

    let home = dirs::home_dir();
    if let Some(home) = home.as_ref() {
        collect_registry_history_candidates(home, &mut file_candidates);
    }

    // Sort candidates by mtime desc and parse only the newest HISTORY_LIMIT.
    file_candidates.sort_by(|a, b| b.0.cmp(&a.0));
    file_candidates.truncate(HISTORY_LIMIT);

    // Lazy-load the Antigravity / Gemini project-hash → cwd map only
    // if we actually have antigravity candidates — file I/O isn't
    // free and not every user has agy on this machine.
    let antigravity_project_map = if file_candidates.iter().any(|(_, _, t)| *t == "antigravity") {
        load_gemini_project_map()
    } else {
        std::collections::HashMap::new()
    };

    for (_, path, tool) in &file_candidates {
        let parsed = match *tool {
            "hermes"      => parse_hermes_json(path),
            "codex"       => parse_codex_session_jsonl(path),
            "qwen"        => parse_qwen_session_jsonl(path),
            "antigravity" => parse_gemini_session_jsonl(path, &antigravity_project_map),
            other         => parse_agent_jsonl(path, other),
        };
        if let Some(session) = parsed {
            result.push(session);
        }
    }

    // OpenCode second pass — SQLite is cheap (query already caps rows).
    // Bypasses the mtime pipeline: find_opencode_sessions pushes finished
    // SavedSession objects directly.
    if let Some(home) = home.as_ref() {
        if let Some(opencode_dir) = opencode_root(home) {
            find_opencode_sessions(opencode_dir, &mut result);
        }
    }

    // Hermes second pass — read state.db when present (newer Hermes). The
    // JSON candidates were already skipped in collect_hermes_paths_with_mtime
    // when the db exists, so this is the sole Hermes source then; with no db,
    // the JSON path already populated `result` and this is a no-op.
    if let Some(home) = home.as_ref() {
        if let Some(db) = hermes_state_db(home) {
            if db.is_file() {
                find_hermes_sessions_sqlite(&db, &mut result);
            }
        }
    }

    // MiMo Code second pass — Xiaomi's OpenCode fork, identical Drizzle schema,
    // so it reuses find_drizzle_sessions_sqlite with its own db path + labels.
    if let Some(home) = home.as_ref() {
        if let Some(db) = mimocode_db(home) {
            find_drizzle_sessions_sqlite(&db, "mimocode", "MiMo Code Session", &mut result);
        }
    }

    result.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    result.truncate(HISTORY_LIMIT);
    Ok(result)
}

/// Walk every registry tool with a JSONL/Hermes history shape and
/// push (mtime, path, tool_id) tuples into `out`. OpenCodeMixed is
/// skipped — its SQLite scanner runs as a second pass and emits
/// finished SavedSession objects, not file candidates.
///
/// Shared between `load_native_history_blocking` (History board)
/// and `load_message_heatmap_blocking` (contribution heatmap) so
/// the two surfaces can't drift.
fn collect_registry_history_candidates(
    home: &std::path::Path,
    out: &mut Vec<(std::time::SystemTime, std::path::PathBuf, &'static str)>,
) {
    for tool in crate::tools::TOOLS {
        let Some(shape) = tool.history_shape.as_ref() else { continue };
        let scan_dir =
            crate::tool_config::history_path_for(tool.id, shape.join_under(home));
        match shape {
            crate::tools::HistoryShape::HermesFlatJson => {
                collect_hermes_paths_with_mtime(scan_dir, out);
            }
            crate::tools::HistoryShape::OpenCodeMixed { .. } => {}
            _ => {
                if let Some(depth) = shape.jsonl_depth() {
                    collect_jsonl_paths_with_mtime(scan_dir, depth, tool.id, out);
                }
            }
        }
    }
}

/// Resolve OpenCode's session-store root (under the user's home,
/// or wherever `~/.coffee-cli/tools.json` redirects). `None` if
/// OpenCode isn't in the registry — should never happen in
/// practice, but keeps the call sites total.
fn opencode_root(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let tool = crate::tools::find("opencode")?;
    let shape = tool.history_shape.as_ref()?;
    Some(crate::tool_config::history_path_for(tool.id, shape.join_under(home)))
}

// Contribution-heatmap entry: one tuple per session file (mtime + message
// count). The frontend buckets these into local-day boxes — doing the
// bucketing here would require a TZ database (chrono/time) just to honour
// the user's local midnight, which isn't worth the dependency.
#[derive(serde::Serialize)]
struct HeatmapEntry {
    ts: i64,    // file mtime, seconds since UNIX_EPOCH
    count: u32, // approximate message count for the session
}

/// Persisted line-count cache for the heatmap scanner. One JSON file at
/// `~/.coffee-cli/cache/heatmap-counts.json`. Best-effort across the board:
/// any I/O / parse error returns an empty map and the scanner just recounts
/// from disk. The mtime stored is seconds-since-epoch (matches the i64 `ts`
/// field on HeatmapEntry) so a single integer comparison decides cache hit.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct CachedCount {
    mtime: i64,
    count: u32,
}

fn count_cache_path() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".coffee-cli").join("cache").join("heatmap-counts.json"))
}

fn read_count_cache() -> std::collections::HashMap<String, CachedCount> {
    let Some(path) = count_cache_path() else { return std::collections::HashMap::new(); };
    let Ok(content) = std::fs::read_to_string(&path) else { return std::collections::HashMap::new(); };
    serde_json::from_str(&content).unwrap_or_default()
}

fn write_count_cache(map: &std::collections::HashMap<String, CachedCount>) {
    let Some(path) = count_cache_path() else { return; };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(map) {
        let _ = std::fs::write(&path, json);
    }
}

#[tauri::command]
async fn get_message_heatmap() -> Result<Vec<HeatmapEntry>, String> {
    tauri::async_runtime::spawn_blocking(load_message_heatmap_blocking)
        .await
        .map_err(|e| format!("Heatmap task join failed: {e}"))?
}

fn load_message_heatmap_blocking() -> Result<Vec<HeatmapEntry>, String> {
    // Frontend renders a 26-week (≈ 6-month) grid. ~210 days back so
    // the leftmost column is always populated even mid-week.
    const LOOKBACK_SECS: u64 = 210 * 86400;
    let now = std::time::SystemTime::now();
    let cutoff = now.checked_sub(std::time::Duration::from_secs(LOOKBACK_SECS))
        .unwrap_or(std::time::UNIX_EPOCH);

    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf, &'static str)> = Vec::new();

    let home = dirs::home_dir();
    if let Some(home) = home.as_ref() {
        collect_registry_history_candidates(home, &mut candidates);
    }

    // Per-file count cache. Heatmap re-scans every app launch and counts
    // every jsonl line in every history file — for users with hundreds of
    // past sessions that's the bulk of the cold-start I/O. Past sessions are
    // immutable: once a session file's mtime is stable, its line count is
    // stable forever. So we cache `path -> (mtime, count)` to disk, and on
    // subsequent runs skip the open+read for any file whose mtime is
    // unchanged from the cached entry. Cache corruption / partial writes
    // are safe — `read_count_cache` returns empty on any error and we just
    // recount once.
    let mut count_cache = read_count_cache();
    let mut cache_dirty = false;
    let mut keep_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    let mut out: Vec<HeatmapEntry> = Vec::with_capacity(candidates.len());
    for (mtime, path, tool) in &candidates {
        if *mtime < cutoff { continue; }
        let ts = mtime
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let path_key = path.to_string_lossy().into_owned();
        keep_paths.insert(path_key.clone());

        // Cache hit only when mtime exactly matches — any append to the jsonl
        // bumps mtime and forces a recount.
        let count = if let Some(entry) = count_cache.get(&path_key) {
            if entry.mtime == ts {
                entry.count
            } else {
                let c = if *tool == "hermes" {
                    count_hermes_messages(path)
                } else {
                    count_jsonl_message_lines(path)
                };
                count_cache.insert(path_key.clone(), CachedCount { mtime: ts, count: c });
                cache_dirty = true;
                c
            }
        } else {
            let c = if *tool == "hermes" {
                count_hermes_messages(path)
            } else {
                count_jsonl_message_lines(path)
            };
            count_cache.insert(path_key.clone(), CachedCount { mtime: ts, count: c });
            cache_dirty = true;
            c
        };
        if count > 0 {
            out.push(HeatmapEntry { ts, count });
        }
    }

    // Prune stale entries (files that disappeared from disk). Non-jsonl tools
    // like opencode use a separate cache layer below, so don't get caught
    // here; the heuristic is "if we didn't see the path this scan, drop it".
    let before = count_cache.len();
    count_cache.retain(|k, _| keep_paths.contains(k));
    if count_cache.len() != before { cache_dirty = true; }
    if cache_dirty { write_count_cache(&count_cache); }

    // OpenCode SQLite second pass — one GROUP BY query gets the same
    // (timestamp, message_count) tuples the heatmap consumes, pre-
    // filtered by the same 210-day cutoff so we don't read rows the
    // frontend would discard anyway.
    if let Some(home) = home.as_ref() {
        if let Some(db_path) = opencode_root(home).map(|r| r.join("opencode.db")) {
            if db_path.is_file() {
                let cutoff_secs = cutoff
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                collect_opencode_heatmap_entries(&db_path, cutoff_secs, &mut out);
            }
        }
    }

    // Hermes heatmap second pass — state.db when present, same shape as
    // OpenCode. The JSON candidate path was skipped above when the db exists,
    // so there's no double-counting.
    if let Some(home) = home.as_ref() {
        if let Some(db) = hermes_state_db(home) {
            if db.is_file() {
                let cutoff_secs = cutoff
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                collect_hermes_heatmap_entries(&db, cutoff_secs, &mut out);
            }
        }
    }

    // MiMo Code heatmap second pass — identical schema to OpenCode, so it
    // reuses collect_opencode_heatmap_entries with the mimocode.db path.
    if let Some(home) = home.as_ref() {
        if let Some(db) = mimocode_db(home) {
            let cutoff_secs = cutoff
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            collect_opencode_heatmap_entries(&db, cutoff_secs, &mut out);
        }
    }

    Ok(out)
}

/// One-shot SQLite scan of opencode's session table for heatmap entries.
/// Mirrors `find_opencode_sessions_sqlite` (same WHERE/GROUP BY shape) so
/// any schema drift in opencode.db hits both queries together. Best-effort:
/// any error (locked DB, schema change, file missing) silently yields zero
/// rows — opencode just doesn't appear in the heatmap that session.
fn collect_opencode_heatmap_entries(
    db_path: &std::path::Path,
    cutoff_secs: i64,
    out: &mut Vec<HeatmapEntry>,
) {
    let conn = match rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return,
    };
    // opencode.db stores time_updated in MILLISECONDS since epoch (verified
    // via parse_opencode_session at L2006 which treats the JSON `time.updated`
    // as `u64 ms`). Heatmap entries downstream are seconds (frontend does
    // `new Date(ts * 1000)` in ContributionHeatmap.tsx). So compare and emit
    // in millis at the SQL boundary, then divide by 1000 once.
    let cutoff_ms: i64 = cutoff_secs.saturating_mul(1000);
    let query = "SELECT s.time_updated, COUNT(m.id) AS msg_count \
                 FROM session s \
                 LEFT JOIN message m ON m.session_id = s.id \
                 WHERE s.time_archived IS NULL AND s.time_updated >= ?1 \
                 GROUP BY s.id";
    let mut stmt = match conn.prepare(query) {
        Ok(s) => s,
        Err(_) => return,
    };
    let rows = match stmt.query_map([cutoff_ms], |row| {
        let ts_ms: i64 = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((ts_ms, count))
    }) {
        Ok(r) => r,
        Err(_) => return,
    };
    for row in rows.flatten() {
        let (ts_ms, count) = row;
        if count > 0 {
            out.push(HeatmapEntry { ts: ts_ms / 1000, count: count as u32 });
        }
    }
}

// Cheap line-count for JSONL session files. We treat every non-empty
// line as one "turn" — including system / tool-result rows. The heatmap
// is an activity proxy, not a strict user-message tally, so over-
// counting tool spam is fine (more chatter = darker square).
fn count_jsonl_message_lines(path: &std::path::Path) -> u32 {
    use std::io::{BufRead, BufReader, Read};
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return 0,
    };
    // Cap reading at ~32 MiB to keep one runaway session from stalling
    // the whole heatmap scan. 32 MiB of JSONL is ~50k+ lines — already
    // off the chart visually, so capping doesn't affect the bucket.
    const MAX_BYTES: u64 = 32 * 1024 * 1024;
    let mut br = BufReader::new(file.take(MAX_BYTES));
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut count = 0u32;
    while let Ok(n) = br.read_until(b'\n', &mut buf) {
        if n == 0 { break; }
        if buf.iter().any(|&b| !b.is_ascii_whitespace()) {
            count = count.saturating_add(1);
        }
        buf.clear();
    }
    count
}

// Hermes stores one big JSON file per session, not JSONL — so line-
// counting wouldn't work. Approximate message count by counting
// "role" key occurrences. Cheaper than a full serde_json parse.
fn count_hermes_messages(path: &std::path::Path) -> u32 {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return 0,
    };
    let needle = b"\"role\"";
    let mut count = 0u32;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            count = count.saturating_add(1);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

#[tauri::command]
fn tier_terminal_resume(
    session_id: String,
    saved_session_id: String, // The UUID of the new terminal tab
    tool: String,
    session_token: String,
    cols: u16,
    rows: u16,
    cwd: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {

    let preset = terminal::find_preset(&tool)
        .ok_or_else(|| format!("Unknown tool: {}", tool))?;
    let resume_program = preset.resume_program
        .ok_or_else(|| format!("Tool '{}' does not support resume", tool))?;

    // Validate session_token against the tool's token_format.
    // Prevents flag injection: "uuid --dangerously-skip-permissions" would fail this check.
    if let Some(fmt) = preset.token_format {
        let re = regex::Regex::new(fmt)
            .map_err(|e| format!("Invalid token format pattern: {e}"))?;
        if !re.is_match(&session_token) {
            return Err(format!("Invalid session token format for tool '{}'", tool));
        }
    }

    // Build args without string interpolation: token is always a separate element,
    // never concatenated into a command string that gets split by whitespace.
    let program = resume_program.to_string();
    let mut args: Vec<String> = preset.resume_args_before.iter().map(|s| s.to_string()).collect();
    args.push(session_token.clone());
    args.extend(preset.resume_args_after.iter().map(|s| s.to_string()));

    let actual_cwd = cwd.clone();
    let emit_session_id = saved_session_id.clone();

    terminal::spawn(
        app.clone(),
        saved_session_id,
        state.terminal_session.clone(),
        program,
        args,
        Some(cwd),
        "en".to_string(),
        cols,
        rows,
        Some(tool),
        None, // theme_mode: resume sessions use default detection
        None, // locale: resume sessions use env detection
        Vec::new(), // extra_env: single-terminal resume — no per-pane MCP wiring
    ).map_err(|e| format!("Failed to resume: {}", e))?;

    // Emit CWD so the left panel maps the resumed session's directory
    if !actual_cwd.is_empty() {
        #[derive(serde::Serialize, Clone)]
        struct CwdPayload { id: String, cwd: String }
        let _ = app.emit("tier-terminal-cwd", CwdPayload {
            id: emit_session_id,
            cwd: actual_cwd,
        });
    }

    // Remove from saved sessions file
    let path = sessions_file_path();
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut saved) = serde_json::from_str::<Vec<SavedSession>>(&content) {
                saved.retain(|s| s.id != session_id);
                let _ = std::fs::write(&path, serde_json::to_string_pretty(&saved).unwrap_or_default());
            }
        }
    }

    Ok(())
}



// ─── Task Board Persistence ──────────────────────────────────────────────────

fn tasks_file_path() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".coffee-cli");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("tasks.json")
}

#[tauri::command]
fn load_tasks() -> Result<String, String> {
    let path = tasks_file_path();
    if path.exists() {
        std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read tasks: {}", e))
    } else {
        Ok("[]".to_string())
    }
}

#[tauri::command]
fn save_tasks(data: String, app: tauri::AppHandle) -> Result<(), String> {
    // Validate JSON before writing to disk — guards against corrupted broadcasts
    serde_json::from_str::<serde_json::Value>(&data)
        .map_err(|e| format!("Invalid task data (not valid JSON): {e}"))?;
    let path = tasks_file_path();
    std::fs::write(&path, &data)
        .map_err(|e| format!("Failed to save tasks: {}", e))?;
    // Notify all windows so other instances can reload
    let _ = app.emit("tasks-changed", &data);
    Ok(())
}

// ─── Credential Store (OS Keychain) ──────────────────────────────────────────

const KEYRING_SERVICE: &str = "coffee-cli";

/// Persist a remote password in the OS keychain (Windows Credential Manager /
/// macOS Keychain / Linux Secret Service). The key is `username@host`.
#[tauri::command]
fn save_password(host: String, username: String, password: String) -> Result<(), String> {
    let account = format!("{}@{}", username, host);
    keyring::Entry::new(KEYRING_SERVICE, &account)
        .map_err(|e| e.to_string())?
        .set_password(&password)
        .map_err(|e| e.to_string())
}

/// Load a previously saved password from the OS keychain.
/// Returns `None` if no entry exists (user hasn't saved one yet).
#[tauri::command]
fn load_password(host: String, username: String) -> Result<Option<String>, String> {
    let account = format!("{}@{}", username, host);
    match keyring::Entry::new(KEYRING_SERVICE, &account)
        .map_err(|e| e.to_string())?
        .get_password()
    {
        Ok(pw) => Ok(Some(pw)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Remove a saved password from the OS keychain (e.g. user clicked "forget").
#[tauri::command]
fn delete_password(host: String, username: String) -> Result<(), String> {
    let account = format!("{}@{}", username, host);
    match keyring::Entry::new(KEYRING_SERVICE, &account)
        .map_err(|e| e.to_string())?
        .delete_credential()
    {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()), // already gone, not an error
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn check_network_port(host: String, port: u16) -> Result<bool, String> {
    use std::time::Duration;
    use std::net::ToSocketAddrs;
    
    let target = format!("{}:{}", host, port);
    
    // Run blocking network check in a dedicated blocking task to avoid stalling the async runtime
    let reachable = tauri::async_runtime::spawn_blocking(move || {
        match target.to_socket_addrs() {
            Ok(mut addrs) => {
                if let Some(addr) = addrs.next() {
                    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3)).is_ok()
                } else {
                    false
                }
            },
            Err(_) => false
        }
    }).await.unwrap_or(false);

    Ok(reachable)
}

#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {e}"))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {e}"))?;
    }
    Ok(())
}

// ─── In-app self-update ────────────────────────────────────────────────────
//
// Downloads the latest installer from `coffeecli.com/download/<os>` — a CF
// Worker that proxies the matching GitHub Release asset (China-accessible,
// stable name, no per-version URL to construct). Streams the body so the
// frontend can paint a circular download-progress ring via the
// `self-update-progress` event, then launches the installer and exits so it
// can replace our running files. ureq is blocking + rustls, so the whole
// thing runs on a spawn_blocking thread; `app.emit` works from any thread.

#[derive(serde::Serialize, Clone)]
struct SelfUpdateProgress {
    status: String, // "speed_test" | "downloading" | "launching" | "error"
    percent: u32,
}

fn emit_self_update(app: &tauri::AppHandle, status: &str, percent: u32) {
    let _ = app.emit(
        "self-update-progress",
        SelfUpdateProgress { status: status.to_string(), percent },
    );
}

#[tauri::command]
async fn download_and_install_update(app: tauri::AppHandle) -> Result<(), String> {
    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let url = format!("https://coffeecli.com/download/{os}");
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || run_self_update(&app2, &url))
        .await
        .map_err(|e| format!("self-update task join failed: {e}"))?
}

fn run_self_update(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    use std::io::{Read, Write};

    emit_self_update(app, "speed_test", 0);

    let resp = match ureq::get(url).call() {
        Ok(r) => r,
        Err(e) => {
            emit_self_update(app, "error", 0);
            return Err(format!("download request failed: {e}"));
        }
    };
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let ext = if cfg!(target_os = "windows") {
        "exe"
    } else if cfg!(target_os = "macos") {
        "dmg"
    } else {
        "bin"
    };
    let out_path = std::env::temp_dir().join(format!("coffee-cli-update-setup.{ext}"));

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&out_path).map_err(|e| {
        emit_self_update(app, "error", 0);
        format!("create temp file: {e}")
    })?;

    let mut buf = [0u8; 65536];
    let mut downloaded: u64 = 0;
    let mut last_pct: u32 = 0;
    emit_self_update(app, "downloading", 0);
    loop {
        let n = reader.read(&mut buf).map_err(|e| {
            emit_self_update(app, "error", last_pct);
            format!("download read failed: {e}")
        })?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| {
            emit_self_update(app, "error", last_pct);
            format!("write temp file failed: {e}")
        })?;
        downloaded += n as u64;
        if total > 0 {
            // Cap at 99 during streaming; 100 is reserved for "fully written".
            let pct = (downloaded.saturating_mul(100) / total).min(99) as u32;
            if pct != last_pct {
                last_pct = pct;
                emit_self_update(app, "downloading", pct);
            }
        }
    }
    drop(file);
    emit_self_update(app, "downloading", 100);

    // Launch the installer, then exit so it can overwrite our running files.
    emit_self_update(app, "launching", 100);
    #[cfg(target_os = "windows")]
    let launch = std::process::Command::new(&out_path).spawn();
    #[cfg(target_os = "macos")]
    let launch = std::process::Command::new("open").arg(&out_path).spawn();
    #[cfg(target_os = "linux")]
    let launch = std::process::Command::new("xdg-open").arg(&out_path).spawn();
    if let Err(e) = launch {
        emit_self_update(app, "error", 100);
        return Err(format!("launch installer failed: {e}"));
    }

    // Let the wizard come up before we tear the app down.
    std::thread::sleep(std::time::Duration::from_millis(800));
    app.exit(0);
    Ok(())
}

// ─── Hyper-Agent: global anonymous MCP server for external orchestrators ──
//
// Started lazily when the user opens the Hyper-Agent tab. `self_pane_id=None`,
// so list_panes / send_to_pane bypass tab-scope filtering — exactly the
// "super admin can see and dispatch to every pane" semantics the product
// needs. Uses a port persisted across launches so OpenClaw / Hermes Agent
// configs stay stable (no config-file thrash → no gateway restart loops).
//
// Tauri commands here are all the frontend needs to know about.

// ─── Per-tool launch overrides (~/.coffee-cli/tools.json) ────────────────
//
// Lets users tell Coffee CLI things like "my claude is at
// `/opt/coffee/bin/claude`, not on PATH" or "always launch claude with
// --dangerously-skip-permissions". Replaces what the abandoned in-app
// installer was supposed to handle by auto-detection — defer to the
// user, who knows their machine better than we do.

#[tauri::command]
pub fn get_tool_config(tool: String) -> crate::tool_config::ToolConfigEntry {
    crate::tool_config::get(&tool)
}

#[tauri::command]
pub fn get_all_tool_configs() -> crate::tool_config::ToolConfig {
    crate::tool_config::load()
}

#[tauri::command]
pub fn set_tool_config(
    tool: String,
    entry: crate::tool_config::ToolConfigEntry,
) -> Result<(), String> {
    crate::tool_config::set(&tool, entry).map_err(|e| e.to_string())
}

pub fn start_ui() -> anyhow::Result<()> {
    // Create shared session BEFORE the builder so we can clone it for the exit handler
    let terminal_session = terminal::SharedSession::default();

    let builder = tauri::Builder::default();

    // Single-instance plugin MUST be the first plugin registered (per
    // Tauri docs) so its argv-forwarding hook runs before any other
    // plugin's init touches state. When a user double-launches Coffee CLI,
    // the second process sends its argv+cwd to this callback in the first
    // process and exits — the first process then refocuses the main window.
    // Side effect we want: only ever one WebView2 instance, which kills
    // the multi-process IME-jumps-to-(0,0) bug.
    //
    // Release-only: in debug builds we skip the lock so a dev `cargo tauri
    // dev` window can run side-by-side with an installed production build
    // (devs working on Coffee CLI inside Coffee CLI). Both builds otherwise
    // share the bundle identifier, and the lock would silently redirect the
    // dev launch to the production process and exit.
    #[cfg(not(debug_assertions))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        use tauri::Manager;
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }));

    builder
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState {
            terminal_session,
            hook_port: std::sync::atomic::AtomicU16::new(0),
            fs_watcher: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            pick_folder,
            window_minimize,
            window_maximize,
            window_close,
            show_main_window,
            tier_terminal_start,
            tier_terminal_input,
            tier_terminal_raw_write,
            tier_terminal_kill,
            tier_terminal_resize,
            set_background_mode,
            tier_terminal_resume,
            get_native_history,
            get_message_heatmap,
            read_native_session,
            read_opencode_session,
            read_hermes_session,
            read_mimocode_session,
            check_network_port,
            check_tools_installed,
            crate::tools::list_tools,
            install_hook_for_tool,
            start_fs_watcher,
            stop_fs_watcher,
            save_clipboard_image,
            list_directory,
            read_text_file,
            show_in_folder,
            fs_delete,
            fs_rename,
            fs_paste,
            load_tasks,
            save_tasks,
            save_password,
            load_password,
            delete_password,
            open_url,
            download_and_install_update,
            get_tool_config,
            get_all_tool_configs,
            set_tool_config,
            crate::skills::skills_ensure_dirs,
            crate::skills::skills_write_file,
            crate::skills::skills_list,
            crate::skills::skills_toggle,
            crate::skills::skills_delete,
            crate::marketplace::add_marketplace,
            crate::marketplace::list_marketplaces,
            crate::marketplace::set_marketplace_plugin_enabled,
            crate::marketplace::update_marketplace,
            crate::marketplace::delete_marketplace,
            crate::marketplace::open_marketplace_dir,
            crate::git::git_changes,
            crate::git::git_show_file,
            crate::git::git_init,
        ])
        .setup(|app| {
            // Install Claude/Qwen hook scripts + settings patches.
            // Runs once per launch; safe to call on a machine without either agent.
            crate::hook_installer::install_all();

            // Seed bundled skills (screenshot, vibeid) into
            // ~/.coffee-cli/skills-library/ so first-time users find
            // them in the Skills panel without having to open it
            // once to trigger seeding. Idempotent.
            if let Err(e) = crate::skills::skills_ensure_dirs() {
                log::warn!("[skills] seed at boot failed: {}", e);
            }

            // Start loopback TCP listener that receives events from the hook
            // script and forwards them to the frontend as `agent-status` events.
            match crate::hook_server::start(app.handle().clone()) {
                Ok(port) => {
                    app.state::<AppState>()
                        .hook_port
                        .store(port, std::sync::atomic::Ordering::SeqCst);
                }
                Err(e) => {
                    eprintln!("[hook-server] start failed: {}", e);
                }
            }

            // Per-pane MCP servers are spawned lazily inside
            // `tier_terminal_start` when each multi-agent pane boots
            // its CLI. Users who never open a multi-agent tab pay
            // zero MCP cost.

            // ── Bulletproof window-reveal fallback ──────────────────
            // The window is created with `visible: false` so the user
            // never sees the platform's chrome flash — main.tsx
            // invokes `show_main_window` after the first paint via
            // double-RAF and the window appears already-themed.
            //
            // BUT: if the WebView never paints (Gatekeeper rejection
            // on adhoc-signed macOS bundles, WebKit2GTK Wayland blank
            // window on Ubuntu 24.04, or any JS error before
            // ReactDOM mount), the `invoke` never fires, the window
            // stays hidden forever, and users see "process is running,
            // hook-server is listening, but there is no window".
            // Multiple users have hit this across both platforms.
            //
            // Force a reveal after 3s as a safety net. Healthy
            // startups call show_main_window in ~50ms, well before
            // this fires, so the no-flash UX is preserved. Broken
            // startups at least get a (possibly blank) window the
            // user can interact with — they can quit it, file a bug
            // with devtools, or report what they see, instead of
            // staring at nothing.
            {
                use tauri::Manager;
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    if let Some(window) = handle.get_webview_window("main") {
                        if !window.is_visible().unwrap_or(false) {
                            eprintln!(
                                "[main-window] frontend never called show_main_window after 3s — forcing reveal (likely WebView render failure)"
                            );
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                });
            }

            // Force square corners + no shadow on the borderless window.
            // Windows 11's DWM rounds borderless windows by default and adds
            // a subtle drop-shadow; both create the visible "edge ring" we
            // want gone for the flat look.
            #[cfg(target_os = "windows")]
            {
                use tauri::Manager;
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.set_shadow(false);
                    if let Ok(hwnd) = window.hwnd() {
                        unsafe {
                            use windows::Win32::Foundation::HWND;
                            use windows::Win32::Graphics::Dwm::{
                                DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE,
                                DWMWCP_DONOTROUND,
                            };
                            let pref: i32 = DWMWCP_DONOTROUND.0;
                            let _ = DwmSetWindowAttribute(
                                HWND(hwnd.0 as *mut _),
                                DWMWA_WINDOW_CORNER_PREFERENCE,
                                &pref as *const _ as *const _,
                                std::mem::size_of_val(&pref) as u32,
                            );
                        }
                    }
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .map_err(|e| anyhow::anyhow!("Error while building tauri application: {}", e))?
        .run(|app_handle, event| {
            // ── Graceful PTY-child cleanup on app exit ─────────────────
            // Issue #28: closing Coffee CLI without first killing tabs
            // left orphan `claude.exe` / `node.exe` alive on Windows
            // (they don't share a job with the parent by default), which
            // held `~/.claude/` session locks and broke the NEXT launch's
            // Claude Code tab.
            //
            // Two-layer fix:
            //   1. Here (graceful path): on ExitRequested, drain every
            //      session and fire kill_tx → drops PTY master → SIGHUP
            //      flows down the pipe → child exits cleanly.
            //   2. Job Object (crash-proof path, see terminal.rs): every
            //      child is bound to a kill-on-close job so even a hard
            //      crash / force-quit takes them with us.
            if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
                let state = app_handle.state::<AppState>();
                let mut n = 0usize;
                if let Ok(mut map) = state.terminal_session.lock() {
                    n = map.len();
                    for (_, session) in map.drain() {
                        let _ = session.kill_tx.send(());
                    }
                }
                if n > 0 {
                    eprintln!(
                        "[Tier Terminal] App exiting — sent kill_tx to {} session(s)",
                        n
                    );
                }
            }
        });

    Ok(())
}
