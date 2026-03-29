use std::fs;
use std::path::PathBuf;
use crate::models::AppSettings;

/// OS-standard location for the pointer file that records where the
/// client directory (and therefore config.json) lives.
///
/// Windows: %LOCALAPPDATA%\YaSLP-GUI\client_dir.txt
/// Linux:   ~/.local/share/YaSLP-GUI/client_dir
fn pointer_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir().map(|p| p.join("YaSLP-GUI").join("client_dir.txt"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        dirs::data_local_dir().map(|p| p.join("YaSLP-GUI").join("client_dir"))
    }
}

/// Default client directory used on first launch before any pointer is written.
fn default_client_dir() -> PathBuf {
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

/// Reads the pointer file and returns the stored client directory path,
/// or the default if the pointer does not exist yet.
fn resolve_client_dir() -> PathBuf {
    if let Some(ptr) = pointer_path() {
        if let Ok(s) = fs::read_to_string(&ptr) {
            let p = PathBuf::from(s.trim());
            if !p.as_os_str().is_empty() {
                return p;
            }
        }
    }
    default_client_dir()
}

/// Persists the client directory path to the OS pointer file.
fn write_pointer(client_dir: &PathBuf) {
    if let Some(ptr) = pointer_path() {
        if let Some(parent) = ptr.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&ptr, client_dir.to_string_lossy().as_bytes());
    }
}

fn config_path() -> PathBuf {
    resolve_client_dir().join("config.json")
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
    let client_dir = PathBuf::from(&cfg.client_dir);
    // Update the pointer so the next launch finds config.json in the right place.
    if !cfg.client_dir.is_empty() {
        write_pointer(&client_dir);
    }
    let path = client_dir.join("config.json");
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(path, json);
    }
}
