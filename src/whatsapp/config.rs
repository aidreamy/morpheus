//! WhatsApp settings and the surface's harness options.

use crate::assembly::{HarnessAssembly, config::AgentConfig};
use crate::harness::{ContextManagerState, Mode};
use crate::setup::Setup;

use super::access::Access;

#[derive(Clone)]
pub struct RuntimeConfig {
    pub agent: AgentConfig,
    pub access: Access,
    pub humanize_default: bool,
}

impl RuntimeConfig {
    pub fn load(cwd: &str) -> Self {
        let setup = Setup::load();
        Self::new(AgentConfig::initialize(&setup, cwd), &setup)
    }

    #[cfg(test)]
    pub fn from_setup(setup: &Setup, cwd: &str) -> Self {
        Self::new(AgentConfig::from_setup(setup, cwd), setup)
    }

    fn new(agent: AgentConfig, setup: &Setup) -> Self {
        Self {
            agent,
            access: Access::new(setup.whatsapp.clone()),
            humanize_default: setup.whatsapp.humanize,
        }
    }

    pub fn assembly(
        &self,
        mode: Mode,
        context: Option<ContextManagerState>,
        cache_key: Option<String>,
    ) -> HarnessAssembly {
        let mut assembly = self.agent.assembly(mode, context, cache_key);
        assembly.disabled_tools.insert("ask_questions".into());
        assembly
    }
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
        assert!(assembly.recall.is_none());
    }

    #[test]
    fn the_configured_mode_is_the_chat_starting_point() {
        let mut setup = Setup::default();
        setup.mode.mode = "manager".to_string();
        assert_eq!(
            RuntimeConfig::from_setup(&setup, "/tmp").agent.initial_mode,
            Mode::Manager
        );
    }
}
