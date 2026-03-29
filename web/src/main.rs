mod fetch;
mod models;
mod settings;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use models::{AppSettings, ServerStatus, ServerView};

static INDEX_HTML: &str = include_str!("../index.html");

// ── Shared application state ─────────────────────────────────────────────────

struct AppState {
    settings: AppSettings,
    servers: Vec<ServerView>,
    refreshing: bool,
    refresh_done: usize,
    refresh_total: usize,
    refresh_error: Option<String>,

    lan_play_child: Option<Child>,
    connected_addr: Option<String>,
    console_cmd: String,
    console_output: Arc<Mutex<String>>,

    download_state: String, // "idle" | "downloading" | "done" | error message
    /// Detected server info for a Quick Connect session; updated by periodic ping.
    qc_server: Option<ServerView>,
    #[cfg(not(target_os = "windows"))]
    sudo_password_for_kill: Option<String>,
    #[cfg(not(target_os = "windows"))]
    lan_play_exact_pid: Arc<Mutex<Option<u32>>>,
}

type Shared = Arc<AsyncMutex<AppState>>;

// ── API response/request types ────────────────────────────────────────────────

#[derive(Serialize)]
struct ServersResponse {
    servers: Vec<ServerView>,
    refreshing: bool,
    done: usize,
    total: usize,
    error: Option<String>,
}

#[derive(Serialize)]
struct StateResponse {
    connected: bool,
    addr: Option<String>,
    console_cmd: String,
    console: String,
    download_state: String,
    qc_server: Option<ServerView>,
}

#[derive(Deserialize)]
struct ConnectRequest {
    addr: String,
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    sudo_password: Option<String>,
}

#[derive(Deserialize)]
struct DetectRequest {
    addr: String,
}

// ── Error helper ──────────────────────────────────────────────────────────────

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn serve_index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX_HTML)
}

async fn get_settings(State(shared): State<Shared>) -> impl IntoResponse {
    let state = shared.lock().await;
    Json(state.settings.clone())
}

async fn post_settings(
    State(shared): State<Shared>,
    Json(new_cfg): Json<AppSettings>,
) -> impl IntoResponse {
    let mut state = shared.lock().await;
    state.settings = new_cfg.clone();
    settings::save(&new_cfg);
    Json(serde_json::json!({ "ok": true }))
}

async fn post_refresh(State(shared): State<Shared>) -> impl IntoResponse {
    let (cfg, already) = {
        let s = shared.lock().await;
        (s.settings.clone(), s.refreshing)
    };
    if already {
        return (StatusCode::CONFLICT, Json(serde_json::json!({ "error": "already refreshing" })));
    }

    {
        let mut s = shared.lock().await;
        s.refreshing = true;
        s.refresh_done = 0;
        s.refresh_total = 0;
        s.refresh_error = None;
        s.servers = Vec::new();
    }

    let shared2 = shared.clone();
    tokio::spawn(async move {
        // Fetch and filter server list
        let entries = match fetch::fetch_server_list(&cfg).await {
            Ok(list) => list.into_iter().filter(|e| !e.is_hidden()).collect::<Vec<_>>(),
            Err(e) => {
                let mut s = shared2.lock().await;
                s.refreshing = false;
                s.refresh_error = Some(e);
                return;
            }
        };

        let total = entries.len();
        let views: Vec<ServerView> = entries.iter().map(ServerView::from_entry).collect();
        let urls: Vec<String> = entries.iter().map(|e| e.status_url()).collect();

        {
            let mut s = shared2.lock().await;
            s.servers = views.clone();
            s.refresh_total = total;
        }

        let client = match fetch::build_client(cfg.http_timeout_ms) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                let mut s = shared2.lock().await;
                s.refreshing = false;
                s.refresh_error = Some(e);
                return;
            }
        };

        // Check all servers concurrently
        let mut handles = Vec::new();
        for (i, (view, url)) in views.into_iter().zip(urls).enumerate() {
            let client = client.clone();
            let sh = shared2.clone();
            handles.push(tokio::spawn(async move {
                let checked = fetch::check_server(&client, view, url).await;
                let mut s = sh.lock().await;
                if i < s.servers.len() {
                    s.servers[i] = checked;
                }
                s.refresh_done += 1;
            }));
        }
        for h in handles {
            let _ = h.await;
        }

        let mut s = shared2.lock().await;
        // Sort once after the initial load; periodic pings update in-place.
        s.servers.sort_by(|a, b| match (a.ping_ms, b.ping_ms) {
            (Some(pa), Some(pb)) => pa.cmp(&pb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        s.refreshing = false;
    });

    (StatusCode::ACCEPTED, Json(serde_json::json!({ "ok": true })))
}

async fn get_servers(State(shared): State<Shared>) -> impl IntoResponse {
    let s = shared.lock().await;
    Json(ServersResponse {
        servers: s.servers.clone(),
        refreshing: s.refreshing,
        done: s.refresh_done,
        total: s.refresh_total,
        error: s.refresh_error.clone(),
    })
}

async fn post_connect(
    State(shared): State<Shared>,
    Json(req): Json<ConnectRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let (cfg, busy) = {
        let s = shared.lock().await;
        (s.settings.clone(), s.lan_play_child.is_some())
    };
    if busy {
        return Err(ApiError(StatusCode::CONFLICT, "already connected".into()));
    }

    let binary = cfg.client_binary();
    let extra = cfg.build_params();
    let mut lan_args: Vec<String> = Vec::new();
    if !extra.is_empty() {
        lan_args.extend(extra.split_whitespace().map(String::from));
    }
    lan_args.push("--relay-server-addr".into());
    lan_args.push(req.addr.clone());
    if cfg.use_netif && !cfg.netif.is_empty() {
        lan_args.push("--netif".into());
        lan_args.push(cfg.netif.clone());
    }
    lan_args.push("--set-ionbf".into());

    #[cfg(not(target_os = "windows"))]
    if cfg.privileged {
        // Pass None to use cached sudo credentials; Some(pw) to authenticate.
        spawn_privileged(shared, binary, lan_args, req.sudo_password.clone(), req.addr).await?;
        return Ok(Json(serde_json::json!({ "ok": true })));
    }

    spawn_process(shared, binary, lan_args, req.addr).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn spawn_process(
    shared: Shared,
    exe: String,
    args: Vec<String>,
    addr: String,
) -> Result<(), ApiError> {
    let cmd_display = format!("{} {}", exe, args.join(" "));
    match Command::new(&exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let output = Arc::new(Mutex::new(String::new()));
            if let Some(stdout) = child.stdout.take() {
                let out = output.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stdout).lines().flatten() {
                        if let Ok(mut g) = out.lock() { g.push_str(&line); g.push('\n'); }
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let out = output.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().flatten() {
                        if let Ok(mut g) = out.lock() { g.push_str(&line); g.push('\n'); }
                    }
                });
            }
            let mut s = shared.lock().await;
            s.console_output = output;
            s.console_cmd = cmd_display;
            s.connected_addr = Some(addr);
            s.lan_play_child = Some(child);
            Ok(())
        }
        Err(e) => Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to launch: {e}"))),
    }
}

#[cfg(not(target_os = "windows"))]
async fn spawn_privileged(
    shared: Shared,
    binary: String,
    lan_args: Vec<String>,
    password: Option<String>,
    addr: String,
) -> Result<(), ApiError> {
    use std::io::Write;
    use std::os::unix::process::CommandExt as _;

    // Use -n (cached credentials) when no password supplied, -S otherwise.
    let (flag, cmd_display) = match &password {
        None    => ("-n", format!("sudo -n {} {}", binary, lan_args.join(" "))),
        Some(_) => ("-S", format!("sudo {} {}", binary, lan_args.join(" "))),
    };
    let mut sudo_args = vec![flag.to_string(), binary.clone()];
    sudo_args.extend(lan_args.iter().cloned());

    match Command::new("sudo")
        .args(&sudo_args)
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let sudo_pgid = child.id();

            if let Some(mut stdin) = child.stdin.take() {
                if let Some(ref pw) = password {
                    let _ = stdin.write_all(format!("{pw}\n").as_bytes());
                    let _ = stdin.flush();
                }
            }

            let output = Arc::new(Mutex::new(String::new()));
            if let Some(stdout) = child.stdout.take() {
                let out = output.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stdout).lines().flatten() {
                        if let Ok(mut g) = out.lock() { g.push_str(&line); g.push('\n'); }
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let out = output.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().flatten() {
                        if let Ok(mut g) = out.lock() { g.push_str(&line); g.push('\n'); }
                    }
                });
            }

            let mut s = shared.lock().await;
            s.console_output = output;
            s.console_cmd = cmd_display;
            s.connected_addr = Some(addr);
            s.lan_play_child = Some(child);
            // Only store password if one was provided (cache path keeps any previous pw).
            if password.is_some() {
                s.sudo_password_for_kill = password;
            }
            s.lan_play_exact_pid = Arc::new(Mutex::new(Some(sudo_pgid)));
            Ok(())
        }
        Err(e) => Err(ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("Failed to launch: {e}"))),
    }
}

async fn post_disconnect(State(shared): State<Shared>) -> impl IntoResponse {
    // Step 1: extract child + kill info and update state while holding the lock.
    // We MUST release the lock before doing any blocking I/O (v.wait, child.wait)
    // — otherwise GET /api/state poll requests are blocked waiting for the same mutex,
    // which prevents the frontend from receiving the disconnect response.
    let kill_info = {
        let mut s = shared.lock().await;
        let child = match s.lan_play_child.take() {
            Some(c) => c,
            None => return Json(serde_json::json!({ "ok": true })),
        };

        // Clear connected state immediately — poll will pick this up right away
        s.connected_addr = None;
        s.console_cmd = String::new();
        s.qc_server = None;
        // Keep console_output intact so the user can still read it (matches GUI behaviour:
        // the GUI sets show_console=false but does not wipe the output buffer)

        #[cfg(not(target_os = "windows"))]
        let sudo_data = {
            let pw  = s.sudo_password_for_kill.take();
            let pid = s.lan_play_exact_pid.lock().ok().and_then(|g| *g);
            if let Ok(mut g) = s.lan_play_exact_pid.lock() { *g = None; }
            (pw, pid)
        };
        #[cfg(target_os = "windows")]
        let sudo_data = (None::<String>, None::<u32>);

        (child, sudo_data)
    }; // ← AsyncMutex released HERE, before any blocking call

    // Step 2: do all blocking operations (sudo + kill + wait) on a dedicated
    // thread-pool thread so we never block tokio worker threads.
    tokio::task::spawn_blocking(move || {
        let (mut child, (sudo_pw, exact_pid)) = kill_info;

        // Privileged sudo kill — non-Windows only
        #[cfg(not(target_os = "windows"))]
        do_sudo_kill(sudo_pw, exact_pid);
        #[cfg(target_os = "windows")]
        let _ = (sudo_pw, exact_pid);

        let _ = child.kill();
        let _ = child.wait();
    })
    .await
    .ok();

    Json(serde_json::json!({ "ok": true }))
}

/// Step 1 — refresh the sudo credential cache with the stored password.
/// Step 2 — kill the entire process group with `sudo kill -9 -<pgid>`.
///
/// Because sudo was spawned with `process_group(0)`, its PGID equals its own
/// PID.  lan-play inherits that PGID, so one negative-PGID kill signal reaches
/// both processes.  This is more reliable than chasing lan-play's individual
/// PID through /proc, which can race against process startup timing.
#[cfg(not(target_os = "windows"))]
fn do_sudo_kill(sudo_pw: Option<String>, pgid: Option<u32>) {
    use std::io::Write;

    // If we have a stored password, refresh the credential cache first.
    // When the cache was used to connect (-n path), skip this — cache is still fresh.
    if let Some(pw) = sudo_pw {
        if !pw.is_empty() {
            if let Ok(mut v) = Command::new("sudo")
                .args(["-S", "-v"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = v.stdin.take() {
                    let _ = stdin.write_all(format!("{pw}\n").as_bytes());
                    let _ = stdin.flush();
                }
                let _ = v.wait();
            }
        }
    }

    // Kill the entire process group via -n (cache is valid either way).
    if let Some(pg) = pgid {
        let _ = Command::new("sudo")
            .args(["-n", "kill", "-9", &format!("-{pg}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|mut c| c.wait());
    }
}

async fn get_state(State(shared): State<Shared>) -> impl IntoResponse {
    let mut s = shared.lock().await;

    // Detect if child process exited on its own
    let exited = s.lan_play_child
        .as_mut()
        .map(|c| c.try_wait().ok().flatten().is_some())
        .unwrap_or(false);
    if exited {
        s.lan_play_child = None;
        s.connected_addr = None;
    }

    let console = s.console_output.lock().ok().map(|g| g.clone()).unwrap_or_default();

    Json(StateResponse {
        connected: s.lan_play_child.is_some(),
        addr: s.connected_addr.clone(),
        console_cmd: s.console_cmd.clone(),
        console,
        download_state: s.download_state.clone(),
        qc_server: s.qc_server.clone(),
    })
}

async fn post_download(State(shared): State<Shared>) -> impl IntoResponse {
    let (dest, busy) = {
        let s = shared.lock().await;
        (s.settings.client_binary(), s.download_state == "downloading")
    };
    if busy {
        return (StatusCode::CONFLICT, Json(serde_json::json!({ "error": "already downloading" })));
    }

    {
        let mut s = shared.lock().await;
        s.download_state = "downloading".into();
    }

    let shared2 = shared.clone();
    tokio::spawn(async move {
        let result = fetch::download_binary(&dest).await;
        let mut s = shared2.lock().await;
        s.download_state = match result {
            Ok(()) => "done".into(),
            Err(e) => format!("error: {e}"),
        };
    });

    (StatusCode::ACCEPTED, Json(serde_json::json!({ "ok": true })))
}

async fn get_info(State(shared): State<Shared>) -> impl IntoResponse {
    #[cfg(not(target_os = "windows"))]
    let privileged = { let s = shared.lock().await; s.settings.privileged };
    #[cfg(target_os = "windows")]
    let privileged = { let _ = shared; false };
    Json(serde_json::json!({
        "platform": std::env::consts::OS,
        "privileged": privileged,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn get_nics() -> impl IntoResponse {
    Json(serde_json::json!({ "nics": enumerate_nics() }))
}

/// Returns `{ "cached": true }` when the sudo credential cache is still valid.
async fn get_sudo_check() -> impl IntoResponse {
    #[cfg(not(target_os = "windows"))]
    {
        let cached = Command::new("sudo")
            .args(["-n", "-v"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut c| c.wait())
            .map(|s| s.success())
            .unwrap_or(false);
        return Json(serde_json::json!({ "cached": cached }));
    }
    #[cfg(target_os = "windows")]
    Json(serde_json::json!({ "cached": false }))
}

async fn post_detect(
    State(shared): State<Shared>,
    Json(req): Json<DetectRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let timeout_ms = { shared.lock().await.settings.http_timeout_ms };
    match fetch::detect_server_type(&req.addr, timeout_ms).await {
        Ok(view) => {
            shared.lock().await.qc_server = Some(view.clone());
            Ok(Json(serde_json::json!({ "ok": true, "server": view })))
        }
        Err(msg) => Err(ApiError(StatusCode::BAD_GATEWAY, msg)),
    }
}

/// Returns (display_name, pcap_device_name) pairs for non-loopback interfaces.
fn enumerate_nics() -> Vec<serde_json::Value> {
    #[cfg(not(target_os = "windows"))]
    {
        let mut nics = Vec::new();
        if let Ok(rd) = std::fs::read_dir("/sys/class/net") {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name != "lo" {
                    nics.push(serde_json::json!({ "display": name, "pcap": name }));
                }
            }
        }
        nics.sort_by(|a, b| a["display"].as_str().cmp(&b["display"].as_str()));
        nics
    }
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::HKEY_LOCAL_MACHINE;
        use winreg::RegKey;
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let net_class = r"SYSTEM\CurrentControlSet\Control\Network\{4D36E972-E325-11CE-BFC1-08002BE10318}";
        let mut nics = Vec::new();
        if let Ok(base) = hklm.open_subkey(net_class) {
            for guid in base.enum_keys().flatten() {
                if !guid.starts_with('{') { continue; }
                let conn_path = format!("{}\\{}\\Connection", net_class, guid);
                if let Ok(conn) = hklm.open_subkey(&conn_path) {
                    let friendly: String = conn.get_value("Name").unwrap_or_else(|_| guid.clone());
                    let pcap_name = format!("\\Device\\NPF_{}", guid);
                    nics.push(serde_json::json!({ "display": friendly, "pcap": pcap_name }));
                }
            }
        }
        nics.sort_by(|a, b| a["display"].as_str().cmp(&b["display"].as_str()));
        nics
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let port = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<u16>().ok())
        .unwrap_or(8080);

    let state: Shared = Arc::new(AsyncMutex::new(AppState {
        settings: settings::load(),
        servers: Vec::new(),
        refreshing: false,
        refresh_done: 0,
        refresh_total: 0,
        refresh_error: None,
        lan_play_child: None,
        connected_addr: None,
        console_cmd: String::new(),
        console_output: Arc::new(Mutex::new(String::new())),
        download_state: "idle".into(),
        qc_server: None,
        #[cfg(not(target_os = "windows"))]
        sudo_password_for_kill: None,
        #[cfg(not(target_os = "windows"))]
        lan_play_exact_pid: Arc::new(Mutex::new(None)),
    }));

    // ── Periodic ping task (every 1 s, updates servers in-place) ─────────────
    {
        let state_ping = state.clone();
        tokio::spawn(async move {
            let mut ping_client: Option<(u64, Arc<reqwest::Client>)> = None;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let (to_ping, timeout_ms) = {
                    let s = state_ping.lock().await;
                    if s.refreshing { continue; }
                    let mut entries: Vec<(String, String, bool)> = s.servers.iter()
                        .map(|v| (v.addr.clone(), v.status_url(), false))
                        .collect();
                    if let Some(ref qc) = s.qc_server {
                        entries.push((qc.addr.clone(), qc.status_url(), true));
                    }
                    if entries.is_empty() { continue; }
                    (entries, s.settings.http_timeout_ms)
                };

                // Reuse the client unless the timeout setting changed.
                let client = match &ping_client {
                    Some((t, c)) if *t == timeout_ms => c.clone(),
                    _ => {
                        match fetch::build_client(timeout_ms) {
                            Ok(c) => {
                                let c = Arc::new(c);
                                ping_client = Some((timeout_ms, c.clone()));
                                c
                            }
                            Err(_) => continue,
                        }
                    }
                };

                let mut handles = Vec::new();
                for (addr, url, is_qc) in to_ping {
                    let c = client.clone();
                    handles.push(tokio::spawn(async move {
                        let start = std::time::Instant::now();
                        match c.get(&url).send().await {
                            Ok(resp) => {
                                let elapsed = start.elapsed().as_millis();
                                if let Ok(body) = resp.text().await {
                                    if let Ok(st) = serde_json::from_str::<ServerStatus>(&body) {
                                        return (addr, Some(elapsed), true, Some(st), is_qc);
                                    }
                                }
                                (addr, Some(elapsed), true, None, is_qc)
                            }
                            Err(_) => (addr, None, false, None, is_qc),
                        }
                    }));
                }

                let mut results = Vec::new();
                for h in handles { if let Ok(r) = h.await { results.push(r); } }

                let mut s = state_ping.lock().await;
                if s.refreshing { continue; }
                for (addr, ping_ms, reachable, status, is_qc) in results {
                    let view_opt = if is_qc {
                        s.qc_server.as_mut()
                    } else {
                        s.servers.iter_mut().find(|v| v.addr == addr)
                    };
                    if let Some(v) = view_opt {
                        v.reachable = reachable;
                        v.ping_ms   = ping_ms;
                        if let Some(st) = status {
                            v.online       = st.online;
                            v.idle         = st.idle;
                            v.version      = st.version;
                            v.client_count = st.client_count;
                        }
                    }
                }
            }
        });
    }

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
        .route("/api/settings", get(get_settings).post(post_settings))
        .route("/api/refresh", post(post_refresh))
        .route("/api/servers", get(get_servers))
        .route("/api/connect", post(post_connect))
        .route("/api/disconnect", post(post_disconnect))
        .route("/api/state", get(get_state))
        .route("/api/download", post(post_download))
        .route("/api/info", get(get_info))
        .route("/api/nics", get(get_nics))
        .route("/api/sudo-check", get(get_sudo_check))
        .route("/api/detect", post(post_detect))
        .with_state(state);

    let ip = local_ip().unwrap_or_else(|| "127.0.0.1".into());
    let addr = format!("0.0.0.0:{port}");
    println!("YaSLP-Web listening on http://{ip}:{port}");

    let listener = tokio::net::TcpListener::bind(&addr).await.expect("Failed to bind");
    axum::serve(listener, app).await.expect("Server error");
}

/// Detect the machine's outbound local IP by connecting a UDP socket to an
/// external address (no packets are actually sent).
fn local_ip() -> Option<String> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?;
    Some(addr.ip().to_string())
}
