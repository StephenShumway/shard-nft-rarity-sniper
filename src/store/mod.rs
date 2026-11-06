//! Tiny shared filesystem helpers for the JSON-backed config stores
//! (`settings/`, `wallet/`, `task/`). All three persist via [`atomic_write`] so
//! a crash mid-write can never leave a half-written, unparseable file.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Base directory the JSON config stores (`settings/`, `wallet/`, `task/`)
/// resolve against.
///
/// Release builds anchor to the directory containing the running executable, so
/// the bot is fully portable: keep the exe and its `settings/`, `wallet/`, and
/// `task/` folders together and it finds them no matter how it's launched
/// (double-click, shortcut, or from a different working directory). Copy that
/// one folder and the config travels with it.
///
/// Debug builds stay relative to the current working directory, so under
/// `cargo tauri dev` (CWD = `src-tauri/`) the stores live in
/// `src-tauri/settings/`, `src-tauri/wallet/`, and `src-tauri/task/` (all
/// gitignored).
#[must_use]
pub fn config_dir() -> PathBuf {
    #[cfg(not(debug_assertions))]
    {
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
        {
            return dir;
        }
    }
    PathBuf::from(".")
}

/// Resolve a config-relative path (e.g. `"settings/settings.json"`) against
/// [`config_dir`].
#[must_use]
pub fn config_path(rel: &str) -> PathBuf {
    config_dir().join(rel)
}

/// Write `data` to `path` atomically: write to a sibling `.tmp` then rename over
/// the target. `std::fs::rename` replaces an existing destination file on both
/// Windows and Unix, so the swap is atomic from a reader's perspective. Creates
/// the parent directory if missing.
///
/// # Errors
/// Returns an error if the parent directory can't be created or the temp
/// write or rename fails.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Current wall-clock time as whole unix seconds (UTC). Used for the cached
/// wallet balance timestamp, and the frontend renders it relative.
#[must_use]
pub fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Current wall-clock time as unix milliseconds. Used to throttle high-rate UI
/// progress events.
#[must_use]
pub fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
