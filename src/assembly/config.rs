//! Shared session settings for surfaces using the harness assembly.

use std::collections::{HashMap, HashSet};

use aiface::connector::{ToolCallMode, normalize_local_base_url};

use crate::assembly::HarnessAssembly;
use crate::harness::{ContextManagerState, Mode};
use crate::mcp::McpConfig;
use crate::setup::{Setup, SkillsConfig};

/// Everything needed to assemble a chat's harness, with the caller's
/// per-chat differences (mode, context, cache key) kept out.
#[derive(Clone)]
pub struct AgentConfig {
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
    #[cfg(feature = "onnx")]
    pub decision: crate::setup::DecisionConfig,
}

impl AgentConfig {
    /// Resolve the configuration for a chat whose working directory is `cwd`.
    ///
    /// Reads `setup.json` (the same file the TUI writes) and the environment
    /// defaults, including the model and route chosen in the TUI.
    pub fn load(cwd: &str) -> Self {
        let setup = Setup::load();
        Self::initialize(&setup, cwd)
    }

    /// Resolve settings and initialize the process-wide registries.
    pub fn initialize(setup: &Setup, cwd: &str) -> Self {
        // Process-wide registration, mirroring `App::new`: custom ACP agents
        // and custom providers must be published before any harness is
        // assembled, and the LSP switch is a process-wide flag. Doing this
        // here (rather than in `from_setup`) keeps the pure resolver
        // testable.
        for error in morpheus_agents::acp::register_custom_agents_lossy(&setup.acp_agent_pairs()) {
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
        Self::from_setup(setup, cwd)
    }

    /// [`Self::load`] with the setup injected: the testable core.
    #[must_use]
    pub fn from_setup(setup: &Setup, cwd: &str) -> Self {
        // Environment defaults come first; a persisted TUI selection wins
        // over them. Assembly surfaces missing credentials as turn errors.
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
            tool_call_mode: crate::tools_config::load_tool_call_mode(setup),
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
            disabled_tools: crate::tools_config::load_disabled_tools(
                setup,
                cwd,
                setup.tools.is_global(),
            ),
            initial_mode: setup.persisted_mode().unwrap_or(Mode::Build),
            #[cfg(feature = "onnx")]
            decision: setup.decision.clone(),
        }
    }

    /// Build the assembly input for one turn of one chat.
    ///
    /// `ctx_state` is the chat's model-facing history (it is never read from
    /// the visual transcript); `cache_key` is the chat's opaque cache
    /// affinity key, so two chats never compete for the same warm backend.
    #[must_use]
    pub fn assembly(
        &self,
        mode: Mode,
        ctx_state: Option<ContextManagerState>,
        cache_key: Option<String>,
    ) -> HarnessAssembly {
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
            disabled_tools: self.disabled_tools.clone(),
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
            user_images: Vec::new(),
            as_manager: false,
            own_coworker_id: None,
        }
    }
}
