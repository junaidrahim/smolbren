use std::path::PathBuf;

use crate::config::ConfigStore;
use crate::error::{Result, SmolbrenError};

pub struct Vault {
    pub name: String,
    /// Directory of markdown files (the Obsidian vault).
    pub source: PathBuf,
    /// `<config root>/vaults/<name>` — lance datasets + ontology.json.
    pub data_dir: PathBuf,
}

impl Vault {
    pub fn notes_uri(&self) -> String {
        self.data_dir.join("notes.lance").to_string_lossy().into_owned()
    }

    pub fn edges_uri(&self) -> String {
        self.data_dir.join("edges.lance").to_string_lossy().into_owned()
    }

    pub fn embeddings_uri(&self) -> String {
        self.data_dir.join("embeddings.lance").to_string_lossy().into_owned()
    }

    pub fn ontology_path(&self) -> PathBuf {
        self.data_dir.join("ontology.json")
    }

    pub fn embeddings_meta_path(&self) -> PathBuf {
        self.data_dir.join("embeddings_meta.json")
    }

    pub fn is_indexed(&self) -> bool {
        self.data_dir.join("notes.lance").exists()
    }

    pub fn has_embeddings(&self) -> bool {
        self.data_dir.join("embeddings.lance").exists()
    }
}

/// Resolve the target vault from `--vault` or the configured default.
pub fn resolve_vault(store: &ConfigStore, name: Option<&str>) -> Result<Vault> {
    let name = match name.or(store.config.default_vault.as_deref()) {
        Some(n) => n.to_string(),
        None => return Err(SmolbrenError::NoVault),
    };
    let source = store
        .config
        .vaults
        .get(&name)
        .cloned()
        .ok_or_else(|| SmolbrenError::VaultNotFound(name.clone()))?;
    let data_dir = store.vaults_dir().join(&name);
    Ok(Vault { name, source, data_dir })
}
