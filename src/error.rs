use thiserror::Error;

#[derive(Debug, Error)]
pub enum SmolbrenError {
    #[error("vault not found: {0}")]
    VaultNotFound(String),
    #[error("no vault specified and no default vault configured")]
    NoVault,
    #[error("note not found: {0}")]
    NoteNotFound(String),
    #[error("index missing for vault '{0}' — run `smolbren index` first")]
    IndexMissing(String),
    #[error("embeddings missing for vault '{0}' — run `smolbren embed` first")]
    EmbeddingsMissing(String),
    #[error("embedding model error: {0} — the model downloads from Hugging Face on first use; check network access, or delete the models dir to clear a corrupt cache")]
    Model(anyhow::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SmolbrenError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::VaultNotFound(_) | Self::NoVault => "vault_not_found",
            Self::NoteNotFound(_) => "note_not_found",
            Self::IndexMissing(_) => "index_missing",
            Self::EmbeddingsMissing(_) => "embeddings_missing",
            Self::Model(_) => "model_error",
            Self::Other(_) => "internal",
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::VaultNotFound(_) | Self::NoVault => 3,
            Self::NoteNotFound(_) => 4,
            Self::IndexMissing(_) => 5,
            Self::EmbeddingsMissing(_) => 6,
            Self::Model(_) => 7,
            Self::Other(_) => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, SmolbrenError>;
