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
//!   * updates itself through the Tauri updater (signed with our own key).

mod fivem;
mod nick;
mod running;

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

/// Reads `serverAddress` from a news file. Missing, empty or invalid = back to the built-in address.
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
    if let Some(path) = settings_path(&app) {
        let mut saved = nick::load(&path);
        saved.fivem_path = exe.display().to_string();
        nick::save(&path, &saved).map_err(|_| "save_failed".to_string())?;
    }
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
    let Some(address) = effective_address() else {
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
    #[cfg(windows)]
    {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        let explorer = std::path::Path::new(&windir).join("explorer.exe");
        if std::process::Command::new(&explorer).arg(&url).spawn().is_ok() {
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

#[tauri::command]
async fn server_status() -> Status {
    let Some(address) = effective_address() else {
        return Status { max_players: config().max_players, ..Default::default() };
    };
    let target = parse_address(&address);
    let mut status = match &target {
        Target::Direct { host, port } => status_direct(host, *port).await,
        Target::JoinCode(code) => status_join_code(code).await,
    };
    status.address = display_address(&target);
    status
}

// ---------------------------------------------------------------- nickname

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("settings.json"))
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
    let path = settings_path(&app);
    let mut saved = path.as_deref().map(nick::load).unwrap_or_default();
    if saved.launcher_id.is_empty() {
        saved.launcher_id = nick::new_launcher_id();
    }
    saved.nick = wanted.clone();
    if let Some(p) = &path {
        let _ = nick::save(p, &saved);
    }
    let Some(address) = effective_address() else {
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            detect_fivem,
            pick_fivem,
            play,
            close_fivem,
            get_nick,
            send_nick,
            server_status,
            fetch_news,
            open_external,
            check_update,
            install_update
        ])
        .run(tauri::generate_context!())
        .expect("error while running COLDZ launcher");
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
        for r in news.get("rules").and_then(|r| r.as_array()).into_iter().flatten() {
            assert!(r.pointer("/title/ru").and_then(|t| t.as_str()).is_some(), "rule without ru title");
            assert!(r.pointer("/text/ru").and_then(|t| t.as_str()).is_some(), "rule without ru text");
        }
    }
}
