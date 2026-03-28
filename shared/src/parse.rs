use serde_json::Value;
use crate::models::ServerEntry;

/// Parse a server list JSON body. Handles three formats:
///   1. `[{"servers": [...]}]`  — wrapped array
///   2. `[{ip, port, ...}, ...]` — flat array
///   3. `{"servers": [...]}`    — top-level object
pub fn parse_server_list(body: &str) -> Result<Vec<ServerEntry>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("JSON parse error: {e}"))?;

    if let Some(arr) = v.as_array() {
        if let Some(first) = arr.first() {
            if let Some(servers) = first.get("servers") {
                if let Ok(list) = serde_json::from_value::<Vec<ServerEntry>>(servers.clone()) {
                    return Ok(list);
                }
            }
        }
        if let Ok(list) = serde_json::from_value::<Vec<ServerEntry>>(v.clone()) {
            return Ok(list);
        }
    }

    if let Some(servers) = v.get("servers") {
        if let Ok(list) = serde_json::from_value::<Vec<ServerEntry>>(servers.clone()) {
            return Ok(list);
        }
    }

    Err("Unrecognized server list format".into())
}

/// Return the lan-play download URL for the current platform.
pub fn download_url() -> &'static str {
    #[cfg(target_os = "windows")]
    return "https://github.com/spacemeowx2/switch-lan-play/releases/latest/download/lan-play-win64.exe";
    #[cfg(target_os = "macos")]
    return "https://github.com/spacemeowx2/switch-lan-play/releases/latest/download/lan-play-macos";
    #[cfg(all(
        not(any(target_os = "windows", target_os = "macos")),
        any(target_arch = "aarch64", target_arch = "arm")
    ))]
    return "https://github.com/metehankaygsz/lan-play-arm/releases/latest/download/lan-play-arm";
    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_arch = "aarch64",
        target_arch = "arm"
    )))]
    return "https://github.com/spacemeowx2/switch-lan-play/releases/latest/download/lan-play-linux";
}
