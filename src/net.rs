//! Client HTTP partagé.
use std::time::Duration;

pub const USER_AGENT: &str = "UniversalDownloader/0.1";

pub fn client(timeout_secs: u64) -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
        .build()
}
