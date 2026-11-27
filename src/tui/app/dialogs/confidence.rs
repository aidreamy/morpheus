use morpheus::agent_profiles::{AgentProfile, CONFIDENCE_USAGE, Confidence, ConfidenceCommand};

use crate::component::list_dialog::FilterState;
use crate::routes::session::agents_split::{AgentsSplitState, MANAGER_LABEL};
use crate::ui::{
    dialogs::DialogType,
    toast::{ToastOptions, ToastVariant},
};

use super::super::App;
use super::super::agents_dispatch::{either_split, pane_effective_cwd};

pub(in crate::app) enum ConfidenceEdit {
    Agent,
    Level { pane_id: u64, profile: AgentProfile },
}

impl App {
    pub(in crate::app) fn pane_profile(
        &self,
        split: &AgentsSplitState,
        index: usize,
    ) -> Option<AgentProfile> {
        let pane = split.panes.get(index)?;
        if pane.agent == MANAGER_LABEL {
            return None;
        }
        let info = pane.session_info.as_ref();
        let options = pane.config_options.as_deref().unwrap_or_default();
        let configured = morpheus::coordinator::turn::model_option_id(options)
            .and_then(|id| options.iter().find(|option| option.id == id))
            .map(|option| option.current.as_str());
        let model = info
            .and_then(|info| info.model.as_deref())
            .filter(|model| !model.is_empty())
            .or_else(|| configured.filter(|model| !model.is_empty()))
            .or_else(|| self.agents_model_picks.get(&pane.agent))
            .map(str::to_owned);
        let mut profile = AgentProfile {
            id: split.pane_registry_id(index)?,
            agent: pane.agent.clone(),
            model,
            provider: info.and_then(|info| info.provider.clone()),
            reasoning: options
                .iter()
                .find(|option| {
                    (option.id == "reasoning"
                        || option.category.as_deref() == Some("thought_level"))
                        && !option.current.is_empty()
                })
                .map(|option| option.current.clone())
                .or_else(|| info.and_then(|info| info.reasoning.clone())),
            mode: format!("{:?}", self.state.mode),
            session_mode: (!pane.mode_current.is_empty()).then(|| pane.mode_current.clone()),
            cwd: pane_effective_cwd(pane, &self.state.working_directory),
            worktree: pane.worktree.as_ref().map(|tree| tree.name.clone()),
            busy: pane.running || pane.rehydrating,
            bound: pane.session_id.is_some(),
            commands: pane
                .commands
                .iter()
                .flatten()
                .map(|command| command.name.clone())
                .collect(),
            observed: pane.observed_tools.clone(),
            ..Default::default()
        };
        profile.confidence = self.setup.agent_confidence.get(&profile);
        Some(profile)
    }

    fn confidence_targets(&self) -> Vec<(u64, AgentProfile)> {
        either_split(&self.agents_split, &self.parked_agents_split)
            .map(|split| {
                split
                    .panes
                    .iter()
                    .enumerate()
                    .filter_map(|(index, pane)| {
                        self.pane_profile(split, index)
                            .map(|profile| (pane.id, profile))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(in crate::app) fn run_confidence_command(&mut self, args: &str) {
        let targets = self.confidence_targets();
        if targets.is_empty() {
            self.toast_error("Select at least one external agent in /agents.");
            return;
        }
        let command = match ConfidenceCommand::parse(args) {
            Ok(command) => command,
            Err(_) => {
                self.toast_error(CONFIDENCE_USAGE);
                return;
            }
        };
        let (target, selection) = match command {
            ConfidenceCommand::Show(None) => {
                let focused = self
                    .agents_split
                    .as_ref()
                    .and_then(|split| split.focused.and_then(|index| split.panes.get(index)))
                    .map(|pane| pane.id);
                if let Some((id, profile)) = targets
                    .iter()
                    .find(|(id, _)| Some(*id) == focused)
                    .or_else(|| (targets.len() == 1).then(|| &targets[0]))
                {
                    self.open_confidence_level(*id, profile.clone());
                } else {
                    self.confidence_edit = Some(ConfidenceEdit::Agent);
                    self.open_confidence_list(
                        "Agent confidence",
                        targets
                            .iter()
                            .map(|(id, profile)| (id.to_string(), profile.summary()))
                            .collect(),
                        "",
                    );
                }
                return;
            }
            ConfidenceCommand::Show(Some(target)) => (target, None),
            ConfidenceCommand::Set { agent, level } => (agent, Some(level)),
        };
        if let Some((id, profile)) = targets
            .into_iter()
            .find(|(_, profile)| profile.id.eq_ignore_ascii_case(&target))
        {
            if let Some(level) = selection {
                self.save_agent_confidence(&profile, level);
            } else {
                self.open_confidence_level(id, profile);
            }
        } else {
            self.toast_error("Choose an external agent by its ID from /confidence.");
        }
    }

    fn open_confidence_level(&mut self, pane_id: u64, profile: AgentProfile) {
        let title = format!("Confidence · {}", profile.id);
        let level = profile.confidence.map_or("reset", Confidence::label);
        let levels = [
            ("reset", "Unrated"),
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("max", "Max"),
        ];
        self.confidence_edit = Some(ConfidenceEdit::Level { pane_id, profile });
        self.open_confidence_list(
            &title,
            levels
                .into_iter()
                .map(|(key, label)| (key.to_string(), label.to_string()))
                .collect(),
            level,
        );
    }

    fn open_confidence_list(&mut self, title: &str, options: Vec<(String, String)>, current: &str) {
        self.agent_config_edit = None;
        let mut filter = FilterState::new();
        filter.selected = options
            .iter()
            .position(|(key, _)| key == current)
            .unwrap_or(0);
        self.dialog.replace(DialogType::AgentConfigList {
            title: title.into(),
            options,
            current: current.into(),
            search: false,
            filter,
        });
    }

    pub(in crate::app) fn apply_confidence_selection(
        &mut self,
        edit: ConfidenceEdit,
        selection: &str,
    ) {
        let targets = self.confidence_targets();
        match edit {
            ConfidenceEdit::Agent => {
                if let Ok(id) = selection.parse::<u64>()
                    && let Some((_, profile)) =
                        targets.into_iter().find(|(target, _)| *target == id)
                {
                    self.open_confidence_level(id, profile);
                }
            }
            ConfidenceEdit::Level { pane_id, profile } => {
                if targets.iter().any(|(id, current)| {
                    *id == pane_id
                        && current.agent == profile.agent
                        && current.model == profile.model
                        && current.provider == profile.provider
                }) {
                    self.save_agent_confidence(&profile, Confidence::parse(selection));
                } else {
                    self.toast_error(
                        "The selected agent or model changed. Open /confidence again.",
                    );
                }
            }
        }
    }

    fn save_agent_confidence(&mut self, profile: &AgentProfile, confidence: Option<Confidence>) {
        self.setup.agent_confidence.set(profile, confidence);
        if self.setup.save() {
            self.toast_state.show(ToastOptions {
                title: Some("Confidence".into()),
                message: format!(
                    "{} confidence: {}",
                    profile.id,
                    confidence.map_or("unrated", Confidence::label)
                ),
                variant: ToastVariant::Info,
                duration_ms: 3000,
            });
        } else {
            self.toast_error("Confidence changed for this run. Could not save setup.json.");
        }
        self.sync_agents_pane_registry();
    }

    fn toast_error(&mut self, message: &str) {
        self.toast_state.show(ToastOptions {
            title: Some("Confidence".into()),
            message: message.into(),
            variant: ToastVariant::Warning,
            duration_ms: 4000,
        });
    }
}
