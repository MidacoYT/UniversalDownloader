//! Modèle de données du catalogue.

pub const ALL_PLATFORMS: &str = "All platforms";
pub const ALL_GENRES: &str = "All genres";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum GameStore {
    #[default]
    Xbox,
    Epic,
}

#[derive(Clone, Default)]
pub struct Game {
    pub store: GameStore,
    pub id: String,
    pub name: String,
    pub image: String,
    pub hero: String,
    pub developer: String,
    pub publisher: String,
    pub publisher_website: String,
    pub support_uri: String,
    pub language: String,
    pub last_modified: String,
    pub manifest_url: String,
    pub cdn_bases: Vec<String>,
    pub launch_exe: String,
    pub egdata_url: String,
    pub release: String,
    pub release_date_sort: String,
    pub date_added: String,
    pub size: String,
    pub description: String,
    pub long_description: String,
    pub egdata_offer_id: String,
    pub critic_score: Option<u8>,
    pub critic_recommend: Option<u8>,
    pub epic_stats_requested: bool,
    pub epic_stats_loaded: bool,
    pub platform: String,
    pub genres: Vec<String>,
    pub categories: Vec<String>,
    pub rating: String,
    pub players: String,
    pub rating_count: u64,
    pub achievements: Option<u64>,
    pub gamerscore: Option<u64>,
    pub screenshots: Vec<String>,
    pub trailers: Vec<Trailer>,
    pub attributes: Vec<GameAttribute>,
    pub ea_play: bool,
    /// Texte de recherche en minuscules, précalculé.
    pub search_key: String,
}

#[derive(Clone, Default)]
pub struct Trailer {
    pub caption: String,
    pub preview: String,
    pub url: String,
}

#[derive(Clone, Default)]
pub struct GameAttribute {
    pub name: String,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FeatureGroup {
    Multiplayer,
    Graphics,
    Compatibility,
    XboxLive,
}

impl FeatureGroup {
    pub const ALL: [Self; 4] = [
        Self::Multiplayer,
        Self::Graphics,
        Self::Compatibility,
        Self::XboxLive,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Multiplayer => "Multiplayer",
            Self::Graphics => "Graphics",
            Self::Compatibility => "Compatibility",
            Self::XboxLive => "Xbox Live",
        }
    }
}

impl GameAttribute {
    pub fn label(&self) -> String {
        let label = match self.name.as_str() {
            "SinglePlayer" => "Single-player",
            "XblOnlineCoop" => "Online co-op",
            "XblOnlineMultiPlayer" => "Online multiplayer",
            "XblCrossPlatformCoop" => "Cross-platform co-op",
            "XblCrossPlatformMultiPlayer" => "Cross-platform multiplayer",
            "XboxLiveCrossGenMP" => "Cross-gen multiplayer",
            "Capability4k" => "4K resolution",
            "CapabilityHDR" => "HDR",
            "CapabilityXboxEnhanced" => "Xbox enhanced",
            "ConsoleGen9Optimized" => "Optimized for Series X|S",
            "ConsoleCrossGen" => "Cross-gen compatible",
            "PcGamePad" => "PC gamepad",
            "XPA" => "Xbox Play Anywhere",
            "XblAchievements" => "Xbox achievements",
            "XblCloudSaves" => "Cloud saves",
            "XboxLive" => "Xbox Live",
            other => other,
        };
        match (self.min, self.max) {
            (Some(min), Some(max)) if min != max => format!("{label} ({min}–{max})"),
            (Some(min), _) if min > 1 => format!("{label} ({min})"),
            _ => label.to_owned(),
        }
    }

    pub fn group(&self) -> FeatureGroup {
        match self.name.as_str() {
            "SinglePlayer"
            | "XblOnlineCoop"
            | "XblOnlineMultiPlayer"
            | "XblCrossPlatformCoop"
            | "XblCrossPlatformMultiPlayer"
            | "XboxLiveCrossGenMP" => FeatureGroup::Multiplayer,
            "Capability4k"
            | "CapabilityHDR"
            | "CapabilityXboxEnhanced"
            | "ConsoleGen9Optimized"
            | "60fps"
            | "120fps" => FeatureGroup::Graphics,
            "ConsoleCrossGen" | "PcGamePad" | "XPA" | "OptimizedForSeriesX" => {
                FeatureGroup::Compatibility
            }
            _ => FeatureGroup::XboxLive,
        }
    }
}

impl Game {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            "Xbox game"
        } else {
            &self.name
        }
    }

    pub fn refresh_search_key(&mut self) {
        self.search_key = format!(
            "{} {} {} {} {} {}",
            self.name,
            self.id,
            self.publisher,
            self.developer,
            self.genres.join(" "),
            self.categories.join(" ")
        )
        .to_lowercase();
    }

    pub fn matches_platform(&self, filter: &str, unknown_ok: bool) -> bool {
        if filter == ALL_PLATFORMS {
            return true;
        }
        if self.platform.is_empty() {
            return unknown_ok;
        }
        match filter {
            "PC" => self.platform.contains("Windows"),
            "Xbox" => self.platform.contains("Xbox"),
            "Cloud" => self.platform.contains("Cloud"),
            _ => true,
        }
    }

    pub fn matches_genre(&self, filter: &str) -> bool {
        let filter = filter.to_lowercase();
        filter == ALL_GENRES.to_lowercase()
            || self
                .genres
                .iter()
                .chain(self.categories.iter())
                .any(|genre| genre.to_lowercase().contains(&filter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_key_is_lowercase_and_complete() {
        let mut g = Game {
            id: "X1".into(),
            name: "Halo Infinite".into(),
            genres: vec!["Shooter".into()],
            ..Default::default()
        };
        g.refresh_search_key();
        assert!(g.search_key.contains("halo infinite") && g.search_key.contains("shooter"));
    }

    #[test]
    fn platform_filter_keeps_unknown_while_loading() {
        let g = Game::default();
        assert!(g.matches_platform("PC", true));
        assert!(!g.matches_platform("PC", false));
    }

    #[test]
    fn attribute_labels_show_ranges() {
        let a = GameAttribute {
            name: "XblOnlineCoop".into(),
            min: Some(2),
            max: Some(4),
        };
        assert_eq!(a.label(), "Online co-op (2–4)");
        assert!(a.group() == FeatureGroup::Multiplayer);
    }
}
