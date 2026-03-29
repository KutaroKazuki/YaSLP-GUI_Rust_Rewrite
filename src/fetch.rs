use std::io;
use std::time::{Duration, Instant};
use reqwest::blocking::Client;
use yaslp_shared::{AppSettings, ServerEntry, ServerStatus};
use yaslp_shared::parse::{download_url, parse_server_list};

use crate::models::Server;

/// Build a single shared client. Call once per refresh cycle.
pub fn build_client(timeout_ms: u64) -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .connection_verbose(false)
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|e| e.to_string())
}

/// Fetch and parse the server list from the configured URL.
pub fn fetch_server_list(cfg: &AppSettings) -> Result<Vec<ServerEntry>, String> {
    let client = Client::builder()
        .timeout(Duration::from_millis(cfg.http_timeout_ms.saturating_mul(3).min(15_000)))
        .build()
        .map_err(|e| e.to_string())?;

    let body = client
        .get(&cfg.server_list_url)
        .send()
        .map_err(|e| format!("Request failed: {e}"))?
        .text()
        .map_err(|e| format!("Failed to read body: {e}"))?;

    parse_server_list(&body)
}

/// Download the lan-play binary for the current platform to `dest_path`.
pub fn download_binary(dest_path: &str) -> Result<(), String> {
    let url = download_url();

    if let Some(parent) = std::path::Path::new(dest_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create dir: {e}"))?;
        }
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let mut resp = client.get(url).send().map_err(|e| format!("Download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let mut file = std::fs::File::create(dest_path)
        .map_err(|e| format!("Cannot create file: {e}"))?;
    io::copy(&mut resp, &mut file).map_err(|e| format!("Write failed: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dest_path, perms)
            .map_err(|e| format!("chmod failed: {e}"))?;
    }

    Ok(())
}

/// Detect whether `addr` (host:port) is a lan-play relay and which backend
/// type it runs. Tries GET /info (rust/node) then GET / (dotnet).
/// Returns a fully-populated Server on success, or an error message.
pub fn detect_server_type(addr: &str, timeout_ms: u64) -> Result<Server, String> {
    // Use at least 3 s for first-contact detection.
    let client = Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms.max(3_000)))
        .build()
        .map_err(|e| e.to_string())?;

    // ── rust / node: GET /info → { "online": N, "idle": M, ... }
    let start = std::time::Instant::now();
    if let Ok(resp) = client.get(&format!("http://{}/info", addr)).send() {
        let elapsed = start.elapsed().as_millis();
        if let Ok(body) = resp.text() {
            if let Ok(status) = serde_json::from_str::<ServerStatus>(&body) {
                if status.online.is_some() || status.idle.is_some() {
                    return Ok(Server {
                        entry: make_qc_entry(addr, "rust"),
                        status,
                        ping_ms: Some(elapsed),
                        reachable: true,
                    });
                }
            }
        }
    }

    // ── dotnet: GET / → { "clientCount": N }
    let start = std::time::Instant::now();
    if let Ok(resp) = client.get(&format!("http://{}/", addr)).send() {
        let elapsed = start.elapsed().as_millis();
        if let Ok(body) = resp.text() {
            if let Ok(status) = serde_json::from_str::<ServerStatus>(&body) {
                if status.client_count.is_some() {
                    return Ok(Server {
                        entry: make_qc_entry(addr, "dotnet"),
                        status,
                        ping_ms: Some(elapsed),
                        reachable: true,
                    });
                }
            }
        }
    }

    Err("Not a lan-play server".into())
}

fn make_qc_entry(addr: &str, server_type: &str) -> ServerEntry {
    let mut parts = addr.splitn(2, ':');
    let ip   = parts.next().unwrap_or(addr).to_string();
    let port = parts.next().unwrap_or("11451").to_string();
    ServerEntry {
        ip: Some(ip),
        port: Some(serde_json::Value::String(port)),
        server_type: Some(server_type.into()),
        ..Default::default()
    }
}

/// Check a single server's online status using the provided shared client.
pub fn check_server(client: &Client, mut server: Server) -> Server {
    let url = server.entry.status_url();
    let start = Instant::now();
    match client.get(&url).send() {
        Ok(resp) => {
            let elapsed = start.elapsed().as_millis();
            if let Ok(body) = resp.text() {
                if let Ok(status) = serde_json::from_str::<ServerStatus>(&body) {
                    server.status = status;
                    server.ping_ms = Some(elapsed);
                    server.reachable = true;
                } else {
                    server.ping_ms = Some(elapsed);
                    server.reachable = true;
                }
            }
        }
        Err(_) => {
            server.reachable = false;
            server.ping_ms = None;
        }
    }
    server
}
