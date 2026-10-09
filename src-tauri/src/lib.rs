mod api;
mod config;
mod meta_windows;
mod watcher;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};

use config::{ConfigStore, GameMeta, MetaCache};
use watcher::{GameProc, Target};

struct AppState {
    cfg: ConfigStore,
    meta: MetaCache,
    api: Mutex<api::GpnApi>,
    targets: Mutex<Vec<Target>>,
    procs: Mutex<Vec<GameProc>>,
}

fn meta_key(exe: &str) -> String {
    std::path::Path::new(exe)
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| exe.to_lowercase())
}

fn watched_meta(state: &AppState) -> HashMap<String, Option<GameMeta>> {
    let games = state.cfg.config.lock().unwrap().games.clone();
    games
        .into_iter()
        .map(|g| {
            let m = state.meta.get(&meta_key(&g));
            (g, m)
        })
        .collect()
}

fn state_json(state: &State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "procs": *state.procs.lock().unwrap(),
        "targets": *state.targets.lock().unwrap(),
        "watched": state.cfg.config.lock().unwrap().games.clone(),
        "gpnServerUrl": state.cfg.config.lock().unwrap().gpn_server_url,
        "meta": watched_meta(state),
    })
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> serde_json::Value {
    state_json(&state)
}

#[tauri::command]
async fn watch_game(app: AppHandle, exe: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.cfg.update(|c| {
        if !c.games.contains(&exe) {
            c.games.push(exe.clone());
        }
    });
    resolve_meta_inner(&app, &exe).await;
    Ok(())
}

#[tauri::command]
async fn unwatch_game(app: AppHandle, exe: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.cfg.update(|c| c.games.retain(|g| g != &exe));
    Ok(())
}

#[tauri::command]
async fn save_settings(app: AppHandle, server_url: String, token: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.cfg.update(|c| {
        c.gpn_server_url = server_url;
        c.gpn_token = token;
    });
    let (url, tok) = {
        let c = state.cfg.config.lock().unwrap();
        (c.gpn_server_url.clone(), c.gpn_token.clone())
    };
    *state.api.lock().unwrap() = api::GpnApi::new(url, tok);
    Ok(())
}

#[tauri::command]
async fn ping_server(app: AppHandle) -> Result<String, String> {
    let api = {
        let state = app.state::<AppState>();
        let guard = state.api.lock().unwrap();
        guard.clone()
    };
    api.ping().await
}

async fn resolve_meta_inner(app: &AppHandle, exe: &str) {
    let key = meta_key(exe);
    let state = app.state::<AppState>();
    if state.meta.get(&key).is_some() {
        return;
    }
    let file_name = std::path::Path::new(exe)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| exe.to_string());
    let name_only = GameMeta {
        name: file_name
            .trim_end_matches(".exe")
            .trim_end_matches(".EXE")
            .to_string(),
        company: None,
        icon: None,
        source: "filename".into(),
    };

    let resolved = meta_windows::resolve(exe).map(|m| {
        let name_src = m.name.clone();
        GameMeta {
            name: m.name.unwrap_or_else(|| name_only.name.clone()),
            company: m.company,
            icon: m
                .icon_b64
                .filter(|b| meta_windows::valid_png_b64(b))
                .map(|b| format!("data:image/png;base64,{b}")),
            source: if name_src.is_some() {
                "versioninfo"
            } else {
                "filename"
            }
            .into(),
        }
    }).unwrap_or(name_only);

    state.meta.insert(key, resolved);
}

/// Background loop: poll processes + connections, emit state, push to VPS.
/// Body wrapped in catch_unwind so any panic (e.g. from OS command parsing)
/// logs and retries instead of killing the app.
async fn watcher_loop(app: AppHandle) {
    loop {
        let result = tokio::spawn({
            let app = app.clone();
            std::panic::AssertUnwindSafe(watcher_tick(app))
        });
        match result.await {
            Ok(()) => {}
            Err(e) => log::error!("watcher tick panicked: {e}"),
        }
        let poll = {
            let state = app.state::<AppState>();
            let cfg = state.cfg.config.lock().unwrap();
            cfg.poll_interval_ms
        };
        tokio::time::sleep(Duration::from_millis(poll)).await;
    }
}

async fn watcher_tick(app: AppHandle) {
        let state = app.state::<AppState>();
        let watched = state.cfg.config.lock().unwrap().games.clone();
        if watched.is_empty() {
            let _ = app.emit("gpn-state", state_json(&state));
            return;
        }

        let procs = watcher::find_processes(&watched);
        let targets = watcher::connections_for(&procs.iter().map(|p| p.pid).collect::<Vec<_>>());

        let changed = {
            let mut cur = state.targets.lock().unwrap();
            let prev: std::collections::HashSet<_> = cur.iter().map(t_key).collect();
            let new: std::collections::HashSet<_> = targets.iter().map(t_key).collect();
            let changed = prev != new;
            *cur = targets.clone();
            changed
        };
        *state.procs.lock().unwrap() = procs;

        let _ = app.emit("gpn-state", state_json(&state));

        if changed && !targets.is_empty() {
            let api = {
                let s = app.state::<AppState>();
                let guard = s.api.lock().unwrap();
                guard.clone()
            };
            let t = targets.clone();
            tokio::spawn(async move {
                if let Err(e) = api.push_targets(&t).await {
                    log::warn!("push failed: {e}");
                }
            });
        }
}

fn t_key(t: &Target) -> String {
    format!(
        "{}:{}:{}",
        t.proto,
        t.ip,
        t.port.map(|p| p.to_string()).unwrap_or_default()
    )
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Dashboard", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().unwrap())
        .menu(&menu)
        .tooltip("GPN Client")
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();
            let cfg = ConfigStore::new(&data_dir);
            let meta = MetaCache::new(&data_dir);
            let api = api::GpnApi::new(
                cfg.config.lock().unwrap().gpn_server_url.clone(),
                cfg.config.lock().unwrap().gpn_token.clone(),
            );
            app.manage(AppState {
                cfg,
                meta,
                api: Mutex::new(api),
                targets: Mutex::new(Vec::new()),
                procs: Mutex::new(Vec::new()),
            });

            tauri::async_runtime::spawn(watcher_loop(handle.clone()));
            setup_tray(&handle)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            watch_game,
            unwatch_game,
            save_settings,
            ping_server
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
