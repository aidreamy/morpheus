pub mod models;
pub mod vec_db;

pub mod rag;

pub use models::{CloudEmbedConfig, EmbedderConfig, LocalEmbedModel};
pub use rag::{Embedder, Rag, RagError};
pub use vec_db::{Entry, VecDb, VecDbError, validate_table_name};

/// Whether `model` is already downloaded into the local model cache.
#[cfg(feature = "fastembed")]
#[must_use]
pub fn local_model_installed(model: fastembed::EmbeddingModel) -> bool {
    fastembed::TextEmbedding::get_model_info(&model)
        // hf-hub stores one snapshot per repo as `models--<org>--<name>`.
        .map(|info| {
            rag::local_model_cache()
                .join(format!("models--{}", info.model_code.replace('/', "--")))
                .is_dir()
        })
        .unwrap_or(false)
}

/// Download `model` into the local model cache, loading it once so a broken
/// download fails here instead of on the first embedding.
#[cfg(feature = "fastembed")]
pub fn download_local_model(model: fastembed::EmbeddingModel) -> Result<(), RagError> {
    Embedder::try_new_local(model).map(|_| ())
}

/// Convert a model name string to `fastembed::EmbeddingModel`.
#[cfg(feature = "fastembed")]
pub fn fastembed_model_from_str(name: &str) -> Result<fastembed::EmbeddingModel, String> {
    name.parse::<LocalEmbedModel>()
        .map(|m| m.to_fastembed_model())
}

#[cfg(test)]
pub mod test;
