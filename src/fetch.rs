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
