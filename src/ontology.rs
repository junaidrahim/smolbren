use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The discovered ontology of a vault: note types and edge types with counts.
/// Written at index time; read at query time to build the graph config
/// without scanning datasets.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ontology {
    pub types: BTreeMap<String, u64>,
    pub edge_types: BTreeMap<String, u64>,
    pub indexed_at_ms: i64,
}

impl Ontology {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}
