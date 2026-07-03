use std::collections::{HashMap, HashSet};

/// Strip `.md`, trim, and normalize separators so targets compare against ids.
pub fn normalize_target(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_suffix(".md").unwrap_or(t);
    t.replace('\\', "/")
}

/// basename -> ids sharing it, for Obsidian shortest-path link resolution.
pub fn basename_map<'a, I: IntoIterator<Item = &'a String>>(ids: I) -> HashMap<&'a str, Vec<&'a str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in ids {
        let base = id.rsplit('/').next().unwrap_or(id);
        map.entry(base).or_default().push(id);
    }
    map
}

/// Resolve a raw wikilink target against the live id set: exact id match,
/// then unique-basename match. Ambiguous or missing targets stay unresolved
/// rather than guessed.
pub fn resolve_target(
    raw: &str,
    ids: &HashSet<String>,
    basenames: &HashMap<&str, Vec<&str>>,
) -> (String, bool) {
    let norm = normalize_target(raw);
    if ids.contains(&norm) {
        return (norm, true);
    }
    let base = norm.rsplit('/').next().unwrap_or(&norm);
    if let Some(matches) = basenames.get(base)
        && matches.len() == 1
    {
        return (matches[0].to_string(), true);
    }
    (norm, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (HashSet<String>, Vec<String>) {
        let ids: Vec<String> = [
            "blogs/context-engineering",
            "repos/smolbren",
            "Journal/2026, June 01",
            "a/dup",
            "b/dup",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        (ids.iter().cloned().collect(), ids)
    }

    #[test]
    fn exact_match() {
        let (set, ids) = setup();
        let bm = basename_map(&ids);
        assert_eq!(resolve_target("repos/smolbren", &set, &bm), ("repos/smolbren".into(), true));
    }

    #[test]
    fn unique_basename_match() {
        let (set, ids) = setup();
        let bm = basename_map(&ids);
        assert_eq!(resolve_target("smolbren", &set, &bm), ("repos/smolbren".into(), true));
    }

    #[test]
    fn ambiguous_basename_unresolved() {
        let (set, ids) = setup();
        let bm = basename_map(&ids);
        assert_eq!(resolve_target("dup", &set, &bm), ("dup".into(), false));
    }

    #[test]
    fn missing_unresolved() {
        let (set, ids) = setup();
        let bm = basename_map(&ids);
        assert_eq!(resolve_target("nope/nothing", &set, &bm), ("nope/nothing".into(), false));
    }

    #[test]
    fn md_suffix_stripped() {
        let (set, ids) = setup();
        let bm = basename_map(&ids);
        assert_eq!(resolve_target("repos/smolbren.md", &set, &bm), ("repos/smolbren".into(), true));
    }
}
