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

use models::{AppSettings, ServerView};

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
}

#[derive(Deserialize)]
struct ConnectRequest {
    addr: String,
    #[allow(dead_code)]
    sudo_password: Option<String>,
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

        shared2.lock().await.refreshing = false;
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

    #[cfg(not(target_os = "windows"))]
    if cfg.privileged {
        let pw = req.sudo_password.clone().unwrap_or_default();
        if pw.is_empty() {
            return Err(ApiError(StatusCode::BAD_REQUEST, "sudo_password_required".into()));
        }
        spawn_privileged(shared, binary, lan_args, pw, req.addr).await?;
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
    password: String,
    addr: String,
) -> Result<(), ApiError> {
    use std::io::Write;
    use std::os::unix::process::CommandExt as _;

    let mut sudo_args = vec!["-S".to_string(), binary.clone()];
    sudo_args.extend(lan_args.iter().cloned());
    let cmd_display = format!("sudo {} {}", binary, lan_args.join(" "));

    match Command::new("sudo")
        .args(&sudo_args)
        // Give sudo its own process group (PGID = sudo's PID).
        // lan-play inherits that PGID, so `kill -9 -<pgid>` kills both
        // in one shot — no need to chase lan-play's exact PID via /proc.
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            // With process_group(0), PGID == sudo's own PID — available immediately.
            let sudo_pgid = child.id();

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(format!("{password}\n").as_bytes());
                let _ = stdin.flush();
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
            s.sudo_password_for_kill = Some(password);
            // Store the PGID (not lan-play's individual PID).
            // do_sudo_kill uses `kill -9 -<pgid>` to kill the whole group.
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

    let password = match sudo_pw {
        Some(pw) if !pw.is_empty() => pw,
        _ => return,
    };

    // Refresh sudo credential cache so the non-interactive -n kill works
    if let Ok(mut v) = Command::new("sudo")
        .args(["-S", "-v"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = v.stdin.take() {
            let _ = stdin.write_all(format!("{password}\n").as_bytes());
            let _ = stdin.flush();
        }
        let _ = v.wait();
    }

    // `kill -9 -<pgid>` sends SIGKILL to every process in the group:
    // the sudo wrapper AND lan-play (which inherited the PGID).
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
        #[cfg(not(target_os = "windows"))]
        sudo_password_for_kill: None,
        #[cfg(not(target_os = "windows"))]
        lan_play_exact_pid: Arc::new(Mutex::new(None)),
    }));

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
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    println!("YaSLP-Web listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await.expect("Failed to bind");
    axum::serve(listener, app).await.expect("Server error");
}
