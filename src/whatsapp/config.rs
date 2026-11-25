//! The immutable agent configuration a WhatsApp chat assembles from.
//!
//! The WhatsApp interface runs the SAME harness assembly as the TUI
//! (`crate::assembly`). This module is the bridge between the user's
//! persisted settings (`setup.json` / environment) and a
//! [`HarnessAssembly`]: it is resolved once at startup and cloned into
//! every chat worker, so each conversation gets an identical, independent
//! agent loop.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use aiface::connector::{ToolCallMode, normalize_local_base_url};

use crate::assembly::HarnessAssembly;
use crate::harness::{ContextManagerState, Mode};
use crate::mcp::McpConfig;
use crate::setup::{Setup, SkillsConfig};

use super::access::Access;

/// Everything needed to assemble a chat's harness, with the caller's
/// per-chat differences (mode, context, cache key) kept out.
#[derive(Clone)]
pub struct RuntimeConfig {
    pub provider: String,
    pub model: Option<String>,
    pub reasoning: Option<String>,
    pub tool_call_mode: ToolCallMode,
    pub base_url: Option<String>,
    pub local_base_urls: HashMap<String, String>,
    pub fallbacks: Vec<(String, String)>,
    pub summarization_models: Vec<(String, String)>,
    pub cwd: String,
    pub mcp_config: McpConfig,
    pub skills_config: SkillsConfig,
    pub effective_context: bool,
    pub anthropic_ttl_1h: bool,
    pub openai_cache_retention: Option<String>,
    pub disabled_tools: HashSet<String>,
    /// The mode a fresh chat starts in — the user's last global selection,
    /// exactly like the TUI restores it.
    pub initial_mode: Mode,
    /// Who may reach the agent (`setup.json` → `whatsapp`).
    pub access: Access,
    /// Simulated typing for chats that never ran `/human`
    /// (`setup.json` → `whatsapp.humanize`).
    pub humanize_default: bool,
    #[cfg(feature = "onnx")]
    pub decision: crate::setup::DecisionConfig,
}

impl RuntimeConfig {
    /// Resolve the configuration for a chat whose working directory is `cwd`.
    ///
    /// Reads `setup.json` (the same file the TUI writes) and the environment
    /// defaults, so a model/route chosen in the TUI is what WhatsApp uses.
    pub fn load(cwd: &str) -> Self {
        let setup = Setup::load();
        // Process-wide registration, mirroring `App::new`: custom ACP agents
        // and custom providers must be published before any harness is
        // assembled, and the LSP switch is a process-wide flag. Doing this
        // here (rather than in `from_setup`) keeps the pure resolver
        // testable.
        for error in
            morpheus_tools::subagent::acp::register_custom_agents_lossy(&setup.acp_agent_pairs())
        {
            log::warn!("user-registered ACP agent skipped: {error}");
        }
        if let Err(error) = aiface::connector::register_custom_providers(
            setup
                .custom_provider_triples()
                .into_iter()
                .map(|(name, base_url, family)| (name, base_url, family.sdk_family()))
                .collect(),
        ) {
            log::warn!("user-registered providers not published: {error}");
        }
        crate::harness::lsp::set_lsp_enabled(setup.lsp);
        Self::from_setup(&setup, cwd)
    }

    /// [`Self::load`] with the setup injected: the testable core.
    #[must_use]
    pub fn from_setup(setup: &Setup, cwd: &str) -> Self {
        // Environment defaults come first; a persisted TUI selection wins
        // over them, mirroring `App::new`. Unlike the TUI we do not probe
        // the credential store here: assembly reports a missing credential
        // as a turn error, which the user reads in the chat.
        let env = crate::config::LlmConfig::from_env();
        let persisted_model = setup.persisted_model().map(str::to_string);
        let provider = if setup.model.provider.is_empty() {
            env.provider
        } else {
            setup.model.provider.clone()
        };
        let reasoning = if persisted_model.is_some() {
            setup.model.reasoning.clone()
        } else {
            env.reasoning
        };
        let base_url = setup
            .local_base_url(&provider)
            .map(|url| normalize_local_base_url(&provider, url));
        let local_base_urls = setup
            .providers
            .local
            .iter()
            .map(|(name, endpoint)| {
                (
                    name.clone(),
                    normalize_local_base_url(name, &endpoint.base_url),
                )
            })
            .collect();

        Self {
            provider,
            model: persisted_model.or(env.model),
            reasoning,
            tool_call_mode: match setup.tools.tool_call_mode.as_str() {
                "inline" => ToolCallMode::Inline,
                _ => ToolCallMode::Native,
            },
            base_url,
            local_base_urls,
            fallbacks: crate::fallback::load_fallbacks(setup)
                .into_iter()
                .map(|entry| (entry.provider, entry.model))
                .collect(),
            summarization_models: setup
                .routing
                .summarization_models
                .iter()
                .map(|entry| (entry.provider.clone(), entry.model.clone()))
                .collect(),
            cwd: cwd.to_string(),
            mcp_config: setup.mcp.clone(),
            skills_config: setup.skills.clone(),
            effective_context: setup.effective_context,
            anthropic_ttl_1h: setup.anthropic_cache_ttl_1h(),
            openai_cache_retention: setup.openai_cache_retention().map(String::from),
            disabled_tools: load_disabled_tools(setup, cwd),
            initial_mode: setup.persisted_mode().unwrap_or(Mode::Build),
            access: Access::new(setup.whatsapp.clone()),
            humanize_default: setup.whatsapp.humanize,
            #[cfg(feature = "onnx")]
            decision: setup.decision.clone(),
        }
    }

    /// Build the assembly input for one turn of one chat.
    ///
    /// `ctx_state` is the chat's model-facing history (it is never read from
    /// the WhatsApp transcript); `cache_key` is the chat's opaque cache
    /// affinity key, so two chats never compete for the same warm backend.
    #[must_use]
    pub fn assembly(
        &self,
        mode: Mode,
        ctx_state: Option<ContextManagerState>,
        cache_key: Option<String>,
    ) -> HarnessAssembly {
        // `ask_questions` is an interactive TUI dialog; over WhatsApp there
        // is nobody to answer it. Hiding the tool keeps the model from
        // planning around a flow this surface cannot complete.
        let mut disabled_tools = self.disabled_tools.clone();
        disabled_tools.insert("ask_questions".to_string());

        HarnessAssembly {
            provider: self.provider.clone(),
            model: self.model.clone(),
            reasoning: self.reasoning.clone(),
            tool_call_mode: self.tool_call_mode,
            base_url: self.base_url.clone(),
            local_base_urls: self.local_base_urls.clone(),
            fallbacks: self.fallbacks.clone(),
            summarization_models: self.summarization_models.clone(),
            cwd: self.cwd.clone(),
            mode,
            disabled_tools,
            instructions: None,
            anthropic_ttl_1h: self.anthropic_ttl_1h,
            openai_cache_retention: self.openai_cache_retention.clone(),
            cache_key,
            mcp_config: self.mcp_config.clone(),
            skills_config: self.skills_config.clone(),
            effective_context: self.effective_context,
            #[cfg(feature = "onnx")]
            checkup: Some(self.decision.clone()),
            // Recall databases are wired by the TUI's App state; without
            // them the assembly hides `recall_search` instead of offering a
            // tool with no databases behind it.
            #[cfg(feature = "embed")]
            recall: None,
            ctx_state,
            // WhatsApp replies are text-only for now: no image rides along
            // with the turn's prompt.
            user_images: Vec::new(),
            // A WhatsApp chat is a normal main loop: no manager requester
            // tag and no coworker pane binding.
            as_manager: false,
            own_coworker_id: None,
        }
    }
}

/// The disabled-tool set for this project, with the same scope semantics as
/// the TUI: `setup.json` when the user chose the global scope, otherwise
/// `<cwd>/.morpheus-tools.json` (falling back to the legacy global list until
/// the project saves one of its own).
fn load_disabled_tools(setup: &Setup, cwd: &str) -> HashSet<String> {
    if setup.tools.is_global() {
        return setup.tools.disabled.iter().cloned().collect();
    }
    match std::fs::read_to_string(Path::new(cwd).join(".morpheus-tools.json")) {
        Ok(text) => parse_project_tools(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            setup.tools.disabled.iter().cloned().collect()
        }
        Err(_) => HashSet::new(),
    }
}

#[derive(Default, serde::Deserialize)]
struct ProjectTools {
    #[serde(default)]
    disabled: Vec<String>,
}

fn parse_project_tools(text: &str) -> HashSet<String> {
    serde_json::from_str::<ProjectTools>(text)
        .map(|project| project.disabled.into_iter().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembly_hides_ask_questions_and_disables_recall_without_databases() {
        let setup = Setup::default();
        let config = RuntimeConfig::from_setup(&setup, "/tmp/project");
        let assembly = config.assembly(Mode::Build, None, Some("chat".into()));

        assert!(assembly.disabled_tools.contains("ask_questions"));
        assert_eq!(assembly.mode, Mode::Build);
        assert_eq!(assembly.cache_key.as_deref(), Some("chat"));
        #[cfg(feature = "embed")]
        assert!(assembly.disabled_tools.contains("recall_search"));
    }

    #[test]
    fn the_configured_mode_is_the_chat_starting_point() {
        let mut setup = Setup::default();
        setup.mode.mode = "manager".to_string();
        assert_eq!(
            RuntimeConfig::from_setup(&setup, "/tmp").initial_mode,
            Mode::Manager
        );
    }

    #[test]
    fn project_tool_files_are_parsed_and_corruption_fails_open() {
        assert_eq!(
            parse_project_tools(r#"{"disabled":["bash_run","fs_write"]}"#),
            HashSet::from(["bash_run".to_string(), "fs_write".to_string()])
        );
        assert!(parse_project_tools("not json").is_empty());
        assert!(parse_project_tools("{}").is_empty());
    }
}
