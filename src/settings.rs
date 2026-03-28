use std::fs;
use std::path::PathBuf;
use crate::models::AppSettings;

fn config_path() -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("config.json")
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
