//! Backs the About page's "Export system info" button -- a small plain-text
//! report of the machine/OS/display setup behind whatever's in the exported
//! logs (log_export.rs), for someone helping diagnose a platform-specific
//! issue (a display-scaling problem, the macOS Space/fullscreen bugs, the
//! Linux X11-vs-Wayland gap, ...). Deliberately narrow: no app settings, no
//! user content -- see CLAUDE.md's "Debugging from production logs" for the
//! same "never log user content" property this follows.
//!
//! Same `std::fs` + extension-check pattern as log_export.rs (reuses its
//! `require_extension`) -- no capability entry needed, for the same reason.

use tauri::{AppHandle, WebviewWindow};
#[cfg(target_os = "android")]
use tauri::Manager;

use crate::log_export::require_extension;

fn theme_line(window: &WebviewWindow) -> String {
    match window.theme() {
        Ok(tauri::Theme::Light) => "Light".to_string(),
        Ok(tauri::Theme::Dark) => "Dark".to_string(),
        Ok(_) => "unknown".to_string(),
        Err(e) => {
            log::warn!("system_info::theme_line: window.theme() failed: {e:?}");
            "unknown".to_string()
        }
    }
}

/// Desktop only -- Tauri's monitor API has nothing to report on Android
/// (there's no multi-monitor concept), which instead gets its single
/// screen's resolution/density from the Android bridge in `android_lines`.
#[cfg(not(target_os = "android"))]
fn display_lines(window: &WebviewWindow) -> Vec<String> {
    let primary_position = window.primary_monitor().ok().flatten().map(|m| *m.position());
    match window.available_monitors() {
        Ok(monitors) if !monitors.is_empty() => monitors
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let is_primary = primary_position == Some(*m.position());
                format!(
                    "  Monitor {}{}: {}x{} @ scale {:.2}, position ({}, {}){}",
                    i + 1,
                    m.name().map(|n| format!(" ({n})")).unwrap_or_default(),
                    m.size().width,
                    m.size().height,
                    m.scale_factor(),
                    m.position().x,
                    m.position().y,
                    if is_primary { " [primary]" } else { "" },
                )
            })
            .collect(),
        Ok(_) => vec!["  (no monitors reported)".to_string()],
        Err(e) => {
            log::warn!("system_info::display_lines: available_monitors() failed: {e:?}");
            vec!["  (unavailable)".to_string()]
        }
    }
}

#[cfg(target_os = "linux")]
fn session_type_line() -> String {
    if crate::hook::linux_impl::is_x11_session() {
        "X11".to_string()
    } else {
        "Wayland (or unknown)".to_string()
    }
}

/// OS name/version, everywhere except Android (which has its own line below,
/// from `Build.VERSION.RELEASE`/`SDK_INT` -- `os_info` doesn't identify
/// Android specifically since it's not in scope for this build's targets).
#[cfg(not(target_os = "android"))]
fn os_line() -> String {
    let info = os_info::get();
    format!("{} {}", info.os_type(), info.version())
}

#[cfg(target_os = "android")]
fn android_lines(app: &AppHandle) -> Vec<String> {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    match bridge.get_system_info() {
        Ok(v) => {
            let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("");
            let manufacturer = v.get("manufacturer").and_then(|x| x.as_str()).unwrap_or("");
            let release = v.get("androidRelease").and_then(|x| x.as_str()).unwrap_or("");
            let sdk = v.get("sdkInt").and_then(|x| x.as_u64()).unwrap_or(0);
            let width = v.get("widthPx").and_then(|x| x.as_u64()).unwrap_or(0);
            let height = v.get("heightPx").and_then(|x| x.as_u64()).unwrap_or(0);
            let density = v.get("densityDpi").and_then(|x| x.as_u64()).unwrap_or(0);
            vec![
                format!("OS: Android {release} (SDK {sdk})"),
                format!("Device: {manufacturer} {model}"),
                String::new(),
                "Displays:".to_string(),
                format!("  Screen: {width}x{height} @ {density} dpi"),
            ]
        }
        Err(e) => {
            log::error!("system_info::android_lines: get_system_info bridge call failed: {e:?}");
            vec!["OS: Android (version/device/display unavailable)".to_string()]
        }
    }
}

#[tauri::command]
pub fn export_system_info(app: AppHandle, window: WebviewWindow, dest: String) -> Result<(), String> {
    require_extension(&dest, "txt")?;

    let mut lines: Vec<String> = vec![
        "Reflectodoro system info".to_string(),
        format!("App version: {}", app.package_info().version),
        format!("Architecture: {}", std::env::consts::ARCH),
        String::new(),
    ];

    #[cfg(target_os = "android")]
    {
        lines.extend(android_lines(&app));
    }
    #[cfg(not(target_os = "android"))]
    {
        lines.push(format!("OS: {}", os_line()));
        #[cfg(target_os = "linux")]
        lines.push(format!("Session type: {}", session_type_line()));
        lines.push(String::new());
        lines.push("Displays:".to_string());
        lines.extend(display_lines(&window));
    }

    lines.push(String::new());
    lines.push(format!("System theme: {}", theme_line(&window)));

    let report = lines.join("\n") + "\n";
    std::fs::write(&dest, report).map_err(|e| format!("failed to write {dest}: {e}"))?;
    Ok(())
}
