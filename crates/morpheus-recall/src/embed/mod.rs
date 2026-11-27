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
    let Ok(info) = fastembed::TextEmbedding::get_model_info(&model) else {
        return false;
    };
    model_files_cached(&hf_hub::Cache::new(rag::local_model_cache()), info)
}

#[cfg(feature = "fastembed")]
fn model_files_cached(
    cache: &hf_hub::Cache,
    info: &fastembed::ModelInfo<fastembed::EmbeddingModel>,
) -> bool {
    let repo = cache.model(info.model_code.clone());
    // These are the tokenizer files FastEmbed loads for every text model.
    let tokenizer_files = [
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ];
    std::iter::once(info.model_file.as_str())
        .chain(info.additional_files.iter().map(String::as_str))
        .chain(tokenizer_files)
        .all(|file| repo.get(file).is_some_and(|path| path.is_file()))
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

#[cfg(all(test, feature = "fastembed"))]
mod local_cache_tests {
    use super::*;

    #[test]
    fn partial_embedding_snapshot_is_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = hf_hub::Cache::new(dir.path().into());
        let info =
            fastembed::TextEmbedding::get_model_info(&fastembed::EmbeddingModel::AllMiniLML6V2)
                .unwrap();
        let repo = cache.model(info.model_code.clone());
        repo.create_ref("revision").unwrap();
        let snapshot = repo.pointer_path("revision");
        for file in std::iter::once(info.model_file.as_str())
            .chain(info.additional_files.iter().map(String::as_str))
            .chain([
                "config.json",
                "special_tokens_map.json",
                "tokenizer_config.json",
            ])
        {
            let path = snapshot.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"fixture").unwrap();
        }
        assert!(!model_files_cached(&cache, info));

        std::fs::write(snapshot.join("tokenizer.json"), b"fixture").unwrap();
        assert!(model_files_cached(&cache, info));
    }
}
