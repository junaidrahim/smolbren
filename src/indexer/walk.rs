use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct WalkedFile {
    pub abs: PathBuf,
    /// Vault-relative path, `/`-normalized, with extension.
    pub rel: String,
    pub mtime_ms: i64,
    pub size_bytes: i64,
}

/// Collect all markdown files in the vault. The `ignore` walker's standard
/// filters skip hidden entries (`.obsidian/`, `.git/`, `.trash/`) and honor
/// `.gitignore` for free.
pub fn walk_vault(root: &Path) -> Result<Vec<WalkedFile>> {
    let mut files = Vec::new();
    for entry in ignore::WalkBuilder::new(root).follow_links(false).build() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warn: walk: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if !path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warn: metadata for {}: {e}", path.display());
                continue;
            }
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("stripping {} from {}", root.display(), path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        files.push(WalkedFile {
            abs: path.to_path_buf(),
            rel,
            mtime_ms,
            size_bytes: meta.len() as i64,
        });
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}
