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

use crate::cli::{Cli, Command, VaultCmd};
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
        Command::Vault { cmd } => vault_cmd(&mut cfg, cmd),
        Command::Index { full } => {
            let vault = resolve_vault(&cfg, cli.vault.as_deref())?;
            let stats = indexer::run(&vault, full).await?;
            output::print_json(&stats);
            Ok(())
        }
        Command::Embed { full } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let stats = embed::run(&vault, &cfg.models_dir(), full).await?;
            output::print_json(&stats);
            Ok(())
        }
        Command::Search { query, note_type, limit, hybrid } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let hits = if hybrid {
                require_embedded(&vault)?;
                similarity::hybrid(&vault, &cfg.models_dir(), &query, note_type.as_deref(), limit)
                    .await?
            } else {
                search::bm25(&vault, &query, note_type.as_deref(), limit).await?
            };
            output::print_json(&hits);
            Ok(())
        }
        Command::Similar { query, note_type, limit } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            require_embedded(&vault)?;
            let hits =
                similarity::similar(&vault, &cfg.models_dir(), &query, note_type.as_deref(), limit)
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
        Command::Get { id, body } => {
            let vault = require_indexed(&cfg, cli.vault.as_deref())?;
            let note = store::notes::get(&vault, &id, body).await?;
            output::print_json(&note);
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

fn vault_cmd(cfg: &mut ConfigStore, cmd: VaultCmd) -> Result<()> {
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
            let rows: Vec<_> = cfg
                .config
                .vaults
                .iter()
                .map(|(name, path)| {
                    let data_dir = cfg.vaults_dir().join(name);
                    let indexed_at_ms = ontology::Ontology::load(&data_dir.join("ontology.json"))
                        .ok()
                        .map(|o| o.indexed_at_ms);
                    serde_json::json!({
                        "name": name,
                        "path": path,
                        "default": cfg.config.default_vault.as_deref() == Some(name),
                        "indexed_at_ms": indexed_at_ms,
                    })
                })
                .collect();
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
