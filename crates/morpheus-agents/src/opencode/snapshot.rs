//! On-demand probes and history replay against the OpenCode server.
//!
//! Two consumers exist and both are read-only:
//!
//! - [`snapshot`] answers "what does this agent advertise?" without sending a
//!   prompt: slash commands, the agents (Morpheus session modes), the model
//!   catalog that backs the pane's model picker, and - when a session id is
//!   given - the mode/model that session currently runs with. It uses a
//!   throwaway server and shuts it down before returning.
//! - [`replay`] rebuilds a pane's timeline from an existing session:
//!   `GET /session/:id/message` returns the retained conversation, which is
//!   mapped through the same [`Mapper`] the live stream uses, followed by the
//!   pane chrome the session already has (title and todo plan).

use std::path::PathBuf;

use super::api;
use super::event::Mapper;
use super::http::Http;
use super::server::Server;
use crate::acp::{CwdStrictness, SessionSnapshot, prepare_session_cwd};
use crate::events::{
    AgentCommand, AgentConfigOption, AgentConfigValue, AgentSessionMode, SubagentEvent,
};

/// Agent names the server reserves for its own background work, published
/// with `mode: primary` even though no user selects them.
const INTERNAL_AGENTS: &[&str] = &["compaction", "summary", "title"];

#[derive(serde::Deserialize)]
struct InventoryName {
    id: Option<String>,
    name: Option<String>,
}

pub(super) async fn inventory(
    http: &Http,
    model: Option<&str>,
) -> crate::inventory::AgentInventory {
    let tools = async {
        if let Some((provider, model)) = model.and_then(|name| name.split_once('/')) {
            let query = reqwest::Url::parse_with_params(
                "http://localhost",
                &[("provider", provider), ("model", model)],
            )
            .ok()?;
            let endpoint = format!("/experimental/tool?{}", query.query()?);
            if let Ok(tools) = http.get::<Vec<InventoryName>>(&endpoint).await {
                return Some(names(
                    tools.into_iter().filter_map(|tool| tool.id.or(tool.name)),
                ));
            }
        }
        http.get::<Vec<String>>("/experimental/tool/ids")
            .await
            .ok()
            .map(names)
    };
    let skills = async {
        http.get::<Vec<InventoryName>>("/skill")
            .await
            .ok()
            .map(|skills| {
                names(
                    skills
                        .into_iter()
                        .filter_map(|skill| skill.name.or(skill.id)),
                )
            })
    };
    let (tools, skills) = tokio::join!(tools, skills);
    crate::inventory::AgentInventory {
        tools,
        skills,
        resume: Some(true),
        ..Default::default()
    }
}

fn names(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<_> = values
        .into_iter()
        .filter(|name| crate::inventory::identifier(name))
        .collect();
    names.sort_unstable();
    names.dedup();
    names.truncate(32);
    names
}

/// Probe a session's advertised surface over a throwaway server.
///
/// `session_id` names the session whose current mode/model are reported back
/// as the pane's selection (the server records both on the session).
///
/// # Errors
///
/// Returns an error when the server cannot start or the catalog endpoints
/// fail. Individual endpoints that fail are logged and left empty, so a
/// server version without one of them still yields a usable snapshot.
pub async fn snapshot(
    backend: super::Backend,
    session_id: Option<String>,
    cwd: PathBuf,
) -> Result<SessionSnapshot, String> {
    let cwd = prepare_session_cwd(&cwd, CwdStrictness::Substitute);
    let server = Server::start(&cwd, backend)?;
    let result = snapshot_from(&server, session_id.as_deref()).await;
    server.shutdown();
    result
}

async fn snapshot_from(
    server: &Server,
    session_id: Option<&str>,
) -> Result<SessionSnapshot, String> {
    server.ensure_ready().await?;
    let http = server.http();

    let commands = match http.get::<Vec<api::Command>>("/command").await {
        Ok(commands) => commands
            .into_iter()
            .filter(|command| !command.name.is_empty())
            .map(|command| AgentCommand {
                name: command.name,
                description: command.description.unwrap_or_default(),
                input_hint: None,
            })
            .collect(),
        Err(error) => {
            log::debug!("opencode: /command probe failed: {error}");
            Vec::new()
        }
    };

    let modes = available_modes(http).await.unwrap_or_else(|error| {
        log::debug!("opencode: /agent probe failed: {error}");
        Vec::new()
    });

    // The session records the agent and model it runs with; when it is not
    // available (no session id yet), the server's own defaults apply.
    let session = match session_id.filter(|id| !id.is_empty()) {
        Some(id) => http
            .get_optional::<api::Session>(&format!("/session/{id}"))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let current_model = session.as_ref().and_then(|session| {
        session
            .model
            .as_ref()
            .map(|model| format!("{}/{}", model.provider_id, model.id))
    });
    let config_options = model_option(http, current_model.as_deref())
        .await
        .into_iter()
        .collect();
    let current_mode = session
        .as_ref()
        .and_then(|session| session.agent.clone())
        .unwrap_or_default();

    Ok(SessionSnapshot {
        commands,
        config_options,
        modes,
        current_mode,
    })
}

pub(super) async fn available_modes(http: &Http) -> Result<Vec<AgentSessionMode>, String> {
    let agents = http.get::<Vec<api::Agent>>("/agent").await?;
    Ok(agents
        .into_iter()
        .filter(|agent| {
            agent.mode == "primary"
                && !agent.name.is_empty()
                && !INTERNAL_AGENTS.contains(&agent.name.as_str())
        })
        .map(|agent| AgentSessionMode {
            id: agent.name.clone(),
            name: agent.name,
            description: agent.description,
        })
        .collect())
}

/// The `model` config option, built from the provider catalog and the
/// session/configured default. `None` when the server serves no models.
async fn model_option(
    http: &super::http::Http,
    session_model: Option<&str>,
) -> Option<AgentConfigOption> {
    let providers = match http.get::<api::Providers>("/config/providers").await {
        Ok(providers) => providers,
        Err(error) => {
            log::debug!("opencode: /config/providers probe failed: {error}");
            return None;
        }
    };
    let mut values: Vec<AgentConfigValue> = Vec::new();
    for provider in &providers.providers {
        let provider_id = if provider.id.is_empty() {
            provider.name.clone()
        } else {
            provider.id.clone()
        };
        if provider_id.is_empty() {
            continue;
        }
        let provider_name = if provider.name.is_empty() {
            provider_id.clone()
        } else {
            provider.name.clone()
        };
        for (key, model) in &provider.models {
            // The map key IS the model id the prompt body addresses; the
            // nested `id` is a display duplicate that some versions omit.
            let model_id = if model.id.is_empty() {
                key.as_str()
            } else {
                model.id.as_str()
            };
            if model_id.is_empty() {
                continue;
            }
            let model_name = if model.name.is_empty() {
                model_id
            } else {
                model.name.as_str()
            };
            values.push(AgentConfigValue {
                value: format!("{provider_id}/{model_id}"),
                name: format!("{provider_name} / {model_name}"),
            });
        }
    }
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.value.cmp(&b.value));
    values.dedup_by(|a, b| a.value == b.value);
    let offered = |model: &str| values.iter().any(|value| value.value == model);
    // Priority: what the session actually runs (the server records it), then
    // the configured default, then the catalog default.
    let current = match session_model.filter(|model| offered(model)) {
        Some(model) => model.to_string(),
        None => {
            let configured = match http.get::<api::Config>("/config").await {
                Ok(config) => config.model.filter(|model| offered(model)),
                Err(error) => {
                    log::debug!("opencode: /config probe failed: {error}");
                    None
                }
            };
            configured
                .or_else(|| {
                    // The catalog's own default is a `provider → model` map;
                    // rebuild the `provider/model` spelling and only use it
                    // when the option really offers that value.
                    providers
                        .default
                        .iter()
                        .map(|(provider, model)| format!("{provider}/{model}"))
                        .find(|value| offered(value))
                })
                .unwrap_or_default()
        }
    };
    Some(AgentConfigOption {
        id: "model".to_string(),
        name: "Model".to_string(),
        description: Some("Model served by OpenCode for this session".to_string()),
        category: Some("model".to_string()),
        values,
        current,
        boolean: None,
    })
}

/// Stream a session's retained conversation into the pane's event channel.
///
/// The events are the same shapes the live turn produces, so a rehydrated
/// pane renders exactly like a live one.
///
/// # Errors
///
/// Returns an error when the server cannot start, the session is gone, or
/// the message list cannot be read.
pub async fn replay(
    backend: super::Backend,
    session_id: &str,
    cwd: PathBuf,
    chunk_tx: tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(), String> {
    let cwd = prepare_session_cwd(&cwd, CwdStrictness::Substitute);
    let server = Server::start(&cwd, backend)?;
    let result = replay_from(&server, session_id, &chunk_tx).await;
    server.shutdown();
    result
}

async fn replay_from(
    server: &Server,
    session_id: &str,
    chunk_tx: &tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
) -> Result<(), String> {
    server.ensure_ready().await?;
    let path = format!("/session/{session_id}/message");
    let messages = server
        .http()
        .get_optional::<Vec<api::MessageWithParts>>(&path)
        .await?
        .ok_or_else(|| format!("opencode: session '{session_id}' no longer exists"))?;
    let mut mapper = Mapper::new(true);
    for message in messages {
        // Roles are registered first: the parts rely on them to tell the
        // user's side of the conversation from the assistant's.
        mapper.observe_message(&message.info);
        for part in message.parts {
            for event in mapper.observe_part(&part, None) {
                if chunk_tx.send(event).is_err() {
                    return Ok(());
                }
            }
        }
    }
    // Chrome the pane also shows after a rehydration: the session title and
    // the plan the work had already reached.
    if let Ok(Some(session)) = server
        .http()
        .get_optional::<api::Session>(&format!("/session/{session_id}"))
        .await
        && !session.title.is_empty()
    {
        let _ = chunk_tx.send(SubagentEvent::SessionInfo {
            title: Some(session.title),
        });
    }
    if let Ok(todos) = server
        .http()
        .get::<Vec<api::Todo>>(&format!("/session/{session_id}/todo"))
        .await
    {
        let entries: Vec<_> = todos
            .iter()
            .filter(|todo| !todo.content.is_empty())
            .map(super::event::plan_entry)
            .collect();
        if !entries.is_empty() {
            let _ = chunk_tx.send(SubagentEvent::Plan { entries });
        }
    }
    Ok(())
}
