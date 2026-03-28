use serde::{Deserialize, Serialize};

/// A server entry from the JSON list
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ServerEntry {
    pub ip: Option<String>,
    pub port: Option<serde_json::Value>,
    pub flag: Option<String>,
    #[serde(rename = "type")]
    pub server_type: Option<String>,
    pub hidden: Option<serde_json::Value>,
}

impl ServerEntry {
    pub fn ip_str(&self) -> String {
        self.ip.clone().unwrap_or_default()
    }

    pub fn port_str(&self) -> String {
        match &self.port {
            Some(serde_json::Value::Number(n)) => n.to_string(),
            Some(serde_json::Value::String(s)) => s.clone(),
            _ => String::new(),
        }
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.ip_str(), self.port_str())
    }

    pub fn type_str(&self) -> String {
        self.server_type.clone().unwrap_or_else(|| "rust".into())
    }

    pub fn status_url(&self) -> String {
        if self.type_str() == "dotnet" {
            format!("http://{}/", self.addr())
        } else {
            format!("http://{}/info", self.addr())
        }
    }

    pub fn is_hidden(&self) -> bool {
        match &self.hidden {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s == "true",
            _ => false,
        }
    }
}

/// Parsed live status from a server.
/// Handles all three backend types:
///   rust/node  → GET /info  → { "online": N, "idle": M, "version": "..." }
///   dotnet     → GET /      → { "clientCount": N }
#[derive(Deserialize, Clone, Debug, Default)]
pub struct ServerStatus {
    /// rust + node servers
    pub online: Option<u32>,
    pub idle: Option<u32>,
    pub version: Option<String>,
    /// dotnet servers use "clientCount" instead of "online"
    #[serde(rename = "clientCount")]
    pub client_count: Option<u32>,
}

/// A server with live state attached
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
        // dotnet uses clientCount, rust/node use online
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

/// Settings stored to disk
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub server_list_url: String,
    pub http_timeout_ms: u64,
    pub client_dir: String,
    pub param_mode: ParamMode,
    pub custom_params: String,
    /// Run lan-play via sudo so it can access raw network interfaces (Linux only).
    #[cfg(not(target_os = "windows"))]
    #[serde(default)]
    pub privileged: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum ParamMode {
    Default,
    Acnh,
    Custom,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            server_list_url: "https://raw.githubusercontent.com/GreatWizard/lan-play-status/master/src/data/servers.json".into(),
            http_timeout_ms: 500,
            client_dir: default_client_dir(),
            param_mode: ParamMode::Default,
            custom_params: String::new(),
            #[cfg(not(target_os = "windows"))]
            privileged: true,
        }
    }
}

impl AppSettings {
    pub fn build_params(&self) -> String {
        match self.param_mode {
            ParamMode::Default => String::new(),
            ParamMode::Acnh => "--pmtu 500".into(),
            ParamMode::Custom => self.custom_params.clone(),
        }
    }

    pub fn client_binary(&self) -> String {
        #[cfg(target_os = "windows")]
        let bin = "lan-play-win64.exe";
        #[cfg(not(target_os = "windows"))]
        let bin = "lan-play";

        if self.client_dir.is_empty() {
            bin.into()
        } else {
            format!("{}/{}", self.client_dir.trim_end_matches('/').trim_end_matches('\\'), bin)
        }
    }
}

fn default_client_dir() -> String {
    #[cfg(target_os = "windows")]
    return "C:\\YaSLP-GUI".into();
    #[cfg(not(target_os = "windows"))]
    {
        dirs::home_dir()
            .map(|p| format!("{}/lan-play", p.display()))
            .unwrap_or_else(|| "/opt/lan-play".into())
    }
}
