use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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
    /// Emit documentation generated from this binary
    Docs {
        /// Print the canonical Agent Skill markdown
        #[arg(long, required = true)]
        agent: bool,
    },
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
    /// Safely rebuild a damaged index while preserving the last good copy
    Repair,
    /// Embed note chunks for similarity search (incremental by default)
    Embed {
        /// Re-embed every note from scratch
        #[arg(long)]
        full: bool,
    },
    /// BM25 full-text search over note titles and bodies
    Search {
        query: String,
        /// Restrict results to one note type
        #[arg(long = "type")]
        note_type: Option<String>,
        /// Restrict results to a vault-relative path prefix
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Fuse BM25 with vector similarity (requires `smolbren embed`)
        #[arg(long)]
        hybrid: bool,
    },
    /// Semantic similarity search over embedded note chunks
    Similar {
        query: String,
        /// Restrict results to one note type
        #[arg(long = "type")]
        note_type: Option<String>,
        /// Restrict results to a vault-relative path prefix
        #[arg(long)]
        path: Option<String>,
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
        /// Output JSON metadata or the raw Markdown body
        #[arg(long, value_enum, default_value_t = GetFormat::Json)]
        format: GetFormat,
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
    /// Enumerate unresolved wikilink edges for vault grooming
    Unresolved {
        /// Restrict to one edge type
        #[arg(long = "type")]
        edge_type: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// List note types with counts
    Types,
    /// List edge types with counts
    Edges,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum GetFormat {
    Json,
    Text,
}

impl std::fmt::Display for GetFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json => f.write_str("json"),
            Self::Text => f.write_str("text"),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_value_parser_preserves_equals_in_the_value() {
        assert_eq!(
            parse_kv("query=status=draft").unwrap(),
            ("query".to_string(), "status=draft".to_string())
        );
    }

    #[test]
    fn key_value_parser_rejects_a_missing_separator() {
        assert_eq!(
            parse_kv("status").unwrap_err(),
            "expected key=value, got 'status'"
        );
    }

    #[test]
    fn search_flags_parse_path_and_hybrid_together() {
        let cli = Cli::try_parse_from([
            "smolbren", "search", "context", "--path", "Notes/", "--hybrid",
        ])
        .unwrap();
        match cli.command {
            Command::Search {
                path,
                hybrid,
                limit,
                ..
            } => {
                assert_eq!(path.as_deref(), Some("Notes/"));
                assert!(hybrid);
                assert_eq!(limit, 10);
            }
            _ => panic!("expected search command"),
        }
    }

    #[test]
    fn text_output_format_has_a_stable_cli_value() {
        assert_eq!(GetFormat::Text.to_string(), "text");
        assert_eq!(GetFormat::Json.to_string(), "json");
    }
}
