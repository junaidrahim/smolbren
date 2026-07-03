pub mod frontmatter;
pub mod wikilink;

/// An outward edge extracted from a frontmatter key, before target resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEdge {
    pub edge_type: String,
    pub to_raw: String,
    pub to_alias: Option<String>,
    pub position: i32,
}

#[derive(Debug)]
pub struct ParsedContent {
    pub id: String,
    pub path: String,
    pub note_type: Option<String>,
    pub title: String,
    pub frontmatter_json: String,
    pub body: String,
    pub frontmatter_hash: String,
    pub content_hash: String,
    pub edges: Vec<RawEdge>,
    pub warnings: Vec<String>,
}

/// Note id = vault-relative path, `/`-normalized, `.md` stripped. This is the
/// same shape wikilink targets use (`[[blogs/context-engineering]]`).
pub fn note_id(rel_path: &str) -> String {
    let p = rel_path.replace('\\', "/");
    p.strip_suffix(".md").unwrap_or(&p).to_string()
}

pub fn parse_note(rel_path: &str, content: &str) -> ParsedContent {
    let id = note_id(rel_path);
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let mut warnings = Vec::new();

    let (raw_fm, body) = frontmatter::split(content).unwrap_or(("", content));
    let frontmatter_hash = blake3::hash(raw_fm.as_bytes()).to_hex().to_string();

    let mapping = match frontmatter::parse(raw_fm) {
        Ok(m) => m,
        Err(e) => {
            warnings.push(format!("{rel_path}: bad frontmatter ({e}); indexing body only"));
            serde_yaml_ng::Mapping::new()
        }
    };

    let note_type = mapping.get("type").and_then(|v| v.as_str()).map(str::to_string);

    let title = body
        .lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(&id).to_string());

    // Every frontmatter key (except `type`) whose string values contain
    // wikilinks becomes an edge type; non-link values never do.
    let mut edges = Vec::new();
    for (k, v) in &mapping {
        let Some(key) = k.as_str() else { continue };
        if key == "type" {
            continue;
        }
        let candidates: Vec<&str> = match v {
            serde_yaml_ng::Value::String(s) => vec![s.as_str()],
            serde_yaml_ng::Value::Sequence(seq) => seq.iter().filter_map(|x| x.as_str()).collect(),
            _ => continue,
        };
        let mut position = 0i32;
        for s in candidates {
            for link in wikilink::extract(s) {
                edges.push(RawEdge {
                    edge_type: key.to_string(),
                    to_raw: link.target,
                    to_alias: link.alias,
                    position,
                });
                position += 1;
            }
        }
    }

    let frontmatter_json = serde_json::Value::Object(
        mapping
            .iter()
            .map(|(k, v)| (frontmatter::key_string(k), frontmatter::yaml_to_json(v)))
            .collect(),
    )
    .to_string();

    ParsedContent {
        id,
        path: rel_path.replace('\\', "/"),
        note_type,
        title,
        frontmatter_json,
        body: body.to_string(),
        frontmatter_hash,
        content_hash,
        edges,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"---
type: blog
created: 2026-05-10
updated: 2026-06-30
status: draft
for: "[[orgs/junaid-foo]]"
about: []
mentions: ["[[projects/prism]]", "[[repos/smolbren]]", "[[blogs/context-development-lifecycle]]", "[[blogs/context-platform-engineering]]"]
merged_from: ["[[blogs/context-development-lifecycle]]", "[[blogs/context-platform-engineering]]"]
derives_from: ["[[Journal/2026, June 01]]", "[[Journal/2026, June 04]]"]
published_url: ""
published_at:
---

# Context engineering

Draft thesis: prompt engineering changes the instruction.
"#;

    #[test]
    fn parses_readme_sample() {
        let note = parse_note("blogs/context-engineering.md", SAMPLE);
        assert_eq!(note.id, "blogs/context-engineering");
        assert_eq!(note.note_type.as_deref(), Some("blog"));
        assert_eq!(note.title, "Context engineering");
        assert!(note.warnings.is_empty());

        let of = |t: &str| note.edges.iter().filter(|e| e.edge_type == t).count();
        assert_eq!(of("for"), 1);
        assert_eq!(of("mentions"), 4);
        assert_eq!(of("merged_from"), 2);
        assert_eq!(of("derives_from"), 2);
        // scalar/empty/date keys never become edges
        assert_eq!(of("status"), 0);
        assert_eq!(of("about"), 0);
        assert_eq!(of("created"), 0);
        assert_eq!(of("published_url"), 0);
        assert_eq!(of("published_at"), 0);

        let m: Vec<_> = note
            .edges
            .iter()
            .filter(|e| e.edge_type == "mentions")
            .map(|e| (e.to_raw.as_str(), e.position))
            .collect();
        assert_eq!(m[0], ("projects/prism", 0));
        assert_eq!(m[3], ("blogs/context-platform-engineering", 3));

        let fm: serde_json::Value = serde_json::from_str(&note.frontmatter_json).unwrap();
        assert_eq!(fm["status"], "draft");
        assert_eq!(fm["created"], "2026-05-10");
    }

    #[test]
    fn no_frontmatter_indexes_body() {
        let note = parse_note("inbox/scratch.md", "# Scratch\n\nsome text");
        assert_eq!(note.note_type, None);
        assert_eq!(note.title, "Scratch");
        assert_eq!(note.body, "# Scratch\n\nsome text");
        assert!(note.edges.is_empty());
        assert_eq!(note.frontmatter_json, "{}");
    }

    #[test]
    fn malformed_yaml_warns_but_indexes() {
        let note = parse_note("bad.md", "---\ntype: [unclosed\n---\nbody text");
        assert_eq!(note.note_type, None);
        assert_eq!(note.warnings.len(), 1);
        assert_eq!(note.body, "body text");
    }

    #[test]
    fn title_falls_back_to_filename_stem() {
        let note = parse_note("Journal/2026, June 01.md", "no heading here");
        assert_eq!(note.title, "2026, June 01");
    }

    #[test]
    fn mixed_list_only_links_become_edges() {
        let note = parse_note(
            "n.md",
            "---\ntype: note\nrefs: [\"[[a]]\", \"plain string\", \"[[b]]\"]\n---\n",
        );
        let refs: Vec<_> = note.edges.iter().map(|e| e.to_raw.as_str()).collect();
        assert_eq!(refs, vec!["a", "b"]);
    }

    #[test]
    fn note_id_normalizes() {
        assert_eq!(note_id("blogs/post.md"), "blogs/post");
        assert_eq!(note_id("readme.txt.md"), "readme.txt");
        assert_eq!(note_id("no-extension"), "no-extension");
    }
}
