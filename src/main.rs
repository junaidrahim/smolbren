mod agent_docs;
mod chunker;
mod cli;
mod config;
mod embed;
mod embedder;
mod error;
mod graph;
mod indexer;
mod ontology;
mod output;
mod parser;
mod search;
mod similarity;
mod store;
mod vault;

use clap::Parser;

use crate::cli::{Cli, Command, GetFormat, VaultCmd};
use crate::config::ConfigStore;
use crate::error::{Result, SmolbrenError};
use crate::vault::resolve_vault;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("{}", serde_json::json!({"error": e.to_string(), "code": e.code()}));
        std::process::exit(e.exit_code());
    }
}

async fn run(cli: Cli) -> Result<()> {
    let mut cfg = ConfigStore::load(cli.config.clone())?;
    match cli.command {
        Command::Docs { agent } => {
            debug_assert!(agent, "clap requires --agent");
            output::print_text(agent_docs::SKILL);
            Ok(())
        }
        Command::Vault { cmd } => vault_cmd(&mut cfg, cmd).await,
        Command::Index { full } => {
            let vault = resolve_vault(&cfg, cli.vault.as_deref())?;
            let stats = indexer::run(&vault, full).await?;
            output::print_json(&stats);
            Ok(())
        }
        Command::Repair => {
            let vault = resolve_vault(&cfg, cli.vault.as_deref())?;
            let stats = indexer::run(&vault, true).await?;
            output::print_json(&serde_json::json!({"repaired": true, "stats": stats}));
            Ok(())
        }
        Command::Embed { full } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let stats = embed::run(&vault, &cfg.models_dir(), full).await?;
            output::print_json(&stats);
            Ok(())
        }
        Command::Search { query, note_type, path, limit, hybrid } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let hits = if hybrid {
                require_embedded(&vault)?;
                similarity::hybrid(
                    &vault,
                    &cfg.models_dir(),
                    &query,
                    note_type.as_deref(),
                    path.as_deref(),
                    limit,
                )
                    .await?
            } else {
                search::bm25(&vault, &query, note_type.as_deref(), path.as_deref(), limit).await?
            };
            output::print_json(&hits);
            Ok(())
        }
        Command::Similar { query, note_type, path, limit } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            require_embedded(&vault)?;
            let hits =
                similarity::similar(
                    &vault,
                    &cfg.models_dir(),
                    &query,
                    note_type.as_deref(),
                    path.as_deref(),
                    limit,
                )
                    .await?;
            output::print_json(&hits);
            Ok(())
        }
        Command::Query { cypher, params } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let result = graph::run_query(&vault, &cypher, &params).await?;
            output::print_json(&result);
            Ok(())
        }
        Command::Get { id, body, format } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let note = store::notes::get(&vault, &id, body || format == GetFormat::Text).await?;
            match format {
                GetFormat::Json => output::print_json(&note),
                GetFormat::Text => output::print_text(
                    note["body"].as_str().ok_or_else(|| {
                        SmolbrenError::Other(anyhow::anyhow!("indexed note body is not text: {id}"))
                    })?,
                ),
            }
            Ok(())
        }
        Command::Links { id, edge_type } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let links = store::edges::links(&vault, &id, edge_type.as_deref()).await?;
            output::print_json(&links);
            Ok(())
        }
        Command::Backlinks { id, edge_type } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let links = store::edges::backlinks(&vault, &id, edge_type.as_deref()).await?;
            output::print_json(&links);
            Ok(())
        }
        Command::Unresolved { edge_type, limit } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let links = store::edges::unresolved(&vault, edge_type.as_deref(), limit).await?;
            output::print_json(&links);
            Ok(())
        }
        Command::Types => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let ont = ontology::Ontology::load(&vault.ontology_path())
                .map_err(SmolbrenError::Other)?;
            let rows: Vec<_> = ont
                .types
                .iter()
                .map(|(t, n)| serde_json::json!({"type": t, "count": n}))
                .collect();
            output::print_json(&rows);
            Ok(())
        }
        Command::Edges => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let ont = ontology::Ontology::load(&vault.ontology_path())
                .map_err(SmolbrenError::Other)?;
            let rows: Vec<_> = ont
                .edge_types
                .iter()
                .map(|(t, n)| serde_json::json!({"edge_type": t, "count": n}))
                .collect();
            output::print_json(&rows);
            Ok(())
        }
    }
}

fn require_indexed(cfg: &ConfigStore, name: Option<&str>) -> Result<vault::Vault> {
    let vault = resolve_vault(cfg, name)?;
    if !vault.is_indexed() {
        return Err(SmolbrenError::IndexMissing(vault.name));
    }
    Ok(vault)
}

fn require_embedded(vault: &vault::Vault) -> Result<()> {
    if !vault.has_embeddings() {
        return Err(SmolbrenError::EmbeddingsMissing(vault.name.clone()));
    }
    Ok(())
}

async fn vault_cmd(cfg: &mut ConfigStore, cmd: VaultCmd) -> Result<()> {
    match cmd {
        VaultCmd::Add { name, path, default } => {
            let path = std::fs::canonicalize(&path).map_err(|e| {
                SmolbrenError::Other(anyhow::anyhow!("vault path {}: {e}", path.display()))
            })?;
            if !path.is_dir() {
                return Err(SmolbrenError::Other(anyhow::anyhow!(
                    "vault path is not a directory: {}",
                    path.display()
                )));
            }
            let make_default = default || cfg.config.vaults.is_empty();
            cfg.config.vaults.insert(name.clone(), path.clone());
            if make_default {
                cfg.config.default_vault = Some(name.clone());
            }
            cfg.save()?;
            output::print_json(&serde_json::json!({
                "name": name, "path": path, "default": make_default
            }));
            Ok(())
        }
        VaultCmd::List => {
            let mut rows = Vec::with_capacity(cfg.config.vaults.len());
            for (name, path) in &cfg.config.vaults {
                    let data_dir = cfg.vaults_dir().join(name);
                    let indexed_at_ms = ontology::Ontology::load(&data_dir.join("ontology.json"))
                        .ok()
                        .map(|o| o.indexed_at_ms);
                    let vault = vault::Vault {
                        name: name.clone(),
                        source: path.clone(),
                        data_dir,
                    };
                    let embedding_status = embedding_status(&vault).await;
                    rows.push(serde_json::json!({
                        "name": name,
                        "path": path,
                        "default": cfg.config.default_vault.as_deref() == Some(name),
                        "indexed_at_ms": indexed_at_ms,
                        "indexed_notes": embedding_status.as_ref().ok().map(|s| s.indexed_notes),
                        "embedded_notes": embedding_status.as_ref().ok().map(|s| s.embedded_notes),
                        "embedding_lag_notes": embedding_status.as_ref().ok().map(|s| s.lag_notes),
                        "orphaned_embedding_notes": embedding_status.as_ref().ok().map(|s| s.orphaned_notes),
                        "embeddings_stale": embedding_status.as_ref().ok().map(|s| s.lag_notes > 0 || s.orphaned_notes > 0),
                        "embedding_status_error": embedding_status.err().map(|e| e.to_string()),
                    }));
            }
            output::print_json(&rows);
            Ok(())
        }
        VaultCmd::Remove { name } => {
            if cfg.config.vaults.remove(&name).is_none() {
                return Err(SmolbrenError::VaultNotFound(name));
            }
            if cfg.config.default_vault.as_deref() == Some(name.as_str()) {
                cfg.config.default_vault = None;
            }
            let data_dir = cfg.vaults_dir().join(&name);
            if data_dir.exists() {
                std::fs::remove_dir_all(&data_dir).map_err(|e| {
                    SmolbrenError::Other(anyhow::anyhow!("removing {}: {e}", data_dir.display()))
                })?;
            }
            cfg.save()?;
            output::print_json(&serde_json::json!({"removed": name}));
            Ok(())
        }
    }
}

struct EmbeddingStatus {
    indexed_notes: usize,
    embedded_notes: usize,
    lag_notes: usize,
    orphaned_notes: usize,
}

async fn embedding_status(vault: &vault::Vault) -> anyhow::Result<EmbeddingStatus> {
    if !vault.is_indexed() {
        anyhow::bail!("index missing");
    }
    let notes = store::notes::load_state(vault).await?;
    let embedded = store::embeddings::load_state(vault).await?;
    let lag_notes = notes
        .iter()
        .filter(|(id, meta)| embedded.get(*id) != Some(&meta.content_hash))
        .count();
    let orphaned_notes = embedded.keys().filter(|id| !notes.contains_key(*id)).count();
    Ok(EmbeddingStatus {
        indexed_notes: notes.len(),
        embedded_notes: embedded.len(),
        lag_notes,
        orphaned_notes,
    })
}
