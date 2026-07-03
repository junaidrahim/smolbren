use std::sync::LazyLock;

use regex::Regex;

/// `[[target]]`, `[[target|alias]]`, `[[target#heading]]`, `[[target#heading|alias]]`,
/// `[[target^block]]` — anchors are stripped, aliases captured.
static WIKILINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[([^\]|#^]+)(?:[#^][^\]|]*)?(?:\|([^\]]+))?\]\]").expect("valid regex")
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    pub target: String,
    pub alias: Option<String>,
}

pub fn extract(text: &str) -> Vec<WikiLink> {
    WIKILINK_RE
        .captures_iter(text)
        .filter_map(|c| {
            let target = c.get(1)?.as_str().trim();
            if target.is_empty() {
                return None;
            }
            Some(WikiLink {
                target: target.to_string(),
                alias: c.get(2).map(|m| m.as_str().trim().to_string()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(target: &str, alias: Option<&str>) -> WikiLink {
        WikiLink { target: target.to_string(), alias: alias.map(str::to_string) }
    }

    #[test]
    fn plain_link() {
        assert_eq!(extract("[[projects/prism]]"), vec![link("projects/prism", None)]);
    }

    #[test]
    fn aliased_link() {
        assert_eq!(extract("[[orgs/junaid-foo|Foo Org]]"), vec![link("orgs/junaid-foo", Some("Foo Org"))]);
    }

    #[test]
    fn heading_anchor_stripped() {
        assert_eq!(extract("[[blogs/post#Section]]"), vec![link("blogs/post", None)]);
        assert_eq!(extract("[[blogs/post#Section|see this]]"), vec![link("blogs/post", Some("see this"))]);
    }

    #[test]
    fn block_anchor_stripped() {
        assert_eq!(extract("[[Journal/2026, June 01^abc123]]"), vec![link("Journal/2026, June 01", None)]);
    }

    #[test]
    fn multiple_links_in_one_string() {
        assert_eq!(
            extract("see [[a]] and [[b|B]]"),
            vec![link("a", None), link("b", Some("B"))]
        );
    }

    #[test]
    fn spaces_and_commas_in_target() {
        assert_eq!(extract("[[Journal/2026, June 01]]"), vec![link("Journal/2026, June 01", None)]);
    }

    #[test]
    fn non_links_ignored() {
        assert!(extract("draft").is_empty());
        assert!(extract("").is_empty());
        assert!(extract("[not a wikilink](url)").is_empty());
        assert!(extract("[[]]").is_empty());
    }
}
