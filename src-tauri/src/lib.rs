//! COLDZ launcher backend.
//!
//! The launcher never touches FiveM or game files. It only:
//!   * looks for an installed FiveM (fivem.rs: the path chosen by hand, registry, a running FiveM, usual folders and
//!     shortcuts; where it looked goes to fivem-search.log next to settings.json),
//!   * starts the real FiveM client with `fivem://connect/<address>` — not a second time while FiveM is starting or
//!     stuck (running.rs: that is FiveM's "LinkProtocolIPC" error); can close stuck FiveM processes on request,
//!   * reads the public server endpoints (/dynamic.json, /players.json, /info.json),
//!   * downloads a news JSON file (it may also carry the server address and the rules,
//!     so both can be changed without a new launcher build),
//!   * sends the player's nickname to our own server before "Play" (nick.rs: `POST /z_menu/nick`),
//!   * at start, before "Play" and on «Проверить связь» asks the Cfx.re services FiveM needs and our server
//!     separately (netcheck.rs): a provider that cuts Cfx.re is not the COLDZ server's fault,
//!   * on «Играть» asks the main server first and, when it does not answer, the reserve routes from news.json
//!     (route.rs, 0.3.5), and connects through the first that answers,
//!   * updates itself through the Tauri updater (signed with our own key).

mod fivem;
mod hosts;
mod netcheck;
mod nick;
mod route;
mod running;
mod signin;
mod tunnel;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Mutex;

const CONFIG_JSON: &str = include_str!("../../launcher.config.json");
const BUNDLED_NEWS: &str = include_str!("../../news/news.json");

const HTTP_TIMEOUT: Duration = Duration::from_secs(4);
const PING_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------- settings

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Link {
    /// Icon name from ui/icons.js: vk, facebook, youtube, tiktok, discord, telegram, instagram, x.
    icon: String,
    label: String,
    /// Optional small text on the icon, e.g. "EN" / "PT".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    badge: Option<String>,
    /// Empty = button hidden.
    #[serde(default)]
    url: String,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Config {
    server_name: String,
    /// Built-in address. Empty = not known yet (the launcher then waits for
    /// `serverAddress` in news.json). A non-empty `serverAddress` in news.json wins.
    #[serde(default)]
    server_address: String,
    /// 0.3.5: built-in reserve routes to the server (route.rs), `host:port`. A `relays` list in news.json wins
    /// (an empty list there = no reserve routes).
    #[serde(default)]
    relays: Vec<String>,
    /// 0.4.0: built-in sign-in hosts and relay tunnels (signin.rs). A `signin` object in news.json wins.
    #[serde(default)]
    signin: signin::Setup,
    max_players: u32,
    owner_email: String,
    fivem_download_url: String,
    news_url: String,
    #[serde(default)]
    links: Vec<Link>,
}

fn config() -> &'static Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(|| serde_json::from_str(CONFIG_JSON).expect("launcher.config.json is invalid"))
}

/// Where to connect: a plain `host:port` or a `cfx.re/join/<code>` link.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Direct { host: String, port: u16 },
    JoinCode(String),
}

fn parse_address(raw: &str) -> Target {
    let mut a = raw.trim();
    for prefix in ["fivem://connect/", "https://", "http://"] {
        if let Some(rest) = a.strip_prefix(prefix) {
            a = rest;
        }
    }
    let a = a.trim_end_matches('/');
    if let Some(code) = a.strip_prefix("cfx.re/join/") {
        return Target::JoinCode(code.to_string());
    }
    // A bare join code: short, letters/digits only, no dots or colon.
    if !a.contains('.') && !a.contains(':') && !a.is_empty() && a.len() <= 8 {
        return Target::JoinCode(a.to_string());
    }
    match a.rsplit_once(':') {
        Some((h, p)) => Target::Direct {
            host: h.to_string(),
            port: p.parse().unwrap_or(30120),
        },
        None => Target::Direct { host: a.to_string(), port: 30120 },
    }
}

fn connect_url(t: &Target) -> String {
    match t {
        Target::Direct { host, port } => format!("fivem://connect/{host}:{port}"),
        Target::JoinCode(code) => format!("fivem://connect/cfx.re/join/{code}"),
    }
}

/// Accepts only a sane `host:port`, host name, IP or cfx.re join code, so a typo
/// (or anything strange) in news.json can never end up in the fivem:// link.
fn valid_address(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 100 {
        return false;
    }
    if let Some((_, p)) = raw.rsplit_once(':') {
        if !raw.contains("://") && p.parse::<u16>().map_or(true, |n| n == 0) {
            return false;
        }
    }
    match parse_address(raw) {
        Target::JoinCode(code) => {
            !code.is_empty() && code.len() <= 16 && code.chars().all(|c| c.is_ascii_alphanumeric())
        }
        Target::Direct { host, port } => {
            port != 0
                && !host.is_empty()
                && host.len() <= 253
                && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                && !host.starts_with(['.', '-'])
        }
    }
}

/// Address from news.json (remote, cached or bundled). `None` = use the built-in one.
fn remote_address() -> &'static RwLock<Option<String>> {
    static ADDR: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    ADDR.get_or_init(|| RwLock::new(None))
}

/// Reserve routes from news.json. `None` = the key is missing: use the built-in ones.
fn remote_relays() -> &'static RwLock<Option<Vec<String>>> {
    static RELAYS: OnceLock<RwLock<Option<Vec<String>>>> = OnceLock::new();
    RELAYS.get_or_init(|| RwLock::new(None))
}

/// 0.4.0: `signin` from news.json. `None` = the key is missing: use the built-in setup.
fn remote_signin() -> &'static RwLock<Option<signin::Setup>> {
    static S: OnceLock<RwLock<Option<signin::Setup>>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(None))
}

/// The sign-in hosts and tunnels in use (signin.rs `clean`): from news.json, or the built-in ones.
fn effective_signin() -> signin::Setup {
    let remote = remote_signin().read().ok().and_then(|g| g.clone());
    signin::clean(remote.as_ref().unwrap_or(&config().signin))
}

/// Reads `serverAddress` from a news file. Missing, empty or invalid = back to the built-in address.
/// 0.3.5: also `relays` (a list of `host:port`): missing or not a list = the built-in reserve routes.
/// 0.4.0: also `signin` (an object: groups and tunnels): missing or not an object = the built-in setup.
fn apply_feed(v: &serde_json::Value) {
    let addr = v
        .get("serverAddress")
        .and_then(|a| a.as_str())
        .map(str::trim)
        .filter(|a| valid_address(a))
        .map(str::to_string);
    if let Ok(mut slot) = remote_address().write() {
        *slot = addr;
    }
    let relays = v
        .get("relays")
        .and_then(|r| r.as_array())
        .map(|list| list.iter().filter_map(|r| r.as_str()).map(str::to_string).collect::<Vec<_>>());
    if let Ok(mut slot) = remote_relays().write() {
        *slot = relays;
    }
    let signin = v
        .get("signin")
        .filter(|s| s.is_object())
        .and_then(|s| serde_json::from_value::<signin::Setup>(s.clone()).ok());
    if let Ok(mut slot) = remote_signin().write() {
        *slot = signin;
    }
}

/// Only plain `host:port` addresses (a cfx.re/join code cannot be a reserve route), written as `host:port`, without
/// repeats and without the main address, at most `route::MAX_RELAYS`.
fn clean_relays(raw: &[String], primary: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in raw.iter().map(|r| r.trim()).filter(|r| valid_address(r)) {
        let Target::Direct { host, port } = parse_address(r) else { continue };
        let a = format!("{host}:{port}");
        if Some(a.as_str()) != primary && !out.contains(&a) {
            out.push(a);
        }
        if out.len() >= route::MAX_RELAYS {
            break;
        }
    }
    out
}

/// The main address as `host:port` (or `cfx.re/join/<code>`).
fn primary_address() -> Option<String> {
    effective_address().map(|a| display_address(&parse_address(&a)))
}

/// The reserve routes in use: from news.json, or the built-in ones.
fn effective_relays() -> Vec<String> {
    let remote = remote_relays().read().ok().and_then(|g| g.clone());
    clean_relays(remote.as_deref().unwrap_or(&config().relays), primary_address().as_deref())
}

/// The route chosen last (route.rs): by «Играть» or by the connection check.
fn current_route() -> &'static RwLock<Option<route::Route>> {
    static ROUTE: OnceLock<RwLock<Option<route::Route>>> = OnceLock::new();
    ROUTE.get_or_init(|| RwLock::new(None))
}

/// The chosen route, while it is still the main address or one of the reserve routes (news.json may change both).
fn valid_route() -> Option<route::Route> {
    let r = current_route().read().ok().and_then(|g| g.clone())?;
    let primary = primary_address()?;
    (r.address == primary || effective_relays().contains(&r.address)).then_some(r)
}

/// Where FiveM, the nick and the status go: the chosen route, or the main address.
fn route_address() -> Option<String> {
    valid_route().map(|r| r.address).or_else(effective_address)
}

/// Remembers the chosen route for this run and (when something answered) in settings.json for the next start.
fn remember_route(app: &AppHandle, r: &route::Route) {
    if let Ok(mut slot) = current_route().write() {
        *slot = Some(r.clone());
    }
    if r.kind == "none" || last_route(app) == r.address {
        return;
    }
    let _ = update_settings(app, |s| s.last_route = r.address.clone());
}

fn last_route(app: &AppHandle) -> String {
    settings_path(app).map(|p| nick::load(&p).last_route).unwrap_or_default()
}

/// «Играть», step 0 (0.3.5): the main server answers within ~3 s: straight to it; else the first reserve route that
/// answers (route.rs). Err("no_address"): no address yet. A cfx.re/join code is not asked: Cfx.re finds the server.
#[tauri::command]
async fn choose_route(app: AppHandle) -> Result<route::Route, String> {
    let Some(address) = effective_address() else {
        return Err("no_address".into());
    };
    let target = parse_address(&address);
    let primary = display_address(&target);
    let chosen = match &target {
        Target::JoinCode(_) => route::Route::main(&primary),
        Target::Direct { .. } => route::pick(&primary, &effective_relays(), &last_route(&app), route::PROBE_TIMEOUT).await,
    };
    remember_route(&app, &chosen);
    Ok(chosen)
}

/// The address the launcher really uses, or `None` if it is not set anywhere yet.
fn effective_address() -> Option<String> {
    if let Some(a) = remote_address().read().ok().and_then(|g| g.clone()) {
        return Some(a);
    }
    let built_in = config().server_address.trim();
    valid_address(built_in).then(|| built_in.to_string())
}

/// What players see and type into F8: `1.2.3.4:30120` or `cfx.re/join/abc123`.
fn display_address(t: &Target) -> String {
    match t {
        Target::Direct { host, port } => format!("{host}:{port}"),
        Target::JoinCode(code) => format!("cfx.re/join/{code}"),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicConfig {
    #[serde(flatten)]
    cfg: Config,
    version: String,
}

#[tauri::command]
fn get_config(app: AppHandle) -> PublicConfig {
    PublicConfig {
        cfg: config().clone(),
        version: app.package_info().version.to_string(),
    }
}

// ---------------------------------------------------------------- FiveM

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct FivemInfo {
    installed: bool,
    path: Option<String>,
    /// Which source found it (fivem.rs): manual, registry, process, folder, shortcut.
    found_by: Option<String>,
}

/// `<app config dir>\fivem-search.log`: where the launcher looked for FiveM (next to settings.json).
fn search_log_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("fivem-search.log"))
}

/// What the log said last time: the window asks every few seconds while FiveM is missing, the log gets a new entry
/// only when the answer changes (or on a manual choice).
fn last_logged() -> &'static std::sync::Mutex<Option<String>> {
    static LAST: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| std::sync::Mutex::new(None))
}

/// Looks for FiveM (fivem.rs). `why` — a title for the log; `force` — write the log even if nothing changed.
fn find_fivem(app: &AppHandle, why: &str, force: bool) -> Option<fivem::Found> {
    let manual = settings_path(app)
        .map(|p| nick::load(&p).fivem_path)
        .filter(|p| !p.trim().is_empty())
        .map(PathBuf::from);
    let (found, trace) = fivem::search(&fivem::System { manual });
    let key = found.as_ref().map_or_else(|| "none".to_string(), |f| f.path.display().to_string());
    let changed = last_logged()
        .lock()
        .map(|mut g| {
            let changed = g.as_deref() != Some(key.as_str());
            *g = Some(key.clone());
            changed
        })
        .unwrap_or(true);
    if force || changed {
        if let Some(log) = search_log_path(app) {
            let title = match &found {
                Some(f) => format!("{why}: FiveM найден ({}): {}", f.stage, f.path.display()),
                None => format!("{why}: FiveM не найден"),
            };
            fivem::write_log(&log, &title, &trace);
        }
    }
    found
}

fn info_of(found: Option<fivem::Found>) -> FivemInfo {
    FivemInfo {
        installed: found.is_some(),
        path: found.as_ref().map(|f| f.path.display().to_string()),
        found_by: found.map(|f| f.stage.to_string()),
    }
}

#[tauri::command]
fn detect_fivem(app: AppHandle) -> FivemInfo {
    info_of(find_fivem(&app, "проверка", false))
}

/// "Choose FiveM.exe manually": the Windows file dialog. Ok(None) — closed without a choice; Err("not_fivem") — the
/// file does not lead to FiveM.exe (fivem.rs resolve), nothing saved; Ok(Some) — saved to settings.json and found.
#[tauri::command]
async fn pick_fivem(app: AppHandle, title: String) -> Result<Option<FivemInfo>, String> {
    #[cfg(windows)]
    let owner: isize = app
        .get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map_or(0, |h| h.0 as isize);
    #[cfg(not(windows))]
    let owner: isize = 0;
    let picked = tauri::async_runtime::spawn_blocking(move || fivem::pick_file(&title, owner))
        .await
        .map_err(|_| "dialog_failed".to_string())?;
    let Some(file) = picked else {
        return Ok(None);
    };
    let Some(exe) = fivem::resolve(&file) else {
        if let Some(log) = search_log_path(&app) {
            fivem::write_log(&log, &format!("вручную выбран {}: это не FiveM.exe", file.display()), &[]);
        }
        return Err("not_fivem".into());
    };
    update_settings(&app, |s| s.fivem_path = exe.display().to_string())
        .1
        .map_err(|_| "save_failed".to_string())?;
    Ok(Some(info_of(find_fivem(&app, "указан вручную", true))))
}

/// When the launcher last started FiveM: a second "Play" while FiveM is still starting must not start it again.
fn last_launch() -> &'static std::sync::Mutex<Option<Instant>> {
    static LAST: OnceLock<std::sync::Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| std::sync::Mutex::new(None))
}

/// FiveM may take this long to show its window (update check, first start).
const STARTING_FOR: Duration = Duration::from_secs(150);

fn launched_recently() -> bool {
    last_launch().lock().ok().and_then(|g| *g).map_or(false, |t| t.elapsed() < STARTING_FOR)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlayResult {
    /// "launched" — FiveM started with the link; "passed" — FiveM was open, the link went into it;
    /// "starting" — FiveM is still starting from the previous "Play", nothing done.
    action: &'static str,
}

/// Starts the real FiveM client and tells it to join our server (running.rs: not a second time).
/// Errors for the UI: no_address, fivem_missing, fivem_stuck (FiveM processes without a game window), launch_failed.
#[tauri::command]
fn play(app: AppHandle) -> Result<PlayResult, String> {
    // 0.3.5: the route chosen right before (choose_route): the main server or a reserve route
    let Some(address) = route_address() else {
        return Err("no_address".into());
    };
    let url = connect_url(&parse_address(&address));
    let Some(exe) = find_fivem(&app, "«Играть»", false).map(|f| f.path) else {
        return Err("fivem_missing".into());
    };
    let action = match running::decide(&running::state_of(&running::processes()), launched_recently()) {
        running::Decision::Starting => return Ok(PlayResult { action: "starting" }),
        running::Decision::Stuck => return Err("fivem_stuck".into()),
        running::Decision::PassLink => "passed",
        running::Decision::Launch => {
            if let Ok(mut g) = last_launch().lock() {
                *g = Some(Instant::now());
            }
            "launched"
        }
    };
    // FiveM refuses to start when its parent process is not the Windows shell or a browser
    // ("This application should be launched directly from the shell or a web browser").
    // So hand the fivem:// link to explorer.exe: the shell then starts FiveM itself (or, if FiveM is open,
    // FiveM passes the link to the running game).
    // 16.11.2026: explorer opens the link with the registered fivem:// handler. Missing or pointing to a FiveM.exe
    // that is gone, Windows shows «explorer.exe — Приложение не найдено» and FiveM does not start: point the handler
    // at the FiveM.exe found above first (fivem.rs ensure_link_handler); could not — skip explorer, start FiveM.exe.
    #[cfg(windows)]
    {
        let (handler_ok, lines) = fivem::ensure_link_handler(&exe);
        if !lines.is_empty() {
            if let Some(log) = search_log_path(&app) {
                fivem::write_log(&log, &format!("«Играть»: обработчик ссылки fivem:// для {}", exe.display()), &lines);
            }
        }
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let explorer = std::path::Path::new(&windir).join("explorer.exe");
        if handler_ok && std::process::Command::new(&explorer).arg(&url).spawn().is_ok() {
            return Ok(PlayResult { action });
        }
    }
    // Fallback: `FiveM.exe "<link>"` directly (works on FiveM builds without the parent check).
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(&url);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    match cmd.spawn() {
        Ok(_) => Ok(PlayResult { action }),
        // Fall back to the registered fivem:// link handler.
        Err(_) => app
            .opener()
            .open_url(&url, None::<&str>)
            .map(|_| PlayResult { action })
            .map_err(|_| "launch_failed".into()),
    }
}

/// "Close FiveM" after fivem_stuck: ends every FiveM process. (closed, failed) — failed: FiveM runs as administrator.
#[tauri::command]
fn close_fivem() -> (u32, u32) {
    let result = running::close_matching(running::is_fivem);
    if let Ok(mut g) = last_launch().lock() {
        *g = None;
    }
    result
}

// ---------------------------------------------------------------- server status

#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
struct Status {
    online: bool,
    players: u32,
    max_players: u32,
    ping_ms: Option<u32>,
    /// Address in use (for the "copy" line and F8 `connect`). Empty = not set yet.
    address: String,
    /// 0.3.5: the chosen route (route.rs): "main" | "reserve" | "none"; None: not chosen yet.
    route: Option<&'static str>,
}

fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(HTTP_TIMEOUT)
                .user_agent(concat!("COLDZ-Launcher/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("http client")
        })
        .clone()
}

async fn get_json(url: &str) -> Option<serde_json::Value> {
    let resp = http().get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

fn as_u32(v: &serde_json::Value) -> Option<u32> {
    v.as_u64()
        .map(|n| n as u32)
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// "Ping" = time to open a TCP connection to the game port (best of 3).
/// Close to what FiveM shows and works without admin rights (no ICMP).
async fn tcp_ping(host: &str, port: u16) -> Option<u32> {
    let mut best: Option<u32> = None;
    for _ in 0..3 {
        let start = Instant::now();
        let ok = tokio::time::timeout(PING_TIMEOUT, tokio::net::TcpStream::connect((host, port)))
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
        if ok {
            let ms = start.elapsed().as_millis().max(1) as u32;
            best = Some(best.map_or(ms, |b| b.min(ms)));
        }
    }
    best
}

async fn status_direct(host: &str, port: u16) -> Status {
    let base = format!("http://{host}:{port}");
    let max_default = config().max_players;
    let (dynamic_url, players_url, info_url) = (
        format!("{base}/dynamic.json"),
        format!("{base}/players.json"),
        format!("{base}/info.json"),
    );
    let (dynamic, ping) = tokio::join!(get_json(&dynamic_url), tcp_ping(host, port));

    if let Some(d) = dynamic {
        return Status {
            online: true,
            players: d.get("clients").and_then(as_u32).unwrap_or(0),
            max_players: d.get("sv_maxclients").and_then(as_u32).unwrap_or(max_default),
            ping_ms: ping,
            ..Default::default()
        };
    }
    // Older servers: count /players.json and read the limit from /info.json.
    let (players, info) = tokio::join!(get_json(&players_url), get_json(&info_url));
    match players.as_ref().and_then(|p| p.as_array()) {
        Some(list) => Status {
            online: true,
            players: list.len() as u32,
            max_players: info
                .as_ref()
                .and_then(|i| i.pointer("/vars/sv_maxClients"))
                .and_then(as_u32)
                .unwrap_or(max_default),
            ping_ms: ping,
            ..Default::default()
        },
        None => Status { max_players: max_default, ..Default::default() },
    }
}

async fn status_join_code(code: &str) -> Status {
    let max_default = config().max_players;
    let url = format!("https://servers-frontend.fivem.net/api/servers/single/{code}");
    let Some(v) = get_json(&url).await else {
        return Status { max_players: max_default, ..Default::default() };
    };
    let data = v.get("Data").cloned().unwrap_or_default();
    let ping = match data
        .pointer("/connectEndPoints/0")
        .and_then(|e| e.as_str())
        .map(parse_address)
    {
        Some(Target::Direct { host, port }) => tcp_ping(&host, port).await,
        _ => None,
    };
    Status {
        online: true,
        players: data.get("clients").and_then(as_u32).unwrap_or(0),
        max_players: data.get("sv_maxclients").and_then(as_u32).unwrap_or(max_default),
        ping_ms: ping,
        ..Default::default()
    }
}

// ---------------------------------------------------------------- connection check (15.11)

/// `<app config dir>\net-check.log`: what answered how at the connection checks (netcheck.rs), next to settings.json.
fn net_log_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("net-check.log"))
}

/// «Проверить связь», the check before «Играть» and (0.3.4) once at launcher start (netcheck.rs): the Cfx.re services
/// FiveM needs, our server separately (`/info.json`, the first thing FiveM asks when it joins), GitHub (news) to tell
/// "Cfx.re is cut" from "no internet".
#[tauri::command]
async fn check_network(app: AppHandle) -> netcheck::Report {
    // 0.3.5: the main server and the reserve routes (route.rs), all at once; the route for «Играть» is chosen too
    let server = async {
        match effective_address().map(|a| parse_address(&a)) {
            Some(Target::Direct { host, port }) => {
                let primary = format!("{host}:{port}");
                let relays = effective_relays();
                let answers = route::probe_all(&primary, &relays, netcheck::SERVER_TIMEOUT).await;
                let chosen = route::choose(&primary, &relays, &answers, &last_route(&app));
                remember_route(&app, &chosen);
                netcheck::ServerCheck { main: Some(answers.main), reserve: answers.reserve(), route: Some(chosen.kind) }
            }
            // no address yet, or a cfx.re/join code: its status itself comes from Cfx.re
            _ => netcheck::ServerCheck::default(),
        }
    };
    let (mut report, signin) = tokio::join!(netcheck::run(server, netcheck::answers(&config().news_url)), signin_check());
    report.signin = signin;
    if let Some(log) = net_log_path(&app) {
        let (title, lines) = netcheck::log_lines(&report);
        fivem::write_log(&log, &title, &lines);
    }
    report
}

// ---------------------------------------------------------------- sign-in route (0.4.0, signin.rs)

/// What the launcher keeps about the route while it runs.
#[derive(Default)]
struct SigninRun {
    /// The forwarding task (tunnel.rs `serve`) and what it forwards.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    forward: Option<std::sync::Arc<tunnel::Forward>>,
    ip: Option<std::net::IpAddr>,
    /// The hosts file carries the launcher's block.
    active: bool,
    /// The tunnel that answered at the last check.
    tunnel_ok: Option<signin::Tunnel>,
    /// Groups whose straight check failed at the last check (signin.rs `hosts_to_route`).
    failed: Vec<String>,
}

fn signin_run() -> &'static std::sync::Mutex<SigninRun> {
    static RUN: OnceLock<std::sync::Mutex<SigninRun>> = OnceLock::new();
    RUN.get_or_init(|| std::sync::Mutex::new(SigninRun::default()))
}

fn signin_lock() -> std::sync::MutexGuard<'static, SigninRun> {
    signin_run().lock().unwrap_or_else(|e| e.into_inner())
}

/// The elevated copy (`--coldz-hosts <file>`, main.rs): writes the block and exits.
pub fn hosts_main(file: &str) -> i32 {
    hosts::apply(file)
}

/// Runs the futures at once, answers in order (no extra crate for one join).
async fn join_all<F: std::future::Future>(futs: Vec<F>) -> Vec<F::Output> {
    let mut pending: Vec<Option<std::pin::Pin<Box<F>>>> = futs.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for (i, slot) in pending.iter_mut().enumerate() {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(v) => {
                        results[i] = Some(v);
                        *slot = None;
                    }
                    std::task::Poll::Pending => all_done = false,
                }
            }
        }
        if all_done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    results.into_iter().flatten().collect()
}

fn check_host(setup: &signin::Setup) -> Option<String> {
    reqwest::Url::parse(setup.groups.first()?.check.first()?).ok()?.host_str().map(str::to_string)
}

/// For the window and the connection check: the sign-in hosts straight (through the route when it is on) and the
/// first tunnel that answers. None: no groups set up.
async fn signin_check() -> Option<netcheck::SigninReport> {
    let setup = effective_signin();
    let probe_host = check_host(&setup)?;
    let groups = async {
        let futs: Vec<_> = setup.groups.iter().map(|g| netcheck::probe_url(&g.check[0])).collect();
        setup
            .groups
            .iter()
            .zip(join_all(futs).await)
            .map(|(g, p)| netcheck::GroupProbe { id: g.id.clone(), host: p.host, ok: p.ok, problem: p.problem, status: p.status, ms: p.ms })
            .collect::<Vec<_>>()
    };
    let tunnels = async {
        if setup.tunnels.is_empty() {
            return None;
        }
        let started = Instant::now();
        let futs: Vec<_> = setup.tunnels.iter().map(|t| tunnel::probe_full(t, &probe_host)).collect();
        let answers = join_all(futs).await;
        let ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
        let probe = |i: usize, ok: bool, status: Option<u16>, bytes: u64, problem: Option<&'static str>| netcheck::TunnelProbe {
            address: setup.tunnels[i].address.clone(),
            ok,
            status,
            bytes,
            problem,
            ms,
        };
        // the first tunnel that carries data wins; a tunnel that answers but is «frozen» counts as not working
        if let Some(i) = answers.iter().position(|a| matches!(a, Ok(x) if x.ok())) {
            let a = answers[i].as_ref().ok().copied().unwrap_or(tunnel::Answer { status: 0, bytes: 0 });
            return Some((Some(setup.tunnels[i].clone()), probe(i, true, Some(a.status), a.bytes, None)));
        }
        let (i, status, bytes, e) = match answers.iter().enumerate().find(|(_, a)| a.is_ok()) {
            Some((i, Ok(a))) => (i, Some(a.status), a.bytes, "slow"),
            _ => (0, None, 0, answers.first().and_then(|a| a.as_ref().err().copied()).unwrap_or("relay")),
        };
        Some((None, probe(i, false, status, bytes, Some(e))))
    };
    let (groups, tunnel) = tokio::join!(groups, tunnels);
    let active = { signin_lock().active };
    let failed: Vec<String> = groups.iter().filter(|g| !g.ok).map(|g| g.id.clone()).collect();
    let (tunnel_ok, tunnel) = match tunnel {
        Some((t, p)) => (t, Some(p)),
        None => (None, None),
    };
    {
        let mut run = signin_lock();
        run.tunnel_ok = tunnel_ok;
        if !active {
            run.failed = failed.clone();
        }
    }
    Some(netcheck::SigninReport { blocked: !failed.is_empty() && !active, groups, tunnel, active })
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct SigninStatus {
    active: bool,
    /// Where the hosts point while the route is on.
    ip: Option<String>,
    hosts: usize,
    tunnel: Option<String>,
}

fn signin_status_now() -> SigninStatus {
    let run = signin_lock();
    SigninStatus {
        active: run.active,
        ip: run.ip.map(|ip| ip.to_string()),
        hosts: run.forward.as_ref().map(|f| f.hosts.len()).unwrap_or(0),
        tunnel: run.forward.as_ref().map(|f| f.tunnel.address.clone()),
    }
}

#[tauri::command]
fn signin_status() -> SigninStatus {
    signin_status_now()
}

/// Starts (or restarts) the local forwarder for `hosts` through `tunnel`. Returns the loopback address it listens on.
fn start_forwarder(tunnel: signin::Tunnel, hosts: Vec<String>) -> Result<std::net::IpAddr, String> {
    let mut run = signin_lock();
    if let Some(t) = run.task.take() {
        t.abort();
    }
    run.forward = None;
    let (std_listener, ip) = tunnel::bind_local().map_err(|_| "port_busy".to_string())?;
    let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|_| "port_busy".to_string())?;
    let forward = std::sync::Arc::new(tunnel::Forward { tunnel, hosts });
    run.forward = Some(forward.clone());
    run.ip = Some(ip);
    run.task = Some(tauri::async_runtime::spawn(tunnel::serve(listener, forward)));
    Ok(ip)
}

fn stop_forwarder() {
    let mut run = signin_lock();
    if let Some(t) = run.task.take() {
        t.abort();
    }
    run.forward = None;
    run.ip = None;
}

/// A folder the launcher may write the block file to (hosts.rs): next to settings.json.
fn signin_work_dir(app: &AppHandle) -> PathBuf {
    app.path().app_config_dir().unwrap_or_else(|_| std::env::temp_dir())
}

/// The tunnel to use: the one that answered at the last check, or asked now.
async fn pick_tunnel(setup: &signin::Setup) -> Option<signin::Tunnel> {
    if let Some(t) = signin_lock().tunnel_ok.clone() {
        return Some(t);
    }
    let host = check_host(setup)?;
    let futs: Vec<_> = setup.tunnels.iter().map(|t| tunnel::probe_full(t, &host)).collect();
    let answers = join_all(futs).await;
    let picked = answers.iter().position(|a| matches!(a, Ok(x) if x.ok())).map(|i| setup.tunnels[i].clone());
    signin_lock().tunnel_ok = picked.clone();
    picked
}

/// «Включить резервный маршрут входа»: the forwarder, then the hosts block through the Windows prompt.
/// Err: no_setup (no hosts or tunnels in news.json), no_route (no tunnel answers), port_busy (443 is taken on both
/// loopback addresses), cancelled (the player said no to the prompt), failed.
#[tauri::command]
async fn signin_enable(app: AppHandle) -> Result<SigninStatus, String> {
    let setup = effective_signin();
    if setup.groups.is_empty() || setup.tunnels.is_empty() {
        return Err("no_setup".into());
    }
    let tunnel = pick_tunnel(&setup).await.ok_or_else(|| "no_route".to_string())?;
    let failed = signin_lock().failed.clone();
    let hosts = signin::hosts_to_route(&setup, &failed);
    let ip = start_forwarder(tunnel, hosts.clone())?;
    let block = signin::block(&ip.to_string(), &hosts);
    let work = signin_work_dir(&app);
    let written = tauri::async_runtime::spawn_blocking(move || hosts::request(Some(&block), &work))
        .await
        .map_err(|_| "failed".to_string())?;
    match written {
        Ok(()) => {
            signin_lock().active = true;
            let _ = update_settings(&app, |s| s.signin_route = "on".into());
            if let Some(log) = net_log_path(&app) {
                fivem::write_log(&log, &format!("резервный маршрут входа включён: {} хостов → {}", hosts.len(), ip), &hosts);
            }
            Ok(signin_status_now())
        }
        Err(hosts::Refused::Cancelled) => {
            stop_forwarder();
            Err("cancelled".into())
        }
        Err(hosts::Refused::Failed) => {
            stop_forwarder();
            Err("failed".into())
        }
    }
}

/// «Выключить резервный маршрут входа» and the launcher's exit: the block out of the hosts file (one prompt), then
/// the forwarder stops. The player says no: the block stays and so does the forwarder (Err("cancelled")).
#[tauri::command]
async fn signin_disable(app: AppHandle) -> Result<SigninStatus, String> {
    let work = signin_work_dir(&app);
    let removed = tauri::async_runtime::spawn_blocking(move || hosts::request(None, &work))
        .await
        .map_err(|_| "failed".to_string())?;
    match removed {
        Ok(()) => {
            stop_forwarder();
            signin_lock().active = false;
            let _ = update_settings(&app, |s| s.signin_route = "off".into());
            if let Some(log) = net_log_path(&app) {
                fivem::write_log(&log, "резервный маршрут входа выключен", &[]);
            }
            Ok(signin_status_now())
        }
        Err(hosts::Refused::Cancelled) => Err("cancelled".into()),
        Err(hosts::Refused::Failed) => Err("failed".into()),
    }
}

/// At the launcher's exit, on the main thread: the block must not outlive the forwarder.
fn signin_cleanup(app: &AppHandle) {
    let active = { signin_lock().active };
    if !active && hosts::installed().is_none() {
        return;
    }
    let work = signin_work_dir(app);
    if hosts::request(None, &work).is_ok() {
        stop_forwarder();
        signin_lock().active = false;
        let _ = update_settings(app, |s| s.signin_route = "off".into());
    }
}

/// At the launcher's start: a block left in the hosts file (a crash, or the player said no to the removal) with
/// the route "on" in settings.json is taken over without a prompt when the forwarder can listen on the same
/// address and a tunnel answers; otherwise the block is removed (one prompt), so the sign-in hosts never point at
/// a port nobody listens on.
async fn signin_resume(app: AppHandle) {
    let Some((ip, hosts)) = hosts::installed() else { return };
    let wanted_on = settings_path(&app).map(|p| nick::load(&p).signin_route == "on").unwrap_or(false);
    let setup = effective_signin();
    let same_ip = ip.parse::<std::net::IpAddr>().ok();
    if wanted_on && !hosts.is_empty() {
        if let Some(t) = pick_tunnel(&setup).await {
            // the forwarder takes the first free loopback address; it must be the one the block names
            if let Ok(bound) = start_forwarder(t, hosts.clone()) {
                if Some(bound) == same_ip {
                    signin_lock().active = true;
                    if let Some(log) = net_log_path(&app) {
                        fivem::write_log(
                            &log,
                            &format!("резервный маршрут входа продолжен после перезапуска: {} хостов → {}", hosts.len(), bound),
                            &[],
                        );
                    }
                    return;
                }
                stop_forwarder();
            }
        }
    }
    let work = signin_work_dir(&app);
    let removed = tauri::async_runtime::spawn_blocking(move || hosts::request(None, &work)).await;
    if matches!(removed, Ok(Ok(()))) {
        let _ = update_settings(&app, |s| s.signin_route = "off".into());
        if let Some(log) = net_log_path(&app) {
            fivem::write_log(&log, "резервный маршрут входа: старые строки hosts убраны при запуске", &[]);
        }
    }
}


#[tauri::command]
async fn server_status() -> Status {
    // 0.3.5: through the chosen route (a reserve route answers the same /dynamic.json), so the status and the ping are
    // what the player really gets
    let Some(address) = route_address() else {
        return Status { max_players: config().max_players, ..Default::default() };
    };
    let target = parse_address(&address);
    let mut status = match &target {
        Target::Direct { host, port } => status_direct(host, *port).await,
        Target::JoinCode(code) => status_join_code(code).await,
    };
    status.address = display_address(&target);
    status.route = valid_route().map(|r| r.kind);
    status
}

// ---------------------------------------------------------------- nickname

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
}

/// Changes settings.json: one read-modify-write at a time (0.3.5: the connection check remembers the route while
/// «Играть» saves the nick). Returns the saved settings and how the write went.
fn update_settings(app: &AppHandle, change: impl FnOnce(&mut nick::Settings)) -> (nick::Settings, std::io::Result<()>) {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = settings_path(app);
    let mut saved = path.as_deref().map(nick::load).unwrap_or_default();
    change(&mut saved);
    let written = path.as_deref().map_or(Ok(()), |p| nick::save(p, &saved));
    (saved, written)
}

/// The nick saved last time ("" = FiveM name).
#[tauri::command]
fn get_nick(app: AppHandle) -> String {
    settings_path(&app).map(|p| nick::load(&p).nick).unwrap_or_default()
}

/// Where the server takes the nick: the game address, or for a `cfx.re/join/` code its first real endpoint.
async fn nick_endpoint(target: &Target) -> Option<String> {
    match target {
        Target::Direct { host, port } => Some(nick::endpoint(host, *port)),
        Target::JoinCode(code) => {
            let url = format!("https://servers-frontend.fivem.net/api/servers/single/{code}");
            let v = get_json(&url).await?;
            match parse_address(v.pointer("/Data/connectEndPoints/0")?.as_str()?) {
                Target::Direct { host, port } => Some(nick::endpoint(&host, port)),
                Target::JoinCode(_) => None,
            }
        }
    }
}

/// "Play", step 1: remember the nick and tell the server (nick.rs). Err("chars" | "long") — the nick breaks the
/// rules, nothing is sent. The UI starts FiveM unless the server rejected the nick.
#[tauri::command]
async fn send_nick(app: AppHandle, nick: String) -> Result<nick::NickResult, String> {
    let wanted = nick::check(&nick).map_err(str::to_string)?;
    let (saved, _) = update_settings(&app, |s| {
        if s.launcher_id.is_empty() {
            s.launcher_id = nick::new_launcher_id();
        }
        s.nick = wanted.clone();
    });
    // 0.3.5: the same route FiveM will use (choose_route runs before the nick)
    let Some(address) = route_address() else {
        return Ok(nick::NickResult::unreachable(&wanted));
    };
    let Some(url) = nick_endpoint(&parse_address(&address)).await else {
        return Ok(nick::NickResult::unreachable(&wanted));
    };
    Ok(post_nick(&url, &wanted, &saved.launcher_id).await)
}

/// `POST <url> {"nick", "launcher"}` and what the server said.
async fn post_nick(url: &str, wanted: &str, launcher_id: &str) -> nick::NickResult {
    let body = serde_json::json!({ "nick": wanted, "launcher": launcher_id });
    match http().post(url).json(&body).send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            let v = resp.json::<serde_json::Value>().await.ok();
            nick::interpret(code, v.as_ref(), wanted)
        }
        Err(_) => nick::NickResult::unreachable(wanted),
    }
}

// ---------------------------------------------------------------- news

#[derive(Serialize)]
struct News {
    source: &'static str,
    data: serde_json::Value,
}

fn valid_news(v: &serde_json::Value) -> bool {
    v.get("items").map_or(false, |i| i.is_array())
}

#[tauri::command]
async fn fetch_news(app: AppHandle) -> News {
    let cache = app.path().app_cache_dir().ok().map(|d| d.join("news.json"));

    if !config().news_url.is_empty() {
        if let Some(v) = get_json(&config().news_url).await.filter(valid_news) {
            if let Some(c) = &cache {
                let _ = std::fs::create_dir_all(c.parent().unwrap());
                let _ = std::fs::write(c, v.to_string());
            }
            apply_feed(&v);
            return News { source: "remote", data: with_rules(v) };
        }
    }
    let (source, data) = local_news(cache);
    apply_feed(&data);
    News { source, data: with_rules(data) }
}

/// A news file without "rules" (e.g. an older one on GitHub) gets the rules built into the launcher.
fn with_rules(mut v: serde_json::Value) -> serde_json::Value {
    let has_rules = v.get("rules").and_then(|r| r.as_array()).map_or(false, |r| !r.is_empty());
    if !has_rules {
        let bundled: serde_json::Value = serde_json::from_str(BUNDLED_NEWS).unwrap_or_default();
        if let (Some(rules), Some(obj)) = (bundled.get("rules"), v.as_object_mut()) {
            obj.insert("rules".into(), rules.clone());
        }
    }
    v
}

/// Last downloaded news, or the copy built into the launcher.
fn local_news(cache: Option<PathBuf>) -> (&'static str, serde_json::Value) {
    if let Some(v) = cache
        .and_then(|c| std::fs::read_to_string(c).ok())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(valid_news)
    {
        return ("cache", v);
    }
    ("bundled", serde_json::from_str(BUNDLED_NEWS).unwrap_or_default())
}

// ---------------------------------------------------------------- links

/// Opens a web link or e-mail in the default program. Only https:// and mailto:.
#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("mailto:")) {
        return Err("blocked".into());
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- self-update

struct PendingUpdate(Mutex<Option<tauri_plugin_updater::Update>>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
}

#[tauri::command]
async fn check_update(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    let info = update.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        notes: u.body.clone(),
    });
    *app.state::<PendingUpdate>().0.lock().await = update;
    Ok(info)
}

#[tauri::command]
async fn install_update(app: AppHandle) -> Result<(), String> {
    let Some(update) = app.state::<PendingUpdate>().0.lock().await.take() else {
        return Err("no_update".into());
    };
    let mut done: u64 = 0;
    let handle = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                done += chunk as u64;
                if let Some(t) = total.filter(|t| *t > 0) {
                    let _ = handle.emit("update-progress", (done * 100 / t).min(100));
                }
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    // On Windows the installer closes the app itself; restart covers other cases.
    app.restart();
}

// ---------------------------------------------------------------- app

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // TLS crypto for https requests (news, cfx.re list, updates).
    let _ = rustls::crypto::ring::default_provider().install_default();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(PendingUpdate(Mutex::new(None)))
        .setup(|app| {
            // Address from the last downloaded news.json, so the status works before a new download.
            let cache = app.path().app_cache_dir().ok().map(|d| d.join("news.json"));
            apply_feed(&local_news(cache).1);
            // 0.4.0: a sign-in route block left in the hosts file from the last run (signin_resume)
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { signin_resume(handle).await });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            detect_fivem,
            pick_fivem,
            choose_route,
            play,
            close_fivem,
            get_nick,
            send_nick,
            server_status,
            check_network,
            fetch_news,
            open_external,
            check_update,
            install_update,
            signin_status,
            signin_enable,
            signin_disable
        ])
        .build(tauri::generate_context!())
        .expect("error while building COLDZ launcher")
        .run(|app, event| {
            // 0.4.0: the hosts block must not outlive the forwarder (signin.rs)
            if let tauri::RunEvent::Exit = event {
                signin_cleanup(app);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(
            parse_address("127.0.0.1:30120"),
            Target::Direct { host: "127.0.0.1".into(), port: 30120 }
        );
        assert_eq!(
            parse_address("play.coldz.ru"),
            Target::Direct { host: "play.coldz.ru".into(), port: 30120 }
        );
        assert_eq!(parse_address("cfx.re/join/abc123"), Target::JoinCode("abc123".into()));
        assert_eq!(parse_address("https://cfx.re/join/abc123/"), Target::JoinCode("abc123".into()));
        assert_eq!(parse_address("abc123"), Target::JoinCode("abc123".into()));
    }

    #[test]
    fn builds_connect_links() {
        assert_eq!(
            connect_url(&parse_address("1.2.3.4:30120")),
            "fivem://connect/1.2.3.4:30120"
        );
        assert_eq!(
            connect_url(&parse_address("cfx.re/join/abc123")),
            "fivem://connect/cfx.re/join/abc123"
        );
    }

    #[test]
    fn reads_exe_from_handler_command() {
        let exe = std::env::current_exe().unwrap();
        let cmd = format!("\"{}\" \"%1\"", exe.display());
        assert_eq!(fivem::path_from_command(&cmd), Some(exe));
        // a FiveM that is gone: the path is read, but it does not lead to FiveM.exe (fivem.rs resolve)
        let gone = fivem::path_from_command("\"C:\\nope\\FiveM.exe\" \"%1\"").unwrap();
        assert_eq!(fivem::resolve(&gone), None);
    }

    #[test]
    fn https_requests_work() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let res = rt.block_on(async { http().get("https://github.com/").send().await });
        match res {
            Ok(r) => { eprintln!("github status {}", r.status()); assert!(r.status().as_u16() < 500) }
            // Offline machine: nothing to check. Any other error = broken TLS setup.
            Err(e) if e.is_connect() || e.is_timeout() => eprintln!("offline: {e}"),
            Err(e) => panic!("https request failed: {e:?}"),
        }
    }

    #[test]
    fn validates_addresses() {
        for ok in ["1.2.3.4:30120", "play.coldz.ru", "play.coldz.ru:30121", "cfx.re/join/abc123", "abc123"] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in ["", "   ", "1.2.3.4:0", "1.2.3.4:99999", "evil host:30120", "a\"b:1", "-x.ru", "x/../y:1", "1.2.3.4:30120?x"] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    #[test]
    fn news_address_overrides_built_in() {
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "5.6.7.8:30120" }));
        assert_eq!(effective_address().as_deref(), Some("5.6.7.8:30120"));
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "bad host" }));
        assert_ne!(effective_address().as_deref(), Some("bad host"));
        apply_feed(&serde_json::json!({ "items": [] }));
        let built_in = config().server_address.trim();
        assert_eq!(effective_address(), valid_address(built_in).then(|| built_in.to_string()));

        // 0.3.5 reserve routes (same test: these are the same global slots). No `relays` key: the built-in ones.
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683" }));
        assert_eq!(effective_relays(), clean_relays(&config().relays, Some("92.118.11.28:28683")));
        assert_eq!(effective_relays(), s(&["177.3.198.34:30120", "46.247.40.34:30120"]));
        // news.json changes them without a release
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683", "relays": ["5.6.7.8:30120", "bad host", 7] }));
        assert_eq!(effective_relays(), s(&["5.6.7.8:30120"]));
        // an empty list: no reserve routes
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683", "relays": [] }));
        assert!(effective_relays().is_empty());
        // a chosen reserve route that news.json no longer lists is dropped: back to the main address
        *current_route().write().unwrap() = Some(route::Route { kind: "reserve", address: "177.3.198.34:30120".into() });
        assert_eq!(valid_route(), None);
        assert_eq!(route_address().as_deref(), Some("92.118.11.28:28683"));
        apply_feed(&serde_json::json!({ "items": [], "serverAddress": "92.118.11.28:28683" }));
        assert_eq!(route_address().as_deref(), Some("177.3.198.34:30120"));
        assert_eq!(valid_route().map(|r| r.kind), Some("reserve"));
        *current_route().write().unwrap() = None;
        apply_feed(&serde_json::json!({ "items": [] }));
    }

    #[test]
    fn reserve_routes_are_cleaned() {
        let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let raw = s(&[" 177.3.198.34:30120 ", "177.3.198.34:30120", "relay.example", "cfx.re/join/abc123", "92.118.11.28:28683", "x y:1", "1.2.3.4:0"]);
        assert_eq!(clean_relays(&raw, Some("92.118.11.28:28683")), s(&["177.3.198.34:30120", "relay.example:30120"]));
        let many: Vec<String> = (1..=20).map(|i| format!("10.0.0.{i}:30120")).collect();
        assert_eq!(clean_relays(&many, None).len(), route::MAX_RELAYS);
    }

    /// A one-shot fake server on 127.0.0.1: reads one HTTP request, answers `status` + `body`, returns the request.
    async fn fake_server(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len || n == 0 {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&req).to_string()
        });
        (nick::endpoint("127.0.0.1", addr.port()), task)
    }

    #[test]
    fn nick_goes_to_the_server_as_json() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let (url, server) = fake_server(200, r#"{"ok":true,"nick":"Снег"}"#).await;
            let res = post_nick(&url, "Снег", "abc123").await;
            let req = server.await.unwrap();
            assert_eq!(res, nick::NickResult { status: "ok", nick: "Снег".into(), reason: None });
            assert!(req.starts_with("POST /z_menu/nick HTTP/1.1"), "{req}");
            let body = &req[req.find("\r\n\r\n").unwrap() + 4..];
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(v, serde_json::json!({ "nick": "Снег", "launcher": "abc123" }));

            let (url, server) = fake_server(200, r#"{"ok":false,"reason":"reserved"}"#).await;
            let res = post_nick(&url, "Админ", "abc123").await;
            server.await.unwrap();
            assert_eq!((res.status, res.reason.as_deref()), ("rejected", Some("reserved")));

            let (url, server) = fake_server(429, "Rate limit exceeded.").await;
            let res = post_nick(&url, "x", "abc123").await;
            server.await.unwrap();
            assert_eq!(res.reason.as_deref(), Some("often"));

            // old server without the nick: FXServer answers 404 — FiveM still starts
            let (url, server) = fake_server(404, "Not found.").await;
            assert_eq!(post_nick(&url, "x", "abc123").await.status, "unreachable");
            server.await.unwrap();

            // nobody listens
            let closed = {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                l.local_addr().unwrap().port()
            };
            assert_eq!(post_nick(&nick::endpoint("127.0.0.1", closed), "x", "abc").await.status, "unreachable");
        });
    }

    #[test]
    fn old_news_gets_bundled_rules() {
        let v = with_rules(serde_json::json!({ "items": [] }));
        assert!(v.get("rules").and_then(|r| r.as_array()).map_or(false, |r| !r.is_empty()));
        let own = with_rules(serde_json::json!({ "items": [], "rules": [{ "title": { "ru": "x" }, "text": { "ru": "y" } }] }));
        assert_eq!(own["rules"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn config_and_news_are_valid() {
        let c = config();
        assert!(c.server_address.trim().is_empty() || valid_address(&c.server_address));
        assert!(!c.owner_email.is_empty());
        for l in &c.links {
            assert!(l.url.is_empty() || l.url.starts_with("https://"), "link must be https: {}", l.url);
        }
        let news: serde_json::Value = serde_json::from_str(BUNDLED_NEWS).unwrap();
        assert!(valid_news(&news));
        if let Some(a) = news.get("serverAddress").and_then(|a| a.as_str()) {
            assert!(a.trim().is_empty() || valid_address(a), "news.json serverAddress: {a}");
        }
        // 0.3.5: every reserve route (built-in and in news.json) is a plain host:port the launcher accepts
        let news_relays: Vec<String> = news
            .get("relays")
            .and_then(|r| r.as_array())
            .map(|l| l.iter().map(|r| r.as_str().unwrap_or("?").to_string()).collect())
            .unwrap_or_default();
        for list in [&c.relays, &news_relays] {
            assert_eq!(&clean_relays(list, None), list, "relays must be plain host:port: {list:?}");
        }
        for r in news.get("rules").and_then(|r| r.as_array()).into_iter().flatten() {
            assert!(r.pointer("/title/ru").and_then(|t| t.as_str()).is_some(), "rule without ru title");
            assert!(r.pointer("/text/ru").and_then(|t| t.as_str()).is_some(), "rule without ru text");
        }
    }
}
