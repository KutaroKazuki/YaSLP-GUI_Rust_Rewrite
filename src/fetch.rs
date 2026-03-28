use std::io;
use std::time::{Duration, Instant};
use reqwest::blocking::Client;
use serde_json::Value;
use crate::models::{AppSettings, Server, ServerEntry, ServerStatus};

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

fn parse_server_list(body: &str) -> Result<Vec<ServerEntry>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("JSON parse error: {e}"))?;

    // Format 1: [{"servers": [...]}]
    if let Some(arr) = v.as_array() {
        if let Some(first) = arr.first() {
            if let Some(servers) = first.get("servers") {
                if let Ok(list) = serde_json::from_value::<Vec<ServerEntry>>(servers.clone()) {
                    return Ok(list);
                }
            }
        }
        // Format 2: flat array [{ip, port, ...}, ...]
        if let Ok(list) = serde_json::from_value::<Vec<ServerEntry>>(v.clone()) {
            return Ok(list);
        }
    }

    // Format 3: {"servers": [...]}
    if let Some(servers) = v.get("servers") {
        if let Ok(list) = serde_json::from_value::<Vec<ServerEntry>>(servers.clone()) {
            return Ok(list);
        }
    }

    Err("Unrecognized server list format".into())
}

/// Download the lan-play binary for the current platform to `dest_path`.
/// The directory is created if it does not exist.
/// On Unix, the file is made executable after writing.
pub fn download_binary(dest_path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let url = "https://github.com/spacemeowx2/switch-lan-play/releases/latest/download/lan-play-win64.exe";
    #[cfg(target_os = "macos")]
    let url = "https://github.com/spacemeowx2/switch-lan-play/releases/latest/download/lan-play-macos";
    #[cfg(all(not(any(target_os = "windows", target_os = "macos")), any(target_arch = "aarch64", target_arch = "arm")))]
    let url = "https://github.com/metehankaygsz/lan-play-arm/releases/latest/download/lan-play-arm";
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_arch = "aarch64", target_arch = "arm")))]
    let url = "https://github.com/spacemeowx2/switch-lan-play/releases/latest/download/lan-play-linux";

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
