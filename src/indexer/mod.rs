pub mod resolve;
pub mod walk;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rayon::prelude::*;
use serde::Serialize;

use crate::error::{Result, SmolbrenError};
use crate::ontology::Ontology;
use crate::parser::{self, ParsedContent};
use crate::progress::Progress;
use crate::store::schema::{EdgeRow, NoteRow};
use crate::store::{self, StoredMeta};
use crate::vault::Vault;

#[derive(Debug, Serialize)]
pub struct IndexStats {
    pub scanned: usize,
    pub unchanged: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub edges: usize,
    pub unresolved_edges: usize,
    pub duration_ms: u128,
}

pub async fn run(vault: &Vault, full: bool) -> Result<IndexStats> {
    let t0 = Instant::now();
    recover_interrupted_swap(vault)?;
    let mut stats = if full {
        run_transactional_full(vault).await
    } else {
        run_inner(vault, false).await
    }?;
    stats.duration_ms = t0.elapsed().as_millis();
    eprintln!(
        "index: complete — {} notes scanned, {} added, {} updated, {} removed in {:.1}s",
        stats.scanned,
        stats.added,
        stats.updated,
        stats.removed,
        t0.elapsed().as_secs_f64(),
    );
    Ok(stats)
}

/// Build a complete index beside the current one, validate it, and replace
/// the live directory with a same-filesystem rename. Any parse/storage/index
/// error leaves the current index untouched.
async fn run_transactional_full(vault: &Vault) -> Result<IndexStats> {
    let staging = sibling_dir(vault, "rebuild");
    let backup = sibling_dir(vault, "backup");
    remove_dir_if_exists(&staging, "removing stale rebuild directory")?;
    let staged = vault.with_data_dir(staging.clone());

    let result = async {
        let stats = run_inner(&staged, true).await?;
        eprintln!("index: validating rebuilt index");
        preserve_embeddings(vault, &staged)?;
        validate_index(&staged).await?;

        // Deterministic failure injection for the recovery regression test.
        // It is intentionally undocumented and only honored for the exact
        // value `1`.
        if std::env::var("SMOLBREN_TEST_FAIL_FULL_BEFORE_SWAP").as_deref() == Ok("1") {
            return Err(SmolbrenError::Other(anyhow::anyhow!(
                "injected full-rebuild failure before swapping {}",
                staging.display()
            )));
        }
        eprintln!("index: activating rebuilt index");
        swap_index(vault, &staging, &backup)?;
        Ok(stats)
    }
    .await;

    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map_err(|e| {
        SmolbrenError::Other(anyhow::anyhow!(
            "building transactional full index for vault '{}' at {}: {e}",
            vault.name,
            staging.display()
        ))
    })
}

async fn run_inner(vault: &Vault, full: bool) -> Result<IndexStats> {
    let t0 = Instant::now();
    if !vault.source.is_dir() {
        return Err(SmolbrenError::Other(anyhow::anyhow!(
            "vault source is not a directory: {}",
            vault.source.display()
        )));
    }

    eprintln!("index: scanning {}", vault.source.display());
    let files = walk::walk_vault(&vault.source)?;
    let live_ids: HashSet<String> = files.iter().map(|f| parser::note_id(&f.rel)).collect();

    let prior: HashMap<String, StoredMeta> = if full {
        HashMap::new()
    } else {
        store::notes::load_state(vault).await?
    };

    // Cheap pre-filter: identical (mtime, size) → skip without reading.
    let (skipped, candidates): (Vec<_>, Vec<_>) = files.iter().partition(|f| {
        prior
            .get(&parser::note_id(&f.rel))
            .is_some_and(|m| m.mtime_ms == f.mtime_ms && m.size_bytes == f.size_bytes)
    });
    eprintln!(
        "index: discovered {} notes — {} need parsing, {} unchanged",
        files.len(),
        candidates.len(),
        skipped.len(),
    );

    // Parallel read + hash + parse across all cores.
    if !candidates.is_empty() {
        eprintln!("index: parsing 0/{} files", candidates.len());
    }
    let parse_progress =
        Mutex::new((0usize, Progress::new("index: parsed", "files", candidates.len())));
    let parsed: Vec<(&walk::WalkedFile, ParsedContent)> = candidates
        .par_iter()
        .map(|f| {
            let content = std::fs::read_to_string(&f.abs)
                .with_context(|| format!("reading note {}", f.abs.display()))?;
            let parsed = parser::parse_note(&f.rel, &content);
            let mut progress = parse_progress.lock().expect("parse progress reporter poisoned");
            progress.0 += 1;
            let completed = progress.0;
            progress.1.update(completed);
            Ok((*f, parsed))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(SmolbrenError::Other)?;

    for (_, note) in &parsed {
        for w in &note.warnings {
            eprintln!("warn: {w}");
        }
    }

    // Classify: content actually changed vs only fs metadata touched.
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut touched = 0usize;
    let mut note_rows: Vec<NoteRow> = Vec::new();
    let mut changed_ids: Vec<String> = Vec::new();
    let indexed_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let ids_vec: Vec<String> = live_ids.iter().cloned().collect();
    let basenames = resolve::basename_map(&ids_vec);
    let mut edge_rows: Vec<EdgeRow> = Vec::new();

    for (file, note) in &parsed {
        let content_changed = match prior.get(&note.id) {
            Some(m) if m.content_hash == note.content_hash => {
                touched += 1; // mtime-only change; rewrite the row so mtime converges
                false
            }
            Some(_) => {
                updated += 1;
                true
            }
            None => {
                added += 1;
                true
            }
        };
        note_rows.push(NoteRow {
            id: note.id.clone(),
            path: note.path.clone(),
            note_type: note.note_type.clone(),
            title: note.title.clone(),
            frontmatter_json: note.frontmatter_json.clone(),
            body: note.body.clone(),
            frontmatter_hash: note.frontmatter_hash.clone(),
            content_hash: note.content_hash.clone(),
            mtime_ms: file.mtime_ms,
            size_bytes: file.size_bytes,
            indexed_at_ms,
        });
        if content_changed {
            changed_ids.push(note.id.clone());
            for e in &note.edges {
                let (to_id, resolved) = resolve::resolve_target(&e.to_raw, &live_ids, &basenames);
                edge_rows.push(EdgeRow {
                    from_id: note.id.clone(),
                    edge_type: e.edge_type.clone(),
                    to_id,
                    to_raw: e.to_raw.clone(),
                    to_alias: e.to_alias.clone(),
                    resolved,
                    position: e.position,
                });
            }
        }
    }

    let removed_ids: Vec<String> = prior
        .keys()
        .filter(|id| !live_ids.contains(*id))
        .cloned()
        .collect();

    // Writes.
    eprintln!(
        "index: writing changes — {added} added, {updated} updated, {} metadata-only, {} removed, {} changed edges",
        touched,
        removed_ids.len(),
        edge_rows.len(),
    );
    if full {
        store::notes::overwrite(vault, note_rows).await?;
        store::edges::overwrite(vault, edge_rows).await?;
    } else {
        store::notes::upsert(vault, note_rows).await?;
        if !removed_ids.is_empty() {
            store::notes::delete_ids(vault, &removed_ids).await?;
        }
        // Changed notes get their edges replaced; removed notes lose theirs.
        let mut edge_owners = changed_ids.clone();
        edge_owners.extend(removed_ids.iter().cloned());
        if !edge_owners.is_empty() {
            store::edges::replace_for(vault, &edge_owners, edge_rows).await?;
        }
    }
    store::refresh_indices(vault).await?;

    // Ontology + totals from the now-current datasets.
    let (types, total_notes) = store::notes::type_counts(vault).await?;
    let _ = total_notes;
    let (edge_types, total_edges, unresolved_edges) = store::edges::edge_counts(vault).await?;
    Ontology { types, edge_types, indexed_at_ms }
        .save(&vault.ontology_path())
        .map_err(SmolbrenError::Other)?;

    Ok(IndexStats {
        scanned: files.len(),
        unchanged: skipped.len() + touched,
        added,
        updated,
        removed: removed_ids.len(),
        edges: total_edges,
        unresolved_edges,
        duration_ms: t0.elapsed().as_millis(),
    })
}

fn sibling_dir(vault: &Vault, suffix: &str) -> PathBuf {
    let parent = vault.data_dir.parent().unwrap_or_else(|| Path::new("."));
    let name = vault
        .data_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&vault.name);
    parent.join(format!(".{name}.{suffix}"))
}

fn remove_dir_if_exists(path: &Path, operation: &str) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("{operation} {}", path.display()))?;
    }
    Ok(())
}

/// Restore the last good directory if a process died between the two rename
/// operations. If both live and backup exist, the live directory already won
/// the swap and the backup is stale.
pub fn recover_interrupted_swap(vault: &Vault) -> Result<()> {
    let staging = sibling_dir(vault, "rebuild");
    let backup = sibling_dir(vault, "backup");
    if !vault.data_dir.exists() && backup.exists() {
        std::fs::rename(&backup, &vault.data_dir).with_context(|| {
            format!(
                "restoring last good index {} to {}",
                backup.display(),
                vault.data_dir.display()
            )
        })?;
    } else if vault.data_dir.exists() && backup.exists() {
        remove_dir_if_exists(&backup, "removing completed-swap backup")?;
    }
    if staging.exists() {
        remove_dir_if_exists(&staging, "removing interrupted rebuild")?;
    }
    Ok(())
}

fn preserve_embeddings(live: &Vault, staged: &Vault) -> Result<()> {
    let source = live.data_dir.join("embeddings.lance");
    if source.exists() {
        copy_dir_recursive(&source, &staged.data_dir.join("embeddings.lance"))?;
    }
    let meta = live.embeddings_meta_path();
    if meta.exists() {
        std::fs::copy(&meta, staged.embeddings_meta_path()).with_context(|| {
            format!(
                "copying embedding metadata {} to staged index",
                meta.display()
            )
        })?;
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("creating staged directory {}", target.display()))?;
    for entry in std::fs::read_dir(source)
        .with_context(|| format!("reading directory {}", source.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", source.display()))?;
        let destination = target.join(entry.file_name());
        if entry
            .file_type()
            .with_context(|| format!("reading file type for {}", entry.path().display()))?
            .is_dir()
        {
            copy_dir_recursive(&entry.path(), &destination)?;
        } else {
            std::fs::copy(entry.path(), &destination).with_context(|| {
                format!(
                    "copying {} to staged index {}",
                    entry.path().display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

async fn validate_index(vault: &Vault) -> Result<()> {
    let notes = store::open_dataset(&vault.notes_uri()).await?;
    notes
        .count_rows(None)
        .await
        .with_context(|| format!("validating staged notes dataset {}", vault.notes_uri()))?;
    let edges = store::open_dataset(&vault.edges_uri()).await?;
    edges
        .count_rows(None)
        .await
        .with_context(|| format!("validating staged edges dataset {}", vault.edges_uri()))?;
    Ontology::load(&vault.ontology_path()).map_err(SmolbrenError::Other)?;
    Ok(())
}

fn swap_index(vault: &Vault, staging: &Path, backup: &Path) -> Result<()> {
    if let Some(parent) = vault.data_dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating index parent {}", parent.display()))?;
    }
    remove_dir_if_exists(backup, "removing stale backup")?;
    if vault.data_dir.exists() {
        std::fs::rename(&vault.data_dir, backup).with_context(|| {
            format!(
                "moving current index {} to recovery backup {}",
                vault.data_dir.display(),
                backup.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(staging, &vault.data_dir) {
        if backup.exists() && !vault.data_dir.exists() {
            let _ = std::fs::rename(backup, &vault.data_dir);
        }
        return Err(SmolbrenError::Other(anyhow::anyhow!(
            "atomically swapping staged index {} into {}: {error}",
            staging.display(),
            vault.data_dir.display()
        )));
    }
    remove_dir_if_exists(backup, "removing successful-swap backup")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_vault(tmp: &TempDir) -> Vault {
        Vault {
            name: "test".to_string(),
            source: tmp.path().join("source"),
            data_dir: tmp.path().join("vaults/test"),
        }
    }

    #[test]
    fn rebuild_and_backup_directories_are_siblings_of_the_live_index() {
        let tmp = TempDir::new().unwrap();
        let vault = test_vault(&tmp);
        assert_eq!(
            sibling_dir(&vault, "rebuild"),
            tmp.path().join("vaults/.test.rebuild")
        );
        assert_eq!(
            sibling_dir(&vault, "backup"),
            tmp.path().join("vaults/.test.backup")
        );
    }

    #[test]
    fn interrupted_swap_restores_the_last_good_backup() {
        let tmp = TempDir::new().unwrap();
        let vault = test_vault(&tmp);
        let backup = sibling_dir(&vault, "backup");
        let staging = sibling_dir(&vault, "rebuild");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(backup.join("marker"), "last-good").unwrap();

        recover_interrupted_swap(&vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(vault.data_dir.join("marker")).unwrap(),
            "last-good"
        );
        assert!(!backup.exists());
        assert!(!staging.exists());
    }

    #[test]
    fn completed_swap_keeps_live_index_and_removes_stale_backup() {
        let tmp = TempDir::new().unwrap();
        let vault = test_vault(&tmp);
        let backup = sibling_dir(&vault, "backup");
        std::fs::create_dir_all(&vault.data_dir).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(vault.data_dir.join("marker"), "new-live").unwrap();
        std::fs::write(backup.join("marker"), "old-live").unwrap();

        recover_interrupted_swap(&vault).unwrap();

        assert_eq!(
            std::fs::read_to_string(vault.data_dir.join("marker")).unwrap(),
            "new-live"
        );
        assert!(!backup.exists());
    }
}
