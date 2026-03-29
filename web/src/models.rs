// Re-export shared types so the web crate can use `crate::models::AppSettings` etc.
pub use yaslp_shared::{AppSettings, ServerEntry, ServerStatus};

use serde::Serialize;

/// Serializable server view returned by the REST API.
/// Web-specific — the GUI uses its own `Server` wrapper instead.
#[derive(Serialize, Clone, Debug)]
pub struct ServerView {
    pub addr: String,
    pub server_type: String,
    pub flag: Option<String>,
    pub reachable: bool,
    pub online: Option<u32>,
    pub idle: Option<u32>,
    pub version: Option<String>,
    pub client_count: Option<u32>,
    pub ping_ms: Option<u128>,
}

impl ServerView {
    pub fn from_entry(entry: &ServerEntry) -> Self {
        Self {
            addr: entry.addr(),
            server_type: entry.type_str(),
            flag: entry.flag.clone(),
            reachable: false,
            online: None,
            idle: None,
            version: None,
            client_count: None,
            ping_ms: None,
        }
    }

    pub fn apply_status(&mut self, status: ServerStatus, ping_ms: u128) {
        self.online = status.online;
        self.idle = status.idle;
        self.version = status.version;
        self.client_count = status.client_count;
        self.ping_ms = Some(ping_ms);
        self.reachable = true;
    }

    pub fn status_url(&self) -> String {
        if self.server_type == "dotnet" {
            format!("http://{}/", self.addr)
        } else {
            format!("http://{}/info", self.addr)
        }
    }
}
