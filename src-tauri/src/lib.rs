mod api;
mod asn;
mod config;
mod etw;
mod learn;
mod meta_windows;
mod monitor;
mod net;
mod proc;
mod profiles;
mod routing;
mod tunnel;
mod watcher;

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use config::{ConfigStore, GameMeta, MetaCache};
use etw::{EtwHandle, EtwStatus, FlowEvent, PidMap, Proto};
use learn::{Learn, LearnView};
use monitor::{Monitor, TrafficView};
use routing::{desired_routes, Desired, Router};
use tunnel::TunnelManager;
use profiles::{Profile, ProfileEntry, ProfileStore};
use watcher::{game_key, GameProc, Target};

/// ETW flows (UDP has no "connection") count as live targets for this long after the last packet.
const LIVE_FLOW_TTL: Duration = Duration::from_secs(15);
const MAX_TARGETS: usize = 200;

struct EtwSlot {
    status: EtwStatus,
    handle: Option<EtwHandle>,
}

struct AppState {
    cfg: Arc<ConfigStore>,
    meta: MetaCache,
    api: Arc<api::GpnApi>,
    tunnel: Arc<TunnelManager>,
    router: Arc<Router>,
    targets: Mutex<Vec<Target>>,
    procs: Mutex<Vec<GameProc>>,
    /// pid -> game key for the running watched games (read by the ETW callback)
    pids: PidMap,
    /// recent ETW flows of watched games
    live: Mutex<HashMap<(Proto, Ipv4Addr, u16), Instant>>,
    running_games: Mutex<HashSet<String>>,
    learn: Learn,
    /// per-game bandwidth + server breakdown (always on, independent of learn mode)
    monitor: Monitor,
    profiles: ProfileStore,
    etw: Mutex<EtwSlot>,
    etw_tx: UnboundedSender<FlowEvent>,
}

fn meta_key(exe: &str) -> String {
    game_key(exe)
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

fn state_json(state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "procs": *state.procs.lock().unwrap(),
        "targets": *state.targets.lock().unwrap(),
        "watched": state.cfg.config.lock().unwrap().games.clone(),
        "gpnServerUrl": state.cfg.config.lock().unwrap().gpn_server_url,
        "meta": watched_meta(state),
        "etw": state.etw.lock().unwrap().status,
        "learning": state.learn.enabled(),
        "auth": state.api.auth_info(),
        "hasToken": state.api.has_credentials(),
        "insecureUrl": !api::is_secure_or_local(&state.api.server_url()),
        "tunnel": state.tunnel.view(),
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
async fn save_settings(app: AppHandle, server_url: String, token: Option<String>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let url = api::normalize_url(&server_url);
    state.cfg.update(|c| {
        c.gpn_server_url = url.clone();
        if let Some(t) = &token {
            c.gpn_token = t.clone();
        }
    });
    state.api.set_server(&url);
    if let Some(t) = token {
        state.api.set_token(&t);
    }
    Ok(())
}

#[tauri::command]
async fn ping_server(state: State<'_, AppState>) -> Result<String, String> {
    let api = state.api.clone();
    api.ping().await
}

#[tauri::command]
async fn auth_login(state: State<'_, AppState>, username: String, password: String) -> Result<api::AuthInfo, String> {
    let api = state.api.clone();
    api.login(username.trim(), &password).await
}

#[tauri::command]
async fn auth_register(state: State<'_, AppState>, username: String, password: String, invite_code: Option<String>) -> Result<api::AuthInfo, String> {
    let api = state.api.clone();
    api.register_user(username.trim(), &password, invite_code.as_deref().map(str::trim)).await
}

/// Log out and drop the tunnel (it belongs to the user's peer).
#[tauri::command]
async fn auth_logout(state: State<'_, AppState>) -> Result<(), String> {
    state.tunnel.teardown(true).await;
    state.api.logout().await;
    Ok(())
}

// ===== learn mode / profiles / ETW commands =====

/// Live bandwidth, session totals and per-server breakdown for one game (all its processes).
#[tauri::command]
fn get_traffic(state: State<'_, AppState>, game: String, window_secs: Option<usize>) -> TrafficView {
    state.monitor.snapshot(&game_key(&game), window_secs.unwrap_or(120))
}

#[tauri::command]
fn reset_traffic(state: State<'_, AppState>, game: String) {
    state.monitor.begin_session(&game_key(&game));
}

#[tauri::command]
fn get_learn(state: State<'_, AppState>, game: String) -> LearnView {
    state.learn.snapshot(&game_key(&game))
}

#[tauri::command]
fn set_learning(state: State<'_, AppState>, enabled: bool) {
    state.learn.set_enabled(enabled);
}

#[tauri::command]
fn clear_learn(state: State<'_, AppState>, game: String) {
    state.learn.clear(&game_key(&game));
}

#[tauri::command]
fn save_profile(state: State<'_, AppState>, game: String, name: Option<String>, entries: Vec<ProfileEntry>) -> Result<Profile, String> {
    let key = game_key(&game);
    let name = name
        .filter(|n| !n.trim().is_empty())
        .or_else(|| state.meta.get(&key).map(|m| m.name))
        .unwrap_or_else(|| key.clone());
    state.profiles.save(&key, &name, entries)
}

#[tauri::command]
fn get_profiles(state: State<'_, AppState>) -> Vec<Profile> {
    state.profiles.list()
}

#[tauri::command]
fn delete_profile(state: State<'_, AppState>, game: String) {
    state.profiles.delete(&game_key(&game));
}

#[tauri::command]
fn export_profile(state: State<'_, AppState>, game: String) -> Result<String, String> {
    state.profiles.export(&game_key(&game))
}

#[tauri::command]
fn etw_restart(state: State<'_, AppState>) -> EtwStatus {
    start_etw(&state);
    state.etw.lock().unwrap().status.clone()
}

/// ETW needs elevation: start a new elevated instance (UAC prompt) and exit this one.
#[tauri::command]
fn relaunch_as_admin(app: AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let script = "Start-Process -FilePath $env:GPN_SELF -Verb RunAs";
    proc::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("GPN_SELF", exe)
        .spawn()
        .map_err(|e| e.to_string())?;
    // give the elevated instance a moment to start before this one releases the ETW session
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        handle.exit(0);
    });
    Ok(())
}

fn start_etw(state: &AppState) {
    let mut slot = state.etw.lock().unwrap();
    slot.handle = None; // stop any previous session first
    match etw::start(state.etw_tx.clone(), state.pids.clone()) {
        Ok(h) => {
            slot.handle = Some(h);
            slot.status = EtwStatus::running();
        }
        Err(status) => {
            log::warn!("ETW unavailable: {}", status.message);
            slot.status = status;
        }
    }
}

/// Bump when the icon extraction changes so cached entries get refreshed.
const META_VERSION: u32 = 2;

async fn resolve_meta_inner(app: &AppHandle, exe: &str) {
    let key = meta_key(exe);
    let state = app.state::<AppState>();
    let has_path = exe.contains('\\') || exe.contains('/');
    let cached = state.meta.get(&key);
    // up to date, and either complete or impossible to improve from a bare file name
    if let Some(m) = &cached {
        if m.v >= META_VERSION && (m.icon.is_some() || !has_path) {
            return;
        }
    }
    let file_name = std::path::Path::new(exe)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| exe.to_string());
    let name_only = GameMeta {
        name: file_name.trim_end_matches(".exe").trim_end_matches(".EXE").to_string(),
        company: None,
        icon: None,
        source: "filename".into(),
        v: META_VERSION,
    };

    // PowerShell is slow (~1s): keep it off the async runtime threads
    let path = exe.to_string();
    let found = tauri::async_runtime::spawn_blocking(move || meta_windows::resolve(&path))
        .await
        .ok()
        .flatten();
    let mut resolved = found
        .map(|m| {
            let name_src = m.name.clone();
            GameMeta {
                name: m.name.unwrap_or_else(|| name_only.name.clone()),
                company: m.company,
                icon: m
                    .icon_b64
                    .filter(|b| meta_windows::valid_png_b64(b))
                    .map(|b| format!("data:image/png;base64,{b}")),
                source: if name_src.is_some() { "versioninfo" } else { "filename" }.into(),
                v: META_VERSION,
            }
        })
        .unwrap_or(name_only);
    // never lose a good icon just because this lookup couldn't produce one
    if resolved.icon.is_none() {
        resolved.icon = cached.and_then(|m| m.icon);
    }
    state.meta.insert(key, resolved);
}

// ===== background tasks =====

/// ETW events -> live flow table (targets) + learn mode.
async fn flow_consumer(app: AppHandle, mut rx: UnboundedReceiver<FlowEvent>) {
    while let Some(ev) = rx.recv().await {
        let state = app.state::<AppState>();
        let game = state.pids.read().unwrap().get(&ev.pid).cloned();
        let Some(game) = game else { continue };
        {
            let mut live = state.live.lock().unwrap();
            if live.len() < 5000 {
                live.insert((ev.proto, ev.remote_ip, ev.remote_port), Instant::now());
            } else if let Some(t) = live.get_mut(&(ev.proto, ev.remote_ip, ev.remote_port)) {
                *t = Instant::now();
            }
        }
        state.monitor.record(&game, &ev);
        state.learn.record(&game, &ev);
    }
}

/// Batches IPs from learn mode into Team Cymru bulk lookups.
async fn asn_worker(app: AppHandle, mut rx: UnboundedReceiver<String>) {
    while let Some(first) = rx.recv().await {
        tokio::time::sleep(Duration::from_millis(800)).await; // let a burst accumulate
        let mut batch = vec![first];
        while batch.len() < 100 {
            match rx.try_recv() {
                Ok(ip) => batch.push(ip),
                Err(_) => break,
            }
        }
        batch.sort();
        batch.dedup();
        let state = app.state::<AppState>();
        match asn::lookup(&batch).await {
            Ok(results) => state.learn.apply_asn(&batch, results),
            Err(e) => {
                log::warn!("ASN lookup failed: {e}");
                state.learn.apply_asn(&batch, Vec::new()); // retried later by the maintenance loop
            }
        }
    }
}

/// Periodic persistence + retry of failed ASN lookups.
async fn maintenance_loop(app: AppHandle) {
    let mut n: u32 = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let state = app.state::<AppState>();
        state.learn.flush();
        n += 1;
        if n % 12 == 0 {
            state.learn.retry_missing_asn();
        }
    }
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
    state.live.lock().unwrap().retain(|_, t| t.elapsed() < LIVE_FLOW_TTL);

    let watched = state.cfg.config.lock().unwrap().games.clone();
    if watched.is_empty() {
        state.router.update(Desired::default());
        let _ = app.emit("gpn-state", state_json(&state));
        return;
    }

    // tasklist/netstat are blocking: keep them off the async runtime threads
    let (procs, tcp) = tauri::async_runtime::spawn_blocking(move || {
        let procs = watcher::find_processes(&watched);
        let tcp = watcher::connections_for(&procs.iter().map(|p| p.pid).collect::<Vec<_>>());
        (procs, tcp)
    })
    .await
    .unwrap_or_default();

    // tell the ETW callback which PIDs to report, and start a learn session when a game launches
    {
        let mut m = state.pids.write().unwrap();
        m.clear();
        for p in &procs {
            m.insert(p.pid, p.game.clone());
        }
    }
    {
        let now_running: HashSet<String> = procs.iter().map(|p| p.game.clone()).collect();
        let mut prev = state.running_games.lock().unwrap();
        for g in now_running.difference(&prev) {
            state.learn.begin_session(g);
            state.monitor.begin_session(g);
        }
        *prev = now_running;
    }

    // targets = established TCP (netstat) + recent ETW flows (TCP and UDP)
    let mut targets = tcp;
    if !procs.is_empty() {
        let live = state.live.lock().unwrap();
        for (proto, ip, port) in live.keys() {
            targets.push(Target { ip: ip.to_string(), port: Some(*port), proto: proto.as_str().into() });
        }
    }
    targets.sort_by(|a, b| (&a.proto, &a.ip, a.port).cmp(&(&b.proto, &b.ip, b.port)));
    targets.dedup_by(|a, b| a.proto == b.proto && a.ip == b.ip && a.port == b.port);
    targets.truncate(MAX_TARGETS);

    *state.targets.lock().unwrap() = targets.clone();
    *state.procs.lock().unwrap() = procs;

    let _ = app.emit("gpn-state", state_json(&state));

    // pre-route saved profiles of running games + live hosts; the router applies it
    {
        let running: HashSet<String> = state.procs.lock().unwrap().iter().map(|p| p.game.clone()).collect();
        let live: Vec<Ipv4Addr> = targets.iter().filter_map(|t| t.ip.parse().ok()).collect();
        state.router.update(desired_routes(&state.profiles.list(), &running, &live));
    }
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default()
            .with_state_flags(tauri_plugin_window_state::StateFlags::POSITION)
            .build())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir).ok();
            let cfg = Arc::new(ConfigStore::new(&data_dir));
            let meta = MetaCache::new(&data_dir);
            let api = {
                let c = cfg.config.lock().unwrap().clone();
                let sink_cfg = cfg.clone();
                Arc::new(api::GpnApi::new(&c.gpn_server_url, &c.gpn_token, c.auth, Box::new(move |s| sink_cfg.update(|c| c.auth = s))))
            };
            let tunnel = Arc::new(TunnelManager::new(api.clone(), cfg.clone()));
            let router = Router::new(tunnel.clone());

            let (flow_tx, flow_rx) = tokio::sync::mpsc::unbounded_channel::<FlowEvent>();
            let (asn_tx, asn_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let learn = Learn::new(&data_dir);
            learn.set_asn_sender(asn_tx);

            app.manage(AppState {
                cfg: cfg.clone(),
                meta,
                api,
                tunnel: tunnel.clone(),
                router: router.clone(),
                targets: Mutex::new(Vec::new()),
                procs: Mutex::new(Vec::new()),
                pids: Arc::new(RwLock::new(HashMap::new())),
                live: Mutex::new(HashMap::new()),
                running_games: Mutex::new(HashSet::new()),
                learn,
                monitor: Monitor::default(),
                profiles: ProfileStore::new(&data_dir),
                etw: Mutex::new(EtwSlot { status: EtwStatus::off(), handle: None }),
                etw_tx: flow_tx,
            });

            {
                let h = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let games = h.state::<AppState>().cfg.config.lock().unwrap().games.clone();
                    for g in games {
                        resolve_meta_inner(&h, &g).await;
                    }
                });
            }
            start_etw(&handle.state::<AppState>());
            tauri::async_runtime::spawn(flow_consumer(handle.clone(), flow_rx));
            tauri::async_runtime::spawn(asn_worker(handle.clone(), asn_rx));
            tauri::async_runtime::spawn(maintenance_loop(handle.clone()));
            tauri::async_runtime::spawn(watcher_loop(handle.clone()));
            {
                let t = tunnel.clone();
                tauri::async_runtime::spawn(async move {
                    t.init().await; // adopt a tunnel left running by a previous run
                    t.watchdog().await;
                });
                let c = cfg.clone();
                tauri::async_runtime::spawn(router.run(move || c.config.lock().unwrap().tunnel_teardown_when_idle));
            }
            setup_tray(&handle)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            watch_game,
            unwatch_game,
            save_settings,
            ping_server,
            auth_login,
            auth_register,
            auth_logout,
            get_learn,
            get_traffic,
            reset_traffic,
            set_learning,
            clear_learn,
            save_profile,
            get_profiles,
            delete_profile,
            export_profile,
            etw_restart,
            relaunch_as_admin
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                state.learn.flush();
                // don't leave the VPN running headless
                tauri::async_runtime::block_on(state.tunnel.teardown(false));
            }
        });
}
