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
        let file = read_settings();
        let value = |key: &str| file.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        Self {
            library_dir: value(KEY_LIBRARY).unwrap_or_else(|| default_folder("Xbox Games")),
            light_theme: value(KEY_THEME).as_deref() == Some("light"),
            french: value(KEY_LANGUAGE).as_deref() == Some("fr"),
            status: String::new(),
        }
    }

    /// Écrit les réglages sans message de confirmation (un seul cycle lecture/écriture).
    pub fn persist(&self) {
        write_settings(&[
            (KEY_LIBRARY, self.library_dir.as_str()),
            (KEY_THEME, if self.light_theme { "light" } else { "dark" }),
            (KEY_LANGUAGE, if self.french { "fr" } else { "en" }),
        ]);
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

/// Contenu actuel du fichier de réglages (objet vide s'il est absent ou illisible).
fn read_settings() -> serde_json::Map<String, serde_json::Value> {
    settings_file()
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|data| serde_json::from_slice::<serde_json::Value>(&data).ok())
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

fn write_settings(values: &[(&str, &str)]) {
    let Some(file) = settings_file() else { return };
    let Some(parent) = file.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut json = read_settings();
    for (key, value) in values {
        json.insert((*key).into(), serde_json::Value::String((*value).into()));
    }
    if let Ok(data) = serde_json::to_vec_pretty(&serde_json::Value::Object(json)) {
        let _ = std::fs::write(file, data);
    }
}
