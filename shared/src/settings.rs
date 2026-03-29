use std::fs;
use std::path::PathBuf;
use crate::models::AppSettings;

/// Returns the directory where config.json is stored.
/// This is the default client directory so that the config lives
/// alongside the lan-play binary on a default install.
fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from("C:\\YaSLP-GUI")
    }
    #[cfg(not(target_os = "windows"))]
    {
        dirs::home_dir()
            .map(|p| p.join(".config/YaSLP-GUI"))
            .unwrap_or_else(|| PathBuf::from("/opt/YaSLP-GUI"))
    }
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> AppSettings {
    let path = config_path();
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<AppSettings>(&data) {
            return cfg;
        }
    }
    AppSettings::default()
}

pub fn save(cfg: &AppSettings) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, json);
    }
}
