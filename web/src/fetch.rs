use std::time::{Duration, Instant};
use reqwest::Client;
use yaslp_shared::{AppSettings, ServerEntry, ServerStatus};
use yaslp_shared::parse::{download_url, parse_server_list};

use crate::models::ServerView;

pub fn build_client(timeout_ms: u64) -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|e| e.to_string())
}

pub async fn fetch_server_list(cfg: &AppSettings) -> Result<Vec<ServerEntry>, String> {
    let client = Client::builder()
        .timeout(Duration::from_millis(
            cfg.http_timeout_ms.saturating_mul(3).min(15_000),
        ))
        .build()
        .map_err(|e| e.to_string())?;

    let body = client
        .get(&cfg.server_list_url)
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("Failed to read body: {e}"))?;

    parse_server_list(&body)
}

/// Detect whether `addr` is a lan-play relay and which backend it runs.
/// Tries GET /info (rust/node) then GET / (dotnet).
pub async fn detect_server_type(addr: &str, timeout_ms: u64) -> Result<ServerView, String> {
    let client = Client::builder()
        .timeout(Duration::from_millis(timeout_ms.max(3_000)))
        .build()
        .map_err(|e| e.to_string())?;

    // rust / node: GET /info → { "online": N, "idle": M, ... }
    let start = Instant::now();
    if let Ok(resp) = client.get(&format!("http://{}/info", addr)).send().await {
        let elapsed = start.elapsed().as_millis();
        if let Ok(body) = resp.text().await {
            if let Ok(status) = serde_json::from_str::<ServerStatus>(&body) {
                if status.online.is_some() || status.idle.is_some() {
                    let mut view = ServerView {
                        addr: addr.to_string(),
                        server_type: "rust".into(),
                        flag: None,
                        reachable: true,
                        online: None, idle: None, version: None, client_count: None,
                        ping_ms: Some(elapsed),
                    };
                    view.apply_status(status, elapsed);
                    return Ok(view);
                }
            }
        }
    }

    // dotnet: GET / → { "clientCount": N }
    let start = Instant::now();
    if let Ok(resp) = client.get(&format!("http://{}/", addr)).send().await {
        let elapsed = start.elapsed().as_millis();
        if let Ok(body) = resp.text().await {
            if let Ok(status) = serde_json::from_str::<ServerStatus>(&body) {
                if status.client_count.is_some() {
                    let mut view = ServerView {
                        addr: addr.to_string(),
                        server_type: "dotnet".into(),
                        flag: None,
                        reachable: true,
                        online: None, idle: None, version: None, client_count: None,
                        ping_ms: Some(elapsed),
                    };
                    view.apply_status(status, elapsed);
                    return Ok(view);
                }
            }
        }
    }

    Err("Not a lan-play server".into())
}

pub async fn check_server(client: &Client, mut view: ServerView, status_url: String) -> ServerView {
    let start = Instant::now();
    match client.get(&status_url).send().await {
        Ok(resp) => {
            let elapsed = start.elapsed().as_millis();
            if let Ok(body) = resp.text().await {
                if let Ok(status) = serde_json::from_str::<ServerStatus>(&body) {
                    view.apply_status(status, elapsed);
                } else {
                    view.ping_ms = Some(elapsed);
                    view.reachable = true;
                }
            }
        }
        Err(_) => {
            view.reachable = false;
            view.ping_ms = None;
        }
    }
    view
}

pub async fn download_binary(dest_path: &str) -> Result<(), String> {
    let url = download_url();

    if let Some(parent) = std::path::Path::new(dest_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create dir: {e}"))?;
        }
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Read failed: {e}"))?;

    std::fs::write(dest_path, &bytes).map_err(|e| format!("Write failed: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(dest_path, perms)
            .map_err(|e| format!("chmod failed: {e}"))?;
    }

    Ok(())
}
