use std::cmp::Ordering;

use crate::model::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    ExactName = 0,
    PrefixName = 1,
    ContainsName = 2,
    FuzzyName = 3,
    Keyword = 4,
    Description = 5,
}

pub fn filter_entries<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let query = query.trim();
    if query.is_empty() {
        return entries.iter().collect();
    }

    let needle = query.to_lowercase();
    let mut scored: Vec<(Rank, usize, &Entry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| rank_entry(entry, &needle).map(|rank| (rank, index, entry)))
        .collect();

    scored.sort_by(|a, b| match a.0.cmp(&b.0) {
        Ordering::Equal => a.1.cmp(&b.1),
        other => other,
    });
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

fn rank_entry(entry: &Entry, needle: &str) -> Option<Rank> {
    let name = entry.name.to_lowercase();
    if name == needle {
        return Some(Rank::ExactName);
    }
    if name.starts_with(needle) {
        return Some(Rank::PrefixName);
    }
    if name.contains(needle) {
        return Some(Rank::ContainsName);
    }
    if fuzzy_match(&name, needle) {
        return Some(Rank::FuzzyName);
    }
    if entry
        .keywords
        .iter()
        .any(|keyword| keyword.to_lowercase().contains(needle))
    {
        return Some(Rank::Keyword);
    }
    if entry
        .description
        .as_deref()
        .is_some_and(|description| description.to_lowercase().contains(needle))
    {
        return Some(Rank::Description);
    }
    None
}

fn fuzzy_match(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    for needle_char in needle.chars() {
        loop {
            match chars.next() {
                Some(next) if next == needle_char => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Action;
    use std::path::PathBuf;

    fn entry(name: &str, keywords: &[&str], description: Option<&str>) -> Entry {
        Entry {
            id: name.to_string(),
            name: name.to_string(),
            description: description.map(str::to_string),
            icon: None,
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            hotkey: None,
            action: Action::Url {
                url: "https://example.com".into(),
            },
        }
    }

    #[test]
    fn empty_query_returns_all() {
        let entries = vec![entry("Steam", &[], None), entry("Cursor", &[], None)];
        assert_eq!(filter_entries(&entries, "").len(), 2);
    }

    #[test]
    fn prefix_beats_contains() {
        let entries = vec![
            entry("Extra Steam", &[], None),
            entry("Steam", &[], None),
            entry("SteamCMD", &[], None),
        ];
        let names: Vec<_> = filter_entries(&entries, "ste")
            .into_iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(names, ["Steam", "SteamCMD", "Extra Steam"]);
    }

    #[test]
    fn matches_keywords_and_description() {
        let entries = vec![
            entry("Downloads", &["folder"], None),
            entry("Notes", &[], Some("quick capture")),
        ];
        assert_eq!(filter_entries(&entries, "folder")[0].name, "Downloads");
        assert_eq!(filter_entries(&entries, "capture")[0].name, "Notes");
    }

    #[allow(dead_code)]
    fn _path() -> PathBuf {
        PathBuf::from("C:\\")
    }
}
