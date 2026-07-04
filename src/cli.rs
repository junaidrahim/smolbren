use std::path::PathBuf;

use clap::{Parser, Subcommand};

const BANNER: &str = r"┏━┓┏┳┓┏━┓╻  ┏┓ ┏━┓┏━╸┏┓╻
┗━┓┃┃┃┃ ┃┃  ┣┻┓┣┳┛┣╸ ┃┗┫
┗━┛╹ ╹┗━┛┗━╸┗━┛╹┗╸┗━╸╹ ╹";

#[derive(Parser)]
#[command(
    name = "smolbren",
    version,
    about = "ontology-first search over markdown vaults",
    before_help = BANNER
)]
pub struct Cli {
    /// Vault name (defaults to the configured default vault)
    #[arg(long, global = true)]
    pub vault: Option<String>,

    /// Config file path (default: ~/.smolbren/config.json)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage vaults
    Vault {
        #[command(subcommand)]
        cmd: VaultCmd,
    },
    /// Index the vault (incremental by default)
    Index {
        /// Rebuild the index from scratch
        #[arg(long)]
        full: bool,
    },
    /// BM25 full-text search over note titles and bodies
    Search {
        query: String,
        /// Restrict results to one note type
        #[arg(long = "type")]
        note_type: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Run a Cypher query over the note graph
    Query {
        cypher: String,
        /// Query parameters as key=value (repeatable)
        #[arg(long = "param", value_parser = parse_kv)]
        params: Vec<(String, String)>,
    },
    /// Fetch one note by id
    Get {
        id: String,
        /// Include the markdown body
        #[arg(long)]
        body: bool,
    },
    /// Outgoing edges of a note
    Links {
        id: String,
        /// Restrict to one edge type
        #[arg(long = "type")]
        edge_type: Option<String>,
    },
    /// Incoming edges of a note
    Backlinks {
        id: String,
        /// Restrict to one edge type
        #[arg(long = "type")]
        edge_type: Option<String>,
    },
    /// List note types with counts
    Types,
    /// List edge types with counts
    Edges,
}

#[derive(Subcommand)]
pub enum VaultCmd {
    /// Register a vault
    Add {
        name: String,
        path: PathBuf,
        /// Make this the default vault
        #[arg(long)]
        default: bool,
    },
    /// List registered vaults
    List,
    /// Unregister a vault and delete its index data
    Remove { name: String },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got '{s}'"))
}
