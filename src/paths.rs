//! Shared filesystem path helpers.
//!
//! `home()` resolves the user's home directory - shared by modules that build
//! paths under `~/.coffee-cli/` (marketplace, skills). Previously each module
//! had its own byte-identical copy.

use std::path::PathBuf;

/// The user's home directory, or an error string if the OS can't resolve it
/// (no `$HOME` on Linux / profile-load failure on Windows).
pub(crate) fn home() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "No home directory".to_string())
}
