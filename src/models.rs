// Shared types live in yaslp-shared; re-export them so the rest of the GUI
// crate can still use `crate::models::AppSettings` etc. unchanged.
pub use yaslp_shared::{AppSettings, ParamMode, ServerEntry, ServerStatus};

/// A server with live state attached — GUI-specific display wrapper.
#[derive(Clone, Debug)]
pub struct Server {
    pub entry: ServerEntry,
    pub status: ServerStatus,
    pub ping_ms: Option<u128>,
    pub reachable: bool,
}

impl Server {
    pub fn from_entry(entry: ServerEntry) -> Self {
        Self {
            entry,
            status: ServerStatus::default(),
            ping_ms: None,
            reachable: false,
        }
    }

    pub fn online_count(&self) -> u32 {
        self.status.online
            .or(self.status.client_count)
            .unwrap_or(0)
    }

    pub fn idle_count(&self) -> u32 {
        self.status.idle.unwrap_or(0)
    }

    pub fn active_count(&self) -> u32 {
        self.online_count().saturating_sub(self.idle_count())
    }

    pub fn version(&self) -> &str {
        self.status.version.as_deref().unwrap_or("N/A")
    }

    pub fn ping_label(&self) -> String {
        match self.ping_ms {
            Some(ms) => format!("{ms} ms"),
            None => "—".into(),
        }
    }
}
