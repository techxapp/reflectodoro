mod breakit;
mod commands;
mod db;
mod grid;
mod hook;
mod overlay;
mod state;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;

use chrono::Local;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use grid::Phase;
use state::{AppState, OverlayState};

/// Debug builds (`npm run tauri dev`) run in dev mode by default: the overlay
/// shows a visible "Close (DEV)" button and skips the Alt-Tab/Win-key hook.
/// Override with POMODORO_DEV_MODE=0/1 if you need to test enforcement in a
/// debug build, or force it on in a release build for QA.
fn resolve_dev_mode() -> bool {
    match std::env::var("POMODORO_DEV_MODE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => cfg!(debug_assertions),
    }
}

pub(crate) static POMODORO_ENABLED: AtomicBool = AtomicBool::new(true);

async fn run_scheduler(app: AppHandle) {
    let mut last_phase: Option<Phase> = None;

    loop {
        let now = Local::now();
        let slot = grid::slot_for(now);

        if last_phase != Some(slot.phase) {
            if POMODORO_ENABLED.load(Ordering::SeqCst) {
                match slot.phase {
                    Phase::Break => {
                        let (len, include_special) = {
                            let app_state = app.state::<AppState>();
                            let cfg = app_state.breakit_config.lock().unwrap();
                            (cfg.length, cfg.include_special)
                        };
                        let challenge = breakit::generate_challenge(len, include_special);
                        {
                            let state = app.state::<AppState>();
                            let mut ov = state.overlay.lock().unwrap();
                            *ov = OverlayState::opened_for(slot.start_iso(), challenge);
                        }
                        // Guards against the startup webview blank-page race
                        // when the app boots straight into a live break --
                        // a no-op once the app's been running a while.
                        overlay::wait_for_webview_warmup(&app).await;
                        overlay::spawn_or_update_overlay(&app);
                    }
                    Phase::Work => {
                        {
                            let state = app.state::<AppState>();
                            let mut ov = state.overlay.lock().unwrap();
                            if ov.open {
                                ov.time_expired = true;
                            }
                        }
                        overlay::try_close_if_unlocked(&app);
                    }
                }
            }
            last_phase = Some(slot.phase);
        }

        let sleep_dur = (slot.end - Local::now())
            .to_std()
            .unwrap_or(StdDuration::from_secs(1));
        tokio::time::sleep(sleep_dur).await;
    }
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open", "Open Reflectodoro", true, None::<&str>)?;
    let toggle_item = MenuItem::with_id(app, "toggle", "Disable Pomodoro Mode", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &toggle_item, &separator, &quit_item])?;

    TrayIconBuilder::new()
        .menu(&menu)
        .icon(app.default_window_icon().unwrap().clone())
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => {
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            "toggle" => {
                let enabled = !POMODORO_ENABLED.load(Ordering::SeqCst);
                POMODORO_ENABLED.store(enabled, Ordering::SeqCst);
                let label = if enabled {
                    "Disable Pomodoro Mode"
                } else {
                    "Enable Pomodoro Mode"
                };
                let _ = toggle_item.set_text(label);
                let _ = app.emit("pomodoro://enabled-changed", enabled);
            }
            "quit" => {
                // Deliberately a hard process exit, not app.exit()/window.close():
                // this must work unconditionally as a kill switch even while the
                // break overlay's close-requested handler is actively blocking
                // normal window close attempts.
                std::process::exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn setup_dev_kill_switch(app: &AppHandle) -> anyhow::Result<()> {
    // Always registered (not just in dev builds): cheap insurance against a
    // stuck overlay in production too. Task Manager and tray Quit are the
    // other two independent kill switches.
    let shortcut = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::F12,
    );
    app.global_shortcut().on_shortcut(shortcut, move |app, _shortcut, event| {
        if event.state() == ShortcutState::Pressed {
            overlay::close_overlay(app);
        }
    })?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let dev_mode = resolve_dev_mode();

    tauri::Builder::default()
        .manage(AppState::new(dev_mode))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations(db::DB_URL, db::migrations())
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_overlay_state,
            commands::is_dev_mode,
            commands::sync_breakit_config,
            commands::mark_reflection_entered,
            commands::breakit_attempt,
            commands::dev_force_close,
            commands::get_enabled,
            commands::set_enabled,
            commands::get_startup_catchup_slot,
            commands::open_catchup_window,
            commands::get_catchup_slot,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();

            if let Err(e) = handle.autolaunch().enable() {
                eprintln!("failed to enable autostart: {e}");
            }

            setup_tray(&handle)?;
            setup_dev_kill_switch(&handle)?;

            // Hidden, built immediately: gives WebView2 a head start on the
            // startup blank-page race before anything tries to show these.
            // See overlay::WEBVIEW_WARMUP.
            overlay::precreate_windows(&handle);

            let scheduler_handle = handle.clone();
            tauri::async_runtime::spawn(run_scheduler(scheduler_handle));

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
