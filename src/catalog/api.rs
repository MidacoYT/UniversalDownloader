//! Accès réseau au catalogue (liste de jeux + détails Microsoft) et parsing JSON.
use super::{CatalogEvent, EPIC_API_BASE};
use crate::model::{Game, GameAttribute, GameStore, Trailer};
use crate::net;
use crate::util::{GIB, format_date};
use reqwest::blocking::Client;
use serde_json::Value;
use std::sync::mpsc::Sender;
use std::time::Duration;

const CATALOG_KEY: &str = "9eEo0ksYvZTsXgPiJUz7hQCio18hW+mPlQhy0UBq+10=";
const CATALOG_URLS: [&str; 2] = [
    "https://xboxapi.netlify.app/games",
    "https://raw.githubusercontent.com/MidacoYT/Games/refs/heads/main/games.json",
];
const DETAILS_URL: &str = "https://displaycatalog.mp.microsoft.com/v7.0/products";
const BATCH_SIZE: usize = 10;

/// Charge la liste puis enrichit chaque jeu par lots, dans un thread dédié.
pub fn spawn_refresh(tx: Sender<CatalogEvent>, store: GameStore) {
    std::thread::spawn(move || run(tx, store));
}

fn run(tx: Sender<CatalogEvent>, store: GameStore) {
    let client = match net::client(20) {
        Ok(client) => client,
        Err(error) => {
            let _ = tx.send(CatalogEvent::Done(Err(error.to_string())));
            return;
        }
    };
    let result = match store {
        GameStore::Xbox => download_catalog(&client),
        GameStore::Epic => download_epic_catalog(&client),
    };
    let mut games = match result {
        Ok(games) => games,
        Err(error) => {
            let _ = tx.send(CatalogEvent::Done(Err(error)));
            return;
        }
    };
    let _ = tx.send(CatalogEvent::Initial(games.clone()));
    let enriched = match store {
        GameStore::Xbox => enrich(&client, &mut games, &tx),
        GameStore::Epic => games.len(),
    };
    let _ = tx.send(CatalogEvent::Done(Ok(enriched)));
}

fn download_epic_catalog(client: &Client) -> Result<Vec<Game>, String> {
    const CATALOG_KEY: &str = "9eEo0ksYvZTsXgPiJUz7hQCio18hW+mPlQhy0UBq+10=";
    let data: Value = client
        .get(format!("{EPIC_API_BASE}/games"))
        .bearer_auth(CATALOG_KEY)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| format!("Could not load Epic catalog: {error}"))?;
    parse_epic_catalog(&data)
}

pub fn parse_epic_catalog(data: &Value) -> Result<Vec<Game>, String> {
    let entries = data
        .as_array()
        .or_else(|| data.get("games").and_then(Value::as_array))
        .ok_or("Epic API returned an invalid game list.")?;
    let mut seen = std::collections::HashSet::new();
    let mut games = Vec::new();
    for item in entries {
        let Some(id) = item.get("appName").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        let Some(name) = item.get("name").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if id.is_empty() || name.is_empty() || !seen.insert(id.to_owned()) {
            continue;
        }
        let mut game = Game {
            id: id.to_owned(),
            name: name.to_owned(),
            store: GameStore::Epic,
            platform: "Windows".into(),
            manifest_url: item
                .get("manifestUrl")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            image: item
                .get("image")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            hero: item
                .get("hero")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            developer: item
                .get("developer")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            publisher: item
                .get("publisher")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            description: item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            long_description: item
                .get("longDescription")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            egdata_offer_id: item
                .get("egdataOfferId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            launch_exe: item
                .get("launchExe")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            egdata_url: item
                .get("egdataUrl")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            cdn_bases: item
                .get("cdnBases")
                .and_then(Value::as_array)
                .map(|xs| {
                    xs.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            genres: item
                .get("genres")
                .and_then(Value::as_array)
                .map(|xs| {
                    xs.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            screenshots: item
                .get("screenshots")
                .and_then(Value::as_array)
                .map(|xs| {
                    xs.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        };
        if let Some(bytes) = item.get("installSize").and_then(Value::as_u64) {
            game.size = format!("{:.1} GB", bytes as f64 / GIB as f64);
        }
        if let Some(version) = item.get("buildVersion").and_then(Value::as_str) {
            game.last_modified = version.to_owned();
        }
        if let Some(release) = item.get("releaseDate").and_then(Value::as_str) {
            game.release = release.to_owned();
        }
        // Epic tags identify broad player modes, but not player limits or a
        // live concurrent-player count.
        let has_single_player = game.genres.iter().any(|tag| {
            tag.eq_ignore_ascii_case("single player") || tag.eq_ignore_ascii_case("single-player")
        });
        let has_multiplayer = game.genres.iter().any(|tag| {
            tag.eq_ignore_ascii_case("multiplayer") || tag.eq_ignore_ascii_case("co-op")
        });
        game.players = match (has_single_player, has_multiplayer) {
            (true, true) => "Single-player · Multiplayer".into(),
            (true, false) => "Single-player".into(),
            (false, true) => "Multiplayer".into(),
            (false, false) => String::new(),
        };
        game.rating = item
            .get("pegi")
            .or_else(|| item.get("ageRating"))
            .or_else(|| item.get("rating"))
            .and_then(|value| {
                value
                    .as_str()
                    .or_else(|| value.get("pegi").and_then(Value::as_str))
            })
            .unwrap_or_default()
            .to_owned();
        game.refresh_search_key();
        games.push(game);
    }
    games.sort_by_cached_key(|game| game.name.to_lowercase());
    if games.is_empty() {
        return Err("Epic API contains no valid game manifests.".into());
    }
    Ok(games)
}

/// Loads the public critic summary for one Epic offer on demand.
pub fn fetch_epic_critic_stats(offer_id: &str) -> Result<(Option<u8>, Option<u8>), String> {
    let client = net::client(20).map_err(|error| error.to_string())?;
    let data: Value = client
        .get(format!("https://api.egdata.app/offers/{offer_id}/ratings"))
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| format!("Could not load EGData ratings: {error}"))?;
    let score = data
        .get("criticAverage")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok());
    let recommend = data
        .get("recommendPercentage")
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok());
    Ok((score, recommend))
}

fn download_catalog(client: &Client) -> Result<Vec<Game>, String> {
    let mut errors = Vec::new();
    for (index, url) in CATALOG_URLS.iter().enumerate() {
        let mut request = client.get(*url);
        if index == 0 && !CATALOG_KEY.is_empty() {
            request = request.bearer_auth(CATALOG_KEY);
        }
        match request
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json::<Value>())
        {
            Ok(data) => match parse_catalog(&data) {
                Ok(games) => return Ok(games),
                Err(error) => errors.push(error),
            },
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }
    Err(errors.join("\n"))
}

fn enrich(client: &Client, games: &mut [Game], tx: &Sender<CatalogEvent>) -> usize {
    let mut enriched = 0;
    for batch in games.chunks_mut(BATCH_SIZE) {
        let jobs: Vec<_> = batch
            .iter()
            .enumerate()
            .map(|(index, game)| {
                let client = client.clone();
                let id = game.id.clone();
                std::thread::spawn(move || (index, fetch_details(&client, &id)))
            })
            .collect();
        let mut updates = Vec::new();
        for job in jobs {
            if let Ok((index, Some(product))) = job.join() {
                apply_details(&mut batch[index], &product);
                enriched += 1;
                updates.push(batch[index].clone());
            }
        }
        if !updates.is_empty() {
            let _ = tx.send(CatalogEvent::Updated(updates));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    enriched
}

// ───────────────────────── Parsing ─────────────────────────

fn catalog_items(data: &Value) -> Vec<&Value> {
    if let Some(array) = data.as_array() {
        array.iter().collect()
    } else if let Some(array) = data
        .get("Products")
        .or_else(|| data.get("items"))
        .and_then(Value::as_array)
    {
        array.iter().collect()
    } else if let Some(object) = data.as_object() {
        object.values().collect()
    } else {
        Vec::new()
    }
}

pub fn parse_catalog(data: &Value) -> Result<Vec<Game>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut games: Vec<Game> = catalog_items(data)
        .into_iter()
        .filter_map(|item| {
            let id = item
                .get("productId")
                .or_else(|| item.get("ProductId"))?
                .as_str()?
                .trim();
            if id.is_empty() || !seen.insert(id.to_owned()) {
                return None;
            }
            Some(Game {
                id: id.into(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or(id)
                    .into(),
                ..Default::default()
            })
        })
        .collect();
    games.sort_by_key(|g| g.name.to_lowercase());
    for game in &mut games {
        game.refresh_search_key();
    }
    if games.is_empty() {
        return Err("The catalog contains no usable games.".into());
    }
    Ok(games)
}

fn fetch_details(client: &Client, id: &str) -> Option<Value> {
    let response = client
        .get(DETAILS_URL)
        .query(&[
            ("bigIds", id),
            ("market", "US"),
            ("languages", "en-us"),
            ("MS-CV", "DGU1mcuYo0WMMp+F.1"),
        ])
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Value>()
        .ok()?;
    let product = response.get("Products")?.as_array()?.first()?;
    product.is_object().then(|| product.clone())
}

fn absolute_url(uri: &str) -> String {
    if uri.starts_with("http") {
        uri.to_owned()
    } else {
        format!("https:{uri}")
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_owned()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn image_for(images: &[Value], purpose: &str) -> String {
    images
        .iter()
        .find(|img| img["ImagePurpose"] == purpose)
        .and_then(|img| img["Uri"].as_str())
        .map(absolute_url)
        .unwrap_or_default()
}

/// Copie les informations détaillées d'un produit Microsoft dans `game`.
pub fn apply_details(game: &mut Game, product: &Value) {
    let localized = &product["LocalizedProperties"][0];
    let images = localized["Images"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if let Some(title) = localized["ProductTitle"].as_str() {
        game.name = title.to_owned();
    }
    game.image = image_for(images, "BoxArt");
    let hero = image_for(images, "SuperHeroArt");
    game.hero = if hero.is_empty() {
        image_for(images, "TitledHeroArt")
    } else {
        hero
    };
    game.developer = text(&localized["DeveloperName"]);
    game.publisher = text(&localized["PublisherName"]);
    game.publisher_website = text(&localized["PublisherWebsiteUri"]);
    game.support_uri = text(&localized["SupportUri"]);
    game.language = text(&localized["Language"]);
    game.last_modified = format_date(product["LastModifiedDate"].as_str().unwrap_or(""));
    game.date_added = product["DateAdded"].as_str().unwrap_or_default().to_owned();
    game.release = format_date(
        localized["ReleaseDate"]
            .as_str()
            .or(product["ReleaseDate"].as_str())
            .or(product["DisplaySkuAvailabilities"][0]["Availabilities"][0]["Properties"]
                ["OriginalReleaseDate"]
                .as_str())
            .unwrap_or(""),
    );
    game.description = localized["Description"]
        .as_str()
        .or(localized["ShortDescription"].as_str())
        .or(localized["ProductDescription"].as_str())
        .unwrap_or("")
        .to_owned();

    let size = product["DisplaySkuAvailabilities"][0]["Sku"]["Properties"]["Packages"][0]
        ["MaxDownloadSizeInBytes"]
        .as_u64()
        .or(product["InstallAttributes"][0]["Size"].as_u64());
    if let Some(bytes) = size {
        game.size = format!("{:.1} GB", bytes as f64 / GIB);
    }

    game.platform = strings(&product["Platforms"]).join(" · ");
    game.genres = strings(&localized["Genres"]);
    let categories = if product["Properties"]["Categories"].is_array() {
        &product["Properties"]["Categories"]
    } else {
        &localized["Categories"]
    };
    game.categories = strings(categories);

    // Une capture sur deux : l'API renvoie plusieurs résolutions de la même image.
    game.screenshots = images
        .iter()
        .filter(|img| img["ImagePurpose"] == "Screenshot")
        .step_by(2)
        .filter_map(|img| img["Uri"].as_str())
        .map(absolute_url)
        .collect();
    game.trailers = localized["CMSVideos"]
        .as_array()
        .map(|videos| videos.iter().map(parse_trailer).collect())
        .unwrap_or_default();
    game.attributes = product["Properties"]["Attributes"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item["Name"].as_str().map(|name| GameAttribute {
                        name: name.to_owned(),
                        min: item["Minimum"].as_u64(),
                        max: item["Maximum"].as_u64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    game.achievements = product["Achievements"]["AchievementCount"].as_u64();
    game.gamerscore = product["Achievements"]["GamerscoreTotal"].as_u64();
    game.rating_count = parse_rating_count(product);
    game.rating = parse_rating(localized, product);
    game.players = parse_players(product);
    game.ea_play = product["IsEAPlay"].as_bool().unwrap_or(false);
    game.refresh_search_key();
}

fn parse_trailer(video: &Value) -> Trailer {
    Trailer {
        caption: text(&video["Caption"]),
        preview: video["PreviewImage"]["Uri"]
            .as_str()
            .map(absolute_url)
            .unwrap_or_default(),
        url: video["DASH"]
            .as_str()
            .or(video["HLS"].as_str())
            .unwrap_or("")
            .to_owned(),
    }
}

fn parse_rating_count(product: &Value) -> u64 {
    product["MarketProperties"][0]["UsageData"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["AggregateTimeSpan"] == "AllTime")
                .or_else(|| items.first())
        })
        .and_then(|item| {
            item["RatingCount"]
                .as_u64()
                .or_else(|| item["RatingCount"].as_str().and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}

fn parse_rating(localized: &Value, product: &Value) -> String {
    let ratings = localized["ContentRatings"].as_array();
    let find =
        |system: &str| ratings.and_then(|items| items.iter().find(|r| r["RatingSystem"] == system));
    find("PEGI")
        .or_else(|| find("ESRB"))
        .and_then(|rating| {
            let system = rating["RatingSystem"].as_str()?;
            let value = rating["RatingId"].as_str()?;
            Some(if system == "PEGI" {
                if value.to_ascii_lowercase().contains("pegi") {
                    value.to_owned()
                } else {
                    format!("PEGI {value}")
                }
            } else {
                format!("ESRB {value}")
            })
        })
        .or_else(|| {
            let age = product["MarketProperties"][0]["MinimumUserAge"].as_u64()?;
            (age > 0).then(|| format!("PEGI {age}"))
        })
        .unwrap_or_default()
}

fn parse_players(product: &Value) -> String {
    let attributes = product["Properties"]["Attributes"].as_array();
    attributes
        .and_then(|items| items.iter().find(|item| item["Name"] == "XblOnlineCoop"))
        .map(|coop| {
            let min = coop["Minimum"].as_u64().unwrap_or(1);
            let max = coop["Maximum"].as_u64().unwrap_or(4);
            if min == max {
                format!("{min} player{}", if min > 1 { "s" } else { "" })
            } else {
                format!("{min}–{max} players")
            }
        })
        .or_else(|| attributes.map(|_| "1 player".to_owned()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_deduplicates_and_sorts() {
        let data = serde_json::json!([
            {"productId": "B", "name": "Zulu"},
            {"productId": "A", "name": "Alpha"},
            {"productId": "B"}
        ]);
        let games = parse_catalog(&data).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].id, "A");
    }

    #[test]
    fn catalog_accepts_products_shape() {
        let data = serde_json::json!({"Products": [{"ProductId": "ABC"}]});
        assert_eq!(parse_catalog(&data).unwrap()[0].name, "ABC");
    }

    #[test]
    fn empty_catalog_is_an_error() {
        assert!(parse_catalog(&serde_json::json!([])).is_err());
    }
}
