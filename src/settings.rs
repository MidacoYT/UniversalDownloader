//! Réglages utilisateur persistants (%APPDATA%\UniversalDownloader\settings.json).
use crate::util::default_folder;
use std::path::PathBuf;

const KEY_LIBRARY: &str = "library_path";
const KEY_THEME: &str = "theme";
const KEY_LANGUAGE: &str = "language";

pub struct Settings {
    pub library_dir: String,
    pub light_theme: bool,
    pub french: bool,
    pub status: String,
}

impl Settings {
    pub fn load() -> Self {
        Self {
            library_dir: load_value(KEY_LIBRARY).unwrap_or_else(|| default_folder("Xbox Games")),
            light_theme: load_value(KEY_THEME).as_deref() == Some("light"),
            french: load_value(KEY_LANGUAGE).as_deref() == Some("fr"),
            status: String::new(),
        }
    }

    /// Écrit les réglages sans message de confirmation.
    pub fn persist(&self) {
        save_value(KEY_LIBRARY, &self.library_dir);
        save_value(KEY_THEME, if self.light_theme { "light" } else { "dark" });
        save_value(KEY_LANGUAGE, if self.french { "fr" } else { "en" });
    }

    pub fn save(&mut self) {
        self.persist();
        self.status = "Settings saved.".into();
    }

    pub fn reset(&mut self) {
        self.library_dir = default_folder("Xbox Games");
        self.light_theme = false;
        self.french = false;
        self.persist();
        self.status = "Default folder restored.".into();
    }
}

fn settings_file() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("UniversalDownloader").join("settings.json"))
}

fn load_value(key: &str) -> Option<String> {
    let data = std::fs::read(settings_file()?).ok()?;
    serde_json::from_slice::<serde_json::Value>(&data)
        .ok()?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

fn save_value(key: &str, value: &str) {
    let Some(file) = settings_file() else { return };
    let Some(parent) = file.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut json = std::fs::read(&file)
        .ok()
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    json.insert(key.into(), serde_json::Value::String(value.into()));
    if let Ok(data) = serde_json::to_vec_pretty(&serde_json::Value::Object(json)) {
        let _ = std::fs::write(file, data);
    }
}
