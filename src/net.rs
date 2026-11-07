//! Client HTTP partagé.
use std::sync::OnceLock;
use std::time::Duration;

pub const USER_AGENT: &str = "UniversalDownloader/0.1";

/// Jeton utilisé par les API du catalogue (défini une seule fois pour tout le projet).
pub const API_KEY: &str = "9eEo0ksYvZTsXgPiJUz7hQCio18hW+mPlQhy0UBq+10=";

pub fn client(timeout_secs: u64) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
        .build()
}

/// Client réutilisable (pool de connexions + TLS déjà négocié) pour les petites requêtes
/// répétées comme les jaquettes. `Client` est un `Arc` en interne : le cloner est gratuit.
pub fn shared_client() -> Option<reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT.get_or_init(|| client(12).ok()).clone()
}

/// Client réutilisé pour le chargement du catalogue : JSON volumineux + enrichissement
/// de nombreux jeux, il mérite un délai généreux et son propre pool de connexions.
pub fn shared_catalog_client() -> Option<reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT.get_or_init(|| client(60).ok()).clone()
}
