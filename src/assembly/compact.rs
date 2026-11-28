/// The ONE compaction-harness builder, shared by the main `/compact`
/// and the `agents_compact` pane path: resolve the summarization route
/// (the user's summarization models when configured, else the primary
/// route — `auto` has no models of its own), apply the session cache
/// headers keyed to `cache_key`, and return the EMPTY-context harness.
/// The caller restores the context it wants compacted — the mechanism
/// (`compact_on_demand`) is identical either way; only the context and
/// the cache affinity differ.
///
/// Free function (not an `App` method) on purpose: the pane path
/// (`agents_dispatch`) reuses it too, and it touches no `App` state —
/// it only builds the harness the caller drives `compact_on_demand`
/// with.
#[allow(clippy::too_many_arguments)]
pub fn build_manual_compaction_harness(
    provider: &str,
    model: Option<&str>,
    base_url: Option<String>,
    summarization_models: Vec<(String, String)>,
    local_base_urls: std::collections::HashMap<String, String>,
    anthropic_ttl: bool,
    retention: Option<String>,
    reasoning: Option<String>,
    cache_key: &str,
    cwd: &str,
) -> Result<crate::harness::Harness, String> {
    let connector = if !summarization_models.is_empty() {
        summarization_models
            .iter()
            .find_map(|(provider, model)| {
                if model.is_empty() || model == "auto" || provider.is_empty() {
                    return None;
                }
                aiface::connector::Connector::new(provider)
                    .ok()
                    .map(|mut connector| {
                        connector = connector.with_service_keyring(crate::KEYRING_SERVICE);
                        connector = connector.with_model(model);
                        if let Some(url) = local_base_urls.get(provider) {
                            connector = connector.with_base_url(url.clone());
                        }
                        connector
                    })
            })
            .ok_or_else(|| "No configured summarization connector is available.".to_string())?
    } else if model == Some("auto") {
        return Err("Auto has no configured models.".to_string());
    } else {
        let mut connector = aiface::connector::Connector::new(provider)
            .map_err(|error| error.to_string())?
            .with_service_keyring(crate::KEYRING_SERVICE);
        if let Some(m) = model {
            connector = connector.with_model(m);
        }
        if let Some(url) = base_url {
            connector = connector.with_base_url(url);
        }
        if let Some(effort) = reasoning {
            connector = connector.with_reasoning_effort(effort);
        }
        connector
    };
    let mut connector = connector
        .with_prompt_cache_ttl_1h(anthropic_ttl)
        .with_prompt_cache_key(cache_key)
        // Same session-affinity routing as the main loop: compaction
        // requests carry the session's opaque hash so they reach the
        // same cache-warm backend.
        .with_session_id(super::session_affinity_id(cache_key));
    if let Some(retention) = retention {
        connector = connector.with_prompt_cache_retention(retention);
    }
    Ok(
        crate::harness::Harness::new(connector, cwd, std::collections::HashSet::new())
            .with_summarization_models(summarization_models)
            .with_local_base_urls(local_base_urls),
    )
}
