pub mod resolve;
pub mod walk;

use std::collections::{HashMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::Serialize;

use crate::error::{Result, SmolbrenError};
use crate::ontology::Ontology;
use crate::parser::{self, ParsedContent};
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
    if !vault.source.is_dir() {
        return Err(SmolbrenError::Other(anyhow::anyhow!(
            "vault source is not a directory: {}",
            vault.source.display()
        )));
    }

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

    // Parallel read + hash + parse across all cores.
    let parsed: Vec<(&walk::WalkedFile, ParsedContent)> = candidates
        .par_iter()
        .filter_map(|f| {
            let content = match std::fs::read_to_string(&f.abs) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warn: skipping {}: {e}", f.rel);
                    return None;
                }
            };
            Some((*f, parser::parse_note(&f.rel, &content)))
        })
        .collect();

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
