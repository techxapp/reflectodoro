//! Backs the About page's log-export controls -- handing the on-disk log
//! file(s) (see CLAUDE.md's "Debugging from production logs") to a
//! destination the user picked via the native save dialog, without them
//! needing to know Tauri's `app_log_dir()` location for their OS.
//!
//! Plain `std::fs` + the `zip` crate rather than tauri-plugin-fs, same
//! reasoning as `commands::read_text_file`/`write_text_file`: an
//! app-defined command needs no capability entry at all, and the
//! `.log`/`.zip`-extension checks below are the same cheap mitigation
//! against this app's null CSP that those commands use.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

fn require_extension(path: &str, ext: &str) -> Result<(), String> {
    let ok = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if ok {
        Ok(())
    } else {
        Err(format!("only .{ext} files are supported"))
    }
}

/// Log files (the active file plus any rotated archives -- see lib.rs's
/// `tauri_plugin_log` setup) sorted newest-first by modified time.
/// Deliberately sorts by mtime rather than parsing the plugin's
/// `{name}_{date}.log` archive-naming convention, so this doesn't need to
/// track that format if the plugin ever changes it -- the active file is
/// always the most recently written to, so it naturally sorts first.
fn log_files_newest_first(app: &AppHandle) -> Result<Vec<PathBuf>, String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .map_err(|e| format!("failed to read log directory {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("log"))
        .filter_map(|p| {
            p.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| (t, p))
        })
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

#[tauri::command]
pub fn export_last_log_file(app: AppHandle, dest: String) -> Result<(), String> {
    require_extension(&dest, "log")?;
    let files = log_files_newest_first(&app)?;
    let newest = files.first().ok_or("no log file found")?;
    std::fs::copy(newest, &dest).map_err(|e| format!("failed to copy log file: {e}"))?;
    Ok(())
}

/// Zips up to `count` most recent log files (active file + rotated
/// archives) into `dest`. `count` is clamped to what's actually on disk --
/// asking for more than exists just archives everything there is rather
/// than erroring.
#[tauri::command]
pub fn export_log_archive(app: AppHandle, dest: String, count: u32) -> Result<(), String> {
    require_extension(&dest, "zip")?;
    let files = log_files_newest_first(&app)?;
    if files.is_empty() {
        return Err("no log files found".into());
    }
    let take = (count.max(1) as usize).min(files.len());

    let zip_file = File::create(&dest).map_err(|e| format!("failed to create {dest}: {e}"))?;
    let mut writer = zip::ZipWriter::new(zip_file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for path in &files[..take] {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("log file had a non-UTF8 name")?;
        writer
            .start_file(name, options)
            .map_err(|e| format!("failed to add {name} to archive: {e}"))?;
        let mut buf = Vec::new();
        File::open(path)
            .and_then(|mut f| f.read_to_end(&mut buf))
            .map_err(|e| format!("failed to read {name}: {e}"))?;
        writer
            .write_all(&buf)
            .map_err(|e| format!("failed to write {name} into archive: {e}"))?;
    }
    writer
        .finish()
        .map_err(|e| format!("failed to finalize archive: {e}"))?;
    Ok(())
}
