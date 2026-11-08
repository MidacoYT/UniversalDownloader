//! Fonctions utilitaires sans état.
use std::cmp::Ordering;
use std::path::Path;

pub const GIB: f64 = 1_073_741_824.0;

pub fn format_size(bytes: u64) -> String {
    if bytes >= GIB as u64 {
        format!("{:.1} GB", bytes as f64 / GIB)
    } else if bytes >= 1_000_000 {
        format!("{:.0} MB", bytes as f64 / 1_000_000.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// Convertit « 12.3 GB » / « 800 MB » en Go (0.0 si illisible).
pub fn size_in_gb(value: &str) -> f64 {
    let mut parts = value.split_whitespace();
    let Some(amount) = parts.next().and_then(|p| p.parse::<f64>().ok()) else {
        return 0.0;
    };
    match parts.next().unwrap_or("GB").to_ascii_uppercase().as_str() {
        "MB" => amount / 1024.0,
        "KB" => amount / (1024.0 * 1024.0),
        _ => amount,
    }
}

/// Les `k` meilleurs éléments selon `cmp` (le « plus petit » en premier), en O(n·log k)
/// et sans trier toute la collection. Stable : à égalité, l'ordre d'origine est conservé.
pub fn top_k<T>(
    items: impl IntoIterator<Item = T>,
    k: usize,
    mut cmp: impl FnMut(&T, &T) -> Ordering,
) -> Vec<T> {
    let mut best: Vec<T> = Vec::with_capacity(k + 1);
    if k == 0 {
        return best;
    }
    for item in items {
        let pos = best.partition_point(|kept| cmp(kept, &item) != Ordering::Greater);
        if pos < k {
            best.insert(pos, item);
            best.truncate(k);
        }
    }
    best
}

pub fn format_count(count: u64) -> String {
    let mut out = String::new();
    for (index, digit) in count.to_string().chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out.chars().rev().collect()
}

/// « 2024-03-07T… » → « Mar, 7, 2024 ».
pub fn format_date(value: &str) -> String {
    let date = value.split('T').next().unwrap_or(value);
    let mut parts = date.split('-');
    let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next()) else {
        return value.to_owned();
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if year.len() != 4 || day.len() != 2 || !digits(year) || !digits(day) {
        return value.to_owned();
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(name) = month
        .parse::<usize>()
        .ok()
        .and_then(|m| m.checked_sub(1))
        .and_then(|m| MONTHS.get(m))
    else {
        return value.to_owned();
    };
    format!("{name}, {}, {year}", day.trim_start_matches('0'))
}

/// Minuscules alphanumériques uniquement (comparaison de titres).
pub fn normalize_title(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Epic utilise des dates lointaines (ex. 2099-01-20) pour « date inconnue / non annoncée ».
pub fn is_known_release(value: &str) -> bool {
    value
        .get(..4)
        .and_then(|year| year.parse::<u32>().ok())
        .is_some_and(|year| (1970..2090).contains(&year))
}

pub fn default_folder(name: &str) -> String {
    std::env::var("USERPROFILE")
        .map(|profile| format!("{profile}\\{name}"))
        .unwrap_or_else(|_| format!("C:\\{name}"))
}

pub fn open_store_page(id: &str) {
    let _ = webbrowser::open(&format!("https://www.xbox.com/games/store/game/{id}"));
}

pub fn open_folder(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_formatted() {
        assert_eq!(format_date("2024-03-07T10:00:00Z"), "Mar, 7, 2024");
        assert_eq!(format_date("garbage"), "garbage");
    }

    #[test]
    fn placeholder_release_dates_are_unknown() {
        assert!(is_known_release("2024-03-07T10:00:00Z"));
        assert!(!is_known_release("2099-01-20T00:00:00Z"));
        assert!(!is_known_release(""));
    }

    #[test]
    fn counts_get_separators() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(12), "12");
    }

    #[test]
    fn top_k_keeps_best_in_order_and_is_stable() {
        let items = [(3, 'a'), (9, 'b'), (9, 'c'), (1, 'd'), (7, 'e')];
        let best = top_k(items, 3, |a, b| b.0.cmp(&a.0));
        assert_eq!(best, vec![(9, 'b'), (9, 'c'), (7, 'e')]);
        assert!(top_k(items, 0, |a, b| b.0.cmp(&a.0)).is_empty());
        assert_eq!(top_k(items, 10, |a, b| b.0.cmp(&a.0)).len(), 5);
    }

    #[test]
    fn sizes_are_converted() {
        assert_eq!(size_in_gb("2.5 GB"), 2.5);
        assert_eq!(size_in_gb("1024 MB"), 1.0);
        assert_eq!(size_in_gb("-"), 0.0);
    }
}
