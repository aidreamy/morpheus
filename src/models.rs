//! Model discovery shared by the terminal and desktop pickers.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use aiface::connector::{
    Connector, custom_provider_names, get_provider, has_api_key, is_local_provider,
    known_local_providers, known_providers_with_env,
};
use tokio::task::JoinSet;

use crate::{KEYRING_SERVICE, ModelEntry};

/// Cloud providers with credentials and local providers with distinct endpoints.
#[must_use]
pub fn active_providers(base_urls: &HashMap<String, String>) -> Vec<&'static str> {
    let mut active = Vec::new();
    for provider in known_providers_with_env()
        .map(|(provider, _)| provider)
        .chain(custom_provider_names())
    {
        if !is_local_provider(provider)
            && !active.contains(&provider)
            && has_api_key(provider, Some(KEYRING_SERVICE))
        {
            active.push(provider);
        }
    }

    let mut endpoints = HashSet::new();
    for provider in known_local_providers() {
        let endpoint = base_urls
            .get(provider)
            .map(String::as_str)
            .or_else(|| get_provider(provider).map(|config| config.base_url));
        if let Some(endpoint) = endpoint
            && endpoints.insert(endpoint)
        {
            active.push(provider);
        }
    }
    active
}

/// Fetch real provider catalogs in parallel. An unavailable endpoint does not
/// delay the picker beyond the deadline for a single provider.
pub async fn list_models(
    providers: Vec<&'static str>,
    base_urls: &HashMap<String, String>,
) -> Vec<ModelEntry> {
    let mut pending = JoinSet::new();
    for provider in providers {
        let base_url = base_urls.get(provider).cloned();
        pending.spawn(async move {
            let mut connector = Connector::new(provider)
                .ok()?
                .with_service_keyring(KEYRING_SERVICE);
            if let Some(url) = base_url {
                connector = connector.with_base_url(url);
            }
            let output = tokio::time::timeout(Duration::from_secs(6), connector.list_models())
                .await
                .ok()?
                .ok()?;
            Some(
                output
                    .models()
                    .iter()
                    .map(|model| ModelEntry {
                        provider: provider.to_owned(),
                        model: model.id().to_owned(),
                    })
                    .collect::<Vec<_>>(),
            )
        });
    }

    let mut models = Vec::new();
    while let Some(result) = pending.join_next().await {
        if let Ok(Some(result)) = result {
            models.extend(result);
        }
    }
    models.sort_unstable_by(|a, b| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)));
    models.dedup_by(|a, b| a.provider == b.provider && a.model == b.model);
    models
}

#[must_use]
pub fn matches_filter(entry: &ModelEntry, filter: &str) -> bool {
    let needle = filter.to_lowercase();
    entry.model.to_lowercase().contains(&needle) || entry.provider.to_lowercase().contains(&needle)
}
