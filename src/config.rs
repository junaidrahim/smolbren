use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub vaults: BTreeMap<String, PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_vault: Option<String>,
}

/// Config plus the path it was loaded from. Vault data lives next to the
/// config file (`<parent>/vaults/<name>/`), so tests can isolate everything
/// with `--config`.
pub struct ConfigStore {
    pub path: PathBuf,
    pub config: Config,
}

pub fn default_config_path() -> PathBuf {
    dirs::home_dir()
        .expect("cannot determine home directory")
        .join(".smolbren")
        .join("config.json")
}

impl ConfigStore {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(default_config_path);
        let config = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading config {}", path.display()))?;
            serde_json::from_str(&raw)
                .with_context(|| format!("parsing config {}", path.display()))?
        } else {
            Config::default()
        };
        Ok(Self { path, config })
    }

    pub fn save(&self) -> Result<()> {
        let parent = self.root();
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        let tmp = self.path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(&self.config)?;
        std::fs::write(&tmp, raw).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming to {}", self.path.display()))?;
        Ok(())
    }

    /// Directory holding config.json and all vault data.
    pub fn root(&self) -> &Path {
        self.path.parent().expect("config path has no parent")
    }

    pub fn vaults_dir(&self) -> PathBuf {
        self.root().join("vaults")
    }
}
