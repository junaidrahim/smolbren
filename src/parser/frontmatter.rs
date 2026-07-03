use serde_yaml_ng::{Mapping, Value};

/// Split a note into (raw frontmatter, body). The file must start with a
/// `---` fence line; returns None when there is no complete frontmatter block.
pub fn split(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\r\n")
        .or_else(|| content.strip_prefix("---\n"))?;

    // Empty frontmatter: the closing fence is the first line.
    if let Some(body) = rest.strip_prefix("---").and_then(fence_end) {
        return Some(("", body));
    }

    let mut from = 0;
    while let Some(pos) = rest[from..].find("\n---") {
        let i = from + pos;
        if let Some(body) = fence_end(&rest[i + 4..]) {
            return Some((&rest[..i], body));
        }
        from = i + 1;
    }
    None
}

/// `after` is the text following a `---`; a valid fence line ends with
/// a newline or EOF. Returns the body that follows.
fn fence_end(after: &str) -> Option<&str> {
    if after.is_empty() {
        Some("")
    } else if let Some(body) = after.strip_prefix("\r\n") {
        Some(body)
    } else {
        after.strip_prefix('\n')
    }
}

/// Parse raw frontmatter YAML into a mapping. Errors are returned as strings
/// so callers can warn and keep indexing the body.
pub fn parse(raw: &str) -> Result<Mapping, String> {
    if raw.trim().is_empty() {
        return Ok(Mapping::new());
    }
    match serde_yaml_ng::from_str::<Value>(raw) {
        Ok(Value::Mapping(m)) => Ok(m),
        Ok(_) => Err("frontmatter is not a mapping".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn yaml_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else if let Some(u) = n.as_u64() {
                u.into()
            } else {
                n.as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }
        }
        Value::String(s) => s.clone().into(),
        Value::Sequence(seq) => serde_json::Value::Array(seq.iter().map(yaml_to_json).collect()),
        Value::Mapping(m) => serde_json::Value::Object(
            m.iter().map(|(k, val)| (key_string(k), yaml_to_json(val))).collect(),
        ),
        Value::Tagged(t) => yaml_to_json(&t.value),
    }
}

pub fn key_string(k: &Value) -> String {
    match k {
        Value::String(s) => s.clone(),
        other => match yaml_to_json(other) {
            serde_json::Value::String(s) => s,
            v => v.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_simple_frontmatter() {
        let (fm, body) = split("---\ntype: blog\n---\n\n# Hi\n").unwrap();
        assert_eq!(fm, "type: blog");
        assert_eq!(body, "\n# Hi\n");
    }

    #[test]
    fn no_fence_returns_none() {
        assert!(split("# Just a heading\n").is_none());
        assert!(split("").is_none());
    }

    #[test]
    fn unterminated_fence_returns_none() {
        assert!(split("---\ntype: blog\n").is_none());
    }

    #[test]
    fn empty_frontmatter() {
        let (fm, body) = split("---\n---\nbody").unwrap();
        assert_eq!(fm, "");
        assert_eq!(body, "body");
    }

    #[test]
    fn fence_at_eof() {
        let (fm, body) = split("---\ntype: blog\n---").unwrap();
        assert_eq!(fm, "type: blog");
        assert_eq!(body, "");
    }

    #[test]
    fn crlf_fences() {
        let (fm, body) = split("---\r\ntype: blog\r\n---\r\nbody").unwrap();
        assert_eq!(fm, "type: blog\r");
        assert_eq!(body, "body");
    }

    #[test]
    fn triple_dash_inside_body_not_a_fence() {
        let (fm, body) = split("---\ntype: blog\n---\ntext\n---more text\n").unwrap();
        assert_eq!(fm, "type: blog");
        assert!(body.contains("---more text"));
    }

    #[test]
    fn horizontal_rule_inside_frontmatter_value_skipped() {
        // "\n---x" is not a closing fence; the real fence comes later
        let (fm, _) = split("---\nkey: |\n  a\n---real\ntype: blog\n---\nbody").unwrap();
        assert!(fm.contains("---real"));
    }

    #[test]
    fn parse_empty_is_empty_mapping() {
        assert!(parse("").unwrap().is_empty());
        assert!(parse("  \n").unwrap().is_empty());
    }

    #[test]
    fn parse_malformed_errors() {
        assert!(parse("type: [unclosed").is_err());
        assert!(parse("- just\n- a list").is_err());
    }

    #[test]
    fn yaml_dates_stay_strings() {
        let m = parse("created: 2026-05-10").unwrap();
        assert_eq!(m.get("created").and_then(|v| v.as_str()), Some("2026-05-10"));
    }
}
