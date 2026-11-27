//! Per-chat control state: mode, roster, sessions, context and stop signal.
//!
//! Every chat has exactly one [`ChatControl`]. It is created on the first
//! message from that chat and shared by the router (which answers commands and
//! routes messages), the turn worker (which reads the mode and supplies ACP
//! handles) and the executors. Slash commands mutate it immediately; they
//! never enter the prompt queue, so `/agent ls` answers while a long turn is
//! still running.
//!
//! The roster, ACP session ids and main-agent context are persisted on every
//! change. A failed write is reported back to the user (the in-memory state
//! stays authoritative for the running process).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use morpheus_agents::acp::{self, SessionHandle};
use morpheus_agents::model_picks::ModelPicks;

use crate::harness::{ContextManagerState, Mode};
use crate::setup::{Setup, data_dir_override};

use super::commands::{self, Action, AgentCommand, IncomingCommand};
use super::mode;
use super::selection;
use super::state::{ChatStore, STATE_VERSION, StoredChat};
use crate::agent_profiles::{
    AgentObservation, AgentProfile, Confidence, ConfidenceCommand, ConfidencePreferences,
};

/// Mode, roster, sessions, context and cancellation for one chat.
pub struct ChatControl {
    store: ChatStore,
    chat_key: String,
    mode: Mode,
    roster: Vec<String>,
    installed: Vec<String>,
    registered: Vec<String>,
    sessions: BTreeMap<String, String>,
    context: Option<ContextManagerState>,
    confidence: ConfidencePreferences,
    observations: HashMap<String, AgentObservation>,
    /// The `setup.json` default for simulated typing, used until the chat
    /// makes its own `/human` choice.
    humanize_default: bool,
    /// The chat's own `/human` choice, persisted; `None` until it is used.
    humanize_choice: Option<bool>,
    /// Where `/agent model` writes its picks (injected by tests).
    model_dir: PathBuf,
    /// Live handles, for `/agent off` and `/agent reset`. The worker also
    /// holds clones; shutting one down stops the shared supervisor.
    handles: HashMap<String, SessionHandle>,
    /// The cancellation flag shared by the turns of this chat. `/stop` trips
    /// it and installs a fresh one for the next turn.
    stop: Arc<AtomicBool>,
    running: usize,
}

impl ChatControl {
    /// Load a chat's persisted state, resolving the installed agents now.
    pub async fn load(
        store: ChatStore,
        chat_key: String,
        initial_mode: Mode,
        humanize_default: bool,
    ) -> Self {
        let installed = acp::installed_agents_snapshot()
            .into_iter()
            .map(str::to_string)
            .collect();
        let registered = acp::all_agent_names()
            .into_iter()
            .map(str::to_string)
            .collect();
        Self::with_agents_at(
            store,
            chat_key,
            initial_mode,
            installed,
            registered,
            data_dir_override().join("agents"),
            humanize_default,
        )
        .await
    }

    /// [`Self::load`] with the agent registry, model-pick directory and the
    /// simulated-typing default injected: the testable core.
    pub async fn with_agents_at(
        store: ChatStore,
        chat_key: String,
        initial_mode: Mode,
        installed: Vec<String>,
        registered: Vec<String>,
        model_dir: PathBuf,
        humanize_default: bool,
    ) -> Self {
        let state = store.load(&chat_key).await;
        Self {
            store,
            chat_key,
            mode: initial_mode,
            roster: selection::sanitize_roster(state.roster),
            installed,
            registered,
            sessions: state.sessions,
            context: state.context,
            confidence: state.confidence,
            observations: HashMap::new(),
            humanize_default,
            humanize_choice: state.humanize,
            model_dir,
            handles: HashMap::new(),
            stop: Arc::new(AtomicBool::new(false)),
            running: 0,
        }
    }

    /// The mode the next turn runs in.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The selected instances (canonical ids; a name may repeat).
    #[must_use]
    pub fn roster(&self) -> &[String] {
        &self.roster
    }

    /// Whether replies in this chat are preceded by a simulated typing state:
    /// the chat's own `/human` choice, else the `setup.json` default.
    #[must_use]
    pub fn humanize(&self) -> bool {
        self.humanize_choice.unwrap_or(self.humanize_default)
    }

    /// Installed and registered agents, snapshotted at chat creation.
    #[must_use]
    pub fn installed(&self) -> &[String] {
        &self.installed
    }

    /// The model-facing context, cloned for a turn.
    #[must_use]
    pub fn context(&self) -> Option<ContextManagerState> {
        self.context.clone()
    }

    /// The stored ACP session id for one instance.
    #[must_use]
    pub fn session_id(&self, key: &str) -> Option<String> {
        self.sessions.get(key).cloned()
    }

    /// The cancellation flag the next turn must share.
    #[must_use]
    pub fn stop_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Mark a turn as running, for `/stop`'s idle check.
    pub fn begin_turn(&mut self) {
        self.running += 1;
    }

    /// Mark a turn as finished.
    pub fn end_turn(&mut self) {
        self.running = self.running.saturating_sub(1);
    }

    /// Record a live handle so `/agent off` and `/agent reset` can close it.
    pub fn register_handle(&mut self, key: String, handle: SessionHandle) {
        self.handles.insert(key, handle);
    }

    /// Persist the main agent's context after a turn.
    pub async fn set_context(
        &mut self,
        context: Option<ContextManagerState>,
    ) -> Result<(), String> {
        self.context = context;
        self.persist().await
    }

    /// Persist one instance's ACP session id (only when the turn produced
    /// one; a failed turn leaves the previous binding in place).
    pub async fn record_session(&mut self, key: &str, id: String) -> Result<(), String> {
        // A turn that finishes after `/agent off` or after its agent was
        // deselected must not resurrect a session id for a removed agent.
        let agent = key.split('#').next().unwrap_or_default();
        if !self.roster.iter().any(|name| name == agent) {
            return Ok(());
        }
        self.sessions.insert(key.to_string(), id);
        self.persist().await
    }

    /// `/stop`: cancel every turn running in this chat.
    pub fn stop_running(&mut self) -> String {
        if self.running == 0 {
            return "Nothing is running.".to_string();
        }
        let fresh = Arc::new(AtomicBool::new(false));
        let old = std::mem::replace(&mut self.stop, fresh);
        old.store(true, Ordering::SeqCst);
        "Stopped.".to_string()
    }

    /// Answer one slash command. Never runs a turn.
    pub async fn handle_command(&mut self, command: IncomingCommand) -> String {
        match command.action() {
            Action::Help => commands::help_text(),
            Action::SetMode(mode) => self.set_mode(mode),
            Action::Agent(command) => self.agent_command(command).await,
            Action::Confidence(command) => self.confidence_command(command).await,
            Action::Humanize(enabled) => self.human_command(enabled).await,
            Action::Stop => self.stop_running(),
            Action::Usage(message) => message,
        }
    }

    /// The `/mode` behavior, unchanged from the single-agent surface: the
    /// selection is a global preference, restored on the next start.
    fn set_mode(&mut self, mode: Mode) -> String {
        self.mode = mode;
        let mut setup = Setup::load();
        setup.set_mode_selection(mode);
        if mode == Mode::Command {
            "Mode set to *command*. Send a shell command as your next message; \
             no LLM call is made. Use `/mode build` to return to the agent."
                .to_string()
        } else {
            format!(
                "Mode set to *{}*.\n{}",
                mode::name(mode),
                mode::description(mode)
            )
        }
    }

    /// `/human`: report or persist the simulated-typing choice.
    async fn human_command(&mut self, enabled: Option<bool>) -> String {
        let current = self.humanize();
        let Some(enabled) = enabled else {
            return format!(
                "Simulated typing is *{}*. Send `/human on` or `/human off` to change it.",
                if current { "on" } else { "off" }
            );
        };
        if enabled == current {
            return format!(
                "Simulated typing is already *{}*.",
                if enabled { "on" } else { "off" }
            );
        }

        let previous = self.humanize_choice;
        self.humanize_choice = Some(enabled);
        match self.persist().await {
            Ok(()) => {
                if enabled {
                    "Simulated typing is *on* for this chat. Replies will show a “typing…” \
                     state for about 1–2.5 seconds first; commands stay immediate. The text \
                     and timing of the answer are unchanged."
                        .to_string()
                } else {
                    "Simulated typing is *off* for this chat. Replies are sent immediately."
                        .to_string()
                }
            }
            Err(error) => {
                self.humanize_choice = previous;
                format!("Could not save the typing setting: {error}")
            }
        }
    }

    /// The selected instances, with the model and session state `/agent ls`
    /// shows. Resolved here because rendering must not touch the disk.
    fn summaries(&self) -> Vec<selection::AgentSummary> {
        selection::entries(&self.roster)
            .into_iter()
            .map(|entry| selection::AgentSummary {
                model: self.model_pick(&entry.agent),
                session: self
                    .sessions
                    .contains_key(&selection::instance_key(&entry.agent, entry.index)),
                agent: entry.agent,
                index: entry.index,
                total: entry.total,
            })
            .collect()
    }

    /// The persisted model pick for `agent`, without creating the store.
    fn model_pick(&self, agent: &str) -> Option<String> {
        if !self.model_dir.exists() {
            return None;
        }
        ModelPicks::new(&self.model_dir)
            .get(agent)
            .map(str::to_string)
    }

    pub fn agent_profiles(&self, cwd: &std::path::Path) -> Vec<AgentProfile> {
        let picks = ModelPicks::new(&self.model_dir);
        selection::entries(&self.roster)
            .into_iter()
            .map(|entry| {
                let key = selection::instance_key(&entry.agent, entry.index);
                let specified = picks.get(&entry.agent);
                let observed = self.observations.get(&key).filter(|observed| {
                    specified.is_none_or(|model| observed.model.as_deref() == Some(model))
                });
                let mut profile = AgentProfile {
                    id: if entry.total > 1 {
                        key.clone()
                    } else {
                        entry.agent.clone()
                    },
                    agent: entry.agent,
                    model: observed
                        .and_then(|info| info.model.clone())
                        .or_else(|| specified.map(str::to_owned)),
                    provider: observed.and_then(|info| info.provider.clone()),
                    reasoning: observed.and_then(|info| info.reasoning.clone()),
                    mode: format!("{:?}", self.mode),
                    cwd: cwd.to_path_buf(),
                    bound: self.sessions.contains_key(&key),
                    commands: observed
                        .and_then(|info| info.commands.clone())
                        .unwrap_or_default(),
                    observed: observed.map(|info| info.tools.clone()).unwrap_or_default(),
                    ..Default::default()
                };
                profile.confidence = self.confidence.get(&profile);
                profile
            })
            .collect()
    }

    pub fn record_observation(&mut self, key: &str, observation: AgentObservation) {
        if self.handles.contains_key(key) {
            let previous = self.observations.entry(key.to_string()).or_default();
            previous.merge_from(observation);
        }
    }

    async fn confidence_command(&mut self, command: ConfidenceCommand) -> String {
        let profiles = self.agent_profiles(std::path::Path::new("."));
        if profiles.is_empty() {
            return "Select an external agent with `/agent` first.".into();
        }
        let find = |name: &str| {
            profiles.iter().find(|profile| {
                profile.id == name
                    || (!profile.id.contains('#')
                        && name.strip_suffix("#1") == Some(profile.id.as_str()))
            })
        };
        match command {
            ConfidenceCommand::Show(None) => profiles
                .iter()
                .map(AgentProfile::summary)
                .collect::<Vec<_>>()
                .join("\n"),
            ConfidenceCommand::Show(Some(name)) => {
                find(&name).map(AgentProfile::summary).unwrap_or_else(|| {
                    "Choose a selected external agent by its ID. `/confidence` lists the targets."
                        .into()
                })
            }
            ConfidenceCommand::Set { agent, level } => {
                let Some(profile) = find(&agent) else {
                    return "Choose a selected external agent by its ID. `/confidence` lists the targets.".into();
                };
                let old = self.confidence.get(profile);
                self.confidence.set(profile, level);
                if let Err(error) = self.persist().await {
                    self.confidence.set(profile, old);
                    return format!("Could not save confidence: {error}");
                }
                format!(
                    "{} confidence for {}: {}.",
                    profile.id,
                    profile.model.as_deref().unwrap_or("the unconfirmed model"),
                    level.map_or("unrated", Confidence::label)
                )
            }
        }
    }

    async fn agent_command(&mut self, command: AgentCommand) -> String {
        match command {
            AgentCommand::List => selection::render_list(&self.summaries(), &self.installed),
            AgentCommand::ListAll => {
                selection::render_all(&self.summaries(), &self.installed, &self.registered)
            }
            AgentCommand::Off => {
                for handle in self.handles.values() {
                    handle.shutdown();
                }
                self.handles.clear();
                self.observations.clear();
                self.roster.clear();
                self.sessions.clear();
                match self.persist().await {
                    Ok(()) => "External agents are off. The main agent is still available without \
                               a prefix."
                        .to_string(),
                    Err(error) => format!("External agents are off, but saving failed: {error}"),
                }
            }
            AgentCommand::Reset(target) => self.reset_session(&target).await,
            AgentCommand::Model { name, model } => self.set_model(&name, &model),
            AgentCommand::ModelStatus(name) => self.model_status(&name),
            AgentCommand::Set(expression) => {
                match selection::parse_roster(&expression, &self.installed) {
                    Ok(roster) => {
                        let previous = std::mem::replace(&mut self.roster, roster);
                        match self.persist().await {
                            Ok(()) => {
                                self.drop_deselected();
                                if let Err(error) = self.persist().await {
                                    log::warn!(
                                        "morpheus whatsapp: could not rewrite state after dropping agents: {error}"
                                    );
                                }
                                format!(
                                    "External agents selected: {}.\nSend `/agent off` to stop them.",
                                    selection::render_roster(&self.roster)
                                )
                            }
                            Err(error) => {
                                self.roster = previous;
                                format!("Could not save the agent selection: {error}")
                            }
                        }
                    }
                    Err(message) => message,
                }
            }
            AgentCommand::Usage(message) => message,
        }
    }

    /// Close and forget handles/sessions for instances no longer selected.
    /// Called after a successful roster write, so a failed write keeps the
    /// old roster AND its live sessions.
    fn drop_deselected(&mut self) {
        let keys: std::collections::HashSet<String> = selection::entries(&self.roster)
            .into_iter()
            .map(|entry| selection::instance_key(&entry.agent, entry.index))
            .collect();
        self.handles.retain(|key, handle| {
            if keys.contains(key) {
                true
            } else {
                handle.shutdown();
                false
            }
        });
        self.sessions.retain(|key, _| keys.contains(key));
        self.observations.retain(|key, _| keys.contains(key));
    }

    /// Drop one instance's session (or every instance of an agent) while
    /// keeping the roster entry.
    async fn reset_session(&mut self, target: &str) -> String {
        let (agent, index) = match target.trim().to_ascii_lowercase().split_once('#') {
            Some((agent, index)) => match index.parse::<usize>() {
                Ok(index) if index > 0 => (agent.to_string(), Some(index)),
                _ => {
                    return format!(
                        "`{target}` is not a valid instance (use `claude` or `claude#2`)."
                    );
                }
            },
            None => (target.trim().to_ascii_lowercase(), None),
        };
        if !self.roster.iter().any(|name| name == &agent) {
            return format!("`{agent}` is not selected. Send `/agent ls` to see the roster.");
        }

        let keys: Vec<String> = match index {
            Some(index) => vec![selection::instance_key(&agent, index)],
            None => selection::entries(&self.roster)
                .into_iter()
                .filter(|entry| entry.agent == agent)
                .map(|entry| selection::instance_key(&entry.agent, entry.index))
                .collect(),
        };
        if let Some(index) = index {
            let total = self.roster.iter().filter(|name| *name == &agent).count();
            if index > total {
                return format!("`{agent}` has no instance {index}.");
            }
        }

        for key in &keys {
            if let Some(handle) = self.handles.remove(key) {
                handle.shutdown();
            }
            self.sessions.remove(key);
            self.observations.remove(key);
        }
        match self.persist().await {
            Ok(()) => format!(
                "Session reset for {}. The next message starts a fresh ACP session.",
                target.trim()
            ),
            Err(error) => format!("Session reset, but saving failed: {error}"),
        }
    }

    /// `/agent model <name>`: report the current pick, or the default.
    fn model_status(&self, name: &str) -> String {
        let name = name.trim().to_ascii_lowercase();
        if !self.roster.iter().any(|agent| agent == &name) {
            return format!("`{name}` is not selected. Send `/agent ls` to see the roster.");
        }
        match self.model_pick(&name) {
            Some(model) => format!("Model for *{name}*: `{model}`."),
            None => format!("Model for *{name}*: uses the agent default."),
        }
    }

    /// `/agent model <name> <model>`: the same store the TUI writes.
    fn set_model(&mut self, name: &str, model: &str) -> String {
        let name = name.trim().to_ascii_lowercase();
        if !self.roster.iter().any(|agent| agent == &name) {
            return format!("`{name}` is not selected. Send `/agent ls` to see the roster.");
        }
        let mut picks = ModelPicks::new(&self.model_dir);
        match picks.set(&name, model) {
            Ok(()) => format!("Model for *{name}* set to `{model}`."),
            Err(error) => format!("Could not save the model for `{name}`: {error}"),
        }
    }

    /// Persist roster, sessions and context in one atomic write.
    async fn persist(&self) -> Result<(), String> {
        self.store
            .save(
                &self.chat_key,
                &StoredChat {
                    version: STATE_VERSION,
                    roster: self.roster.clone(),
                    sessions: self.sessions.clone(),
                    context: self.context.clone(),
                    humanize: self.humanize_choice,
                    confidence: self.confidence.clone(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::commands::{self, AgentCommand};
    use super::*;

    fn installed(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    async fn control(dir: &std::path::Path) -> ChatControl {
        ChatControl::with_agents_at(
            ChatStore::at(dir),
            "5511912345678@s.whatsapp.net".to_string(),
            Mode::Build,
            installed(&["claude", "codex"]),
            installed(&["claude", "codex", "gemini"]),
            dir.join("agents"),
            false,
        )
        .await
    }

    async fn command(control: &mut ChatControl, text: &str) -> String {
        let parsed = commands::parse(text).expect("a registered command");
        control.handle_command(parsed).await
    }

    #[tokio::test]
    async fn confidence_is_per_chat_and_follows_the_selected_model() {
        let dir = tempfile::tempdir().unwrap();
        let mut chat = control(dir.path()).await;
        command(&mut chat, "/agent claude,claude").await;
        command(&mut chat, "/agent model claude sonnet-4").await;
        let reply = command(&mut chat, "/confidence claude#2 high").await;
        assert!(reply.contains("sonnet-4: high"), "{reply}");
        let rejected = command(&mut chat, "/confidence morpheus max").await;
        assert!(rejected.contains("selected external agent"), "{rejected}");
        let mut reloaded = control(dir.path()).await;
        let list = command(&mut reloaded, "/confidence").await;
        assert_eq!(list.matches("confidence high").count(), 2, "{list}");
        command(&mut reloaded, "/agent model claude opus-4").await;
        let reply = command(&mut reloaded, "/confidence claude#2").await;
        assert!(reply.contains("opus-4 · confidence unrated"), "{reply}");
    }

    #[tokio::test]
    async fn a_selection_is_persisted_and_reloaded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        let reply = command(&mut chat, "/agent Claude, codex").await;
        assert!(reply.contains("*claude*"), "{reply}");
        assert!(reply.contains("*codex*"), "{reply}");
        assert_eq!(chat.roster(), ["claude", "codex"]);

        let reloaded = control(dir.path()).await;
        assert_eq!(reloaded.roster(), ["claude", "codex"]);
    }

    #[tokio::test]
    async fn repeated_names_become_instances() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;
        let reply = command(&mut chat, "/agent claude,claude").await;
        assert!(reply.contains("*claude #1*"), "{reply}");
        assert!(reply.contains("*claude #2*"), "{reply}");
        assert_eq!(chat.roster(), ["claude", "claude"]);
    }

    #[tokio::test]
    async fn a_rejected_selection_changes_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;
        command(&mut chat, "/agent claude").await;

        for invalid in [
            "/agent maria",
            "/agent claude,,codex",
            "/agent morpheus",
            "/agent claude,codex,gemini,opencode,pi",
        ] {
            let reply = command(&mut chat, invalid).await;
            assert!(
                reply.contains("not installed")
                    || reply.contains("is empty")
                    || reply.contains("main agent")
                    || reply.contains("at most 4"),
                "{invalid}: {reply}"
            );
            assert_eq!(chat.roster(), ["claude"], "{invalid} changed the roster");
        }
    }

    #[tokio::test]
    async fn off_clears_and_persists_the_roster_and_sessions() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;
        command(&mut chat, "/agent claude,codex").await;
        chat.sessions
            .insert("claude#1".to_string(), "session-1".to_string());

        let reply = command(&mut chat, "/agent off").await;
        assert!(reply.contains("External agents are off"), "{reply}");
        assert!(chat.roster().is_empty());
        assert!(chat.sessions.is_empty());
        let reloaded = control(dir.path()).await;
        assert!(reloaded.roster().is_empty());
        assert!(reloaded.sessions.is_empty());
    }

    #[tokio::test]
    async fn reset_drops_one_instances_session_but_keeps_the_roster() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;
        command(&mut chat, "/agent claude,claude").await;
        chat.sessions.insert("claude#1".into(), "one".into());
        chat.sessions.insert("claude#2".into(), "two".into());

        let reply = command(&mut chat, "/agent reset claude#2").await;
        assert!(reply.contains("Session reset"), "{reply}");
        assert_eq!(chat.roster(), ["claude", "claude"]);
        assert_eq!(chat.session_id("claude#1").as_deref(), Some("one"));
        assert_eq!(chat.session_id("claude#2"), None);
    }

    #[tokio::test]
    async fn ls_reports_the_surface_without_running_a_turn() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        let reply = command(&mut chat, "/agent ls").await;
        assert!(reply.contains("morpheus"), "{reply}");
        assert!(reply.contains("Selected: none"), "{reply}");
        assert!(reply.contains("Installed: claude, codex"), "{reply}");

        let all = command(&mut chat, "/agent ls all").await;
        assert!(all.contains("• gemini - not installed"), "{all}");
    }

    #[tokio::test]
    async fn stop_reports_idle_and_trips_a_running_signal() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        assert_eq!(command(&mut chat, "/stop").await, "Nothing is running.");
        let signal = chat.stop_signal();
        chat.begin_turn();
        assert_eq!(command(&mut chat, "/stop").await, "Stopped.");
        assert!(
            signal.load(Ordering::SeqCst),
            "the running turn is cancelled"
        );
        assert!(
            !chat.stop_signal().load(Ordering::SeqCst),
            "next turn is fresh"
        );
        chat.end_turn();
    }

    #[tokio::test]
    async fn a_finished_turn_cannot_resurrect_a_deselected_session() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        chat.record_session("claude#1", "late".into())
            .await
            .expect("ignored");
        assert_eq!(chat.session_id("claude#1"), None, "no roster entry yet");

        command(&mut chat, "/agent claude").await;
        chat.record_session("claude#1", "kept".into())
            .await
            .expect("saved");
        assert_eq!(chat.session_id("claude#1").as_deref(), Some("kept"));

        command(&mut chat, "/agent off").await;
        chat.record_session("claude#1", "stale".into())
            .await
            .expect("ignored");
        assert_eq!(chat.session_id("claude#1"), None);
    }

    #[tokio::test]
    async fn simulated_typing_is_off_by_default_and_persists_per_chat() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        let status = command(&mut chat, "/human").await;
        assert!(status.contains("*off*"), "{status}");
        assert!(!chat.humanize());

        let on = command(&mut chat, "/human on").await;
        assert!(on.contains("*on*"), "{on}");
        assert!(chat.humanize());
        assert!(control(dir.path()).await.humanize(), "survives a reload");

        // Another chat keeps the default.
        let other = ChatControl::with_agents_at(
            ChatStore::at(dir.path()),
            "120363012345678901@g.us".to_string(),
            Mode::Build,
            installed(&["claude"]),
            installed(&["claude"]),
            dir.path().join("agents"),
            false,
        )
        .await;
        assert!(!other.humanize(), "the setting is per chat");

        let off = command(&mut chat, "/human off").await;
        assert!(off.contains("*off*"), "{off}");
        assert!(!control(dir.path()).await.humanize());
    }

    #[tokio::test]
    async fn the_settings_default_applies_until_the_chat_chooses() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = ChatControl::with_agents_at(
            ChatStore::at(dir.path()),
            "5511912345678@s.whatsapp.net".to_string(),
            Mode::Build,
            installed(&["claude"]),
            installed(&["claude"]),
            dir.path().join("agents"),
            true,
        )
        .await;

        // Settings → WhatsApp → Humanized replies, for a chat that has not
        // chosen for itself yet.
        assert!(chat.humanize());
        let status = command(&mut chat, "/human").await;
        assert!(status.contains("*on*"), "{status}");

        // The chat's own choice wins over the default, and survives a
        // reload while the default is still on.
        command(&mut chat, "/human off").await;
        assert!(!chat.humanize());
        let reloaded = ChatControl::with_agents_at(
            ChatStore::at(dir.path()),
            "5511912345678@s.whatsapp.net".to_string(),
            Mode::Build,
            installed(&["claude"]),
            installed(&["claude"]),
            dir.path().join("agents"),
            true,
        )
        .await;
        assert!(
            !reloaded.humanize(),
            "the chat's explicit choice outranks the settings default"
        );
    }

    #[tokio::test]
    async fn a_failed_human_write_keeps_the_old_value() {
        let dir = tempfile::tempdir().expect("temp dir");
        // A file where the store expects a directory: every write fails.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").expect("write blocker");
        let mut chat = ChatControl::with_agents_at(
            ChatStore::at(&blocked),
            "5511912345678@s.whatsapp.net".to_string(),
            Mode::Build,
            installed(&["claude"]),
            installed(&["claude"]),
            dir.path().join("agents"),
            false,
        )
        .await;

        let reply = command(&mut chat, "/human on").await;
        assert!(reply.contains("Could not save"), "{reply}");
        assert!(!chat.humanize(), "a failed write must not flip the setting");
    }

    #[tokio::test]
    async fn model_status_reports_the_pick_without_writing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        let unselected = command(&mut chat, "/agent model claude").await;
        assert!(unselected.contains("not selected"), "{unselected}");

        command(&mut chat, "/agent claude").await;
        let default = command(&mut chat, "/agent model claude").await;
        assert!(default.contains("uses the agent default"), "{default}");
        assert!(
            !dir.path().join("agents").exists(),
            "reading the pick must not create the store"
        );

        command(&mut chat, "/agent model claude sonnet-4").await;
        let picked = command(&mut chat, "/agent model claude").await;
        assert!(picked.contains("`sonnet-4`"), "{picked}");
    }

    #[tokio::test]
    async fn ls_shows_the_model_and_session_suffixes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;
        command(&mut chat, "/agent claude,claude").await;
        chat.sessions.insert("claude#1".into(), "session-1".into());
        command(&mut chat, "/agent model claude sonnet-4").await;

        let reply = command(&mut chat, "/agent ls").await;
        assert!(
            reply.contains("• *claude #1* - model `sonnet-4`, session saved"),
            "{reply}"
        );
        assert!(
            reply.contains("• *claude #2* - model `sonnet-4`, new session"),
            "{reply}"
        );
    }

    #[tokio::test]
    async fn a_model_pick_is_saved_only_for_a_selected_agent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;

        let rejected = command(&mut chat, "/agent model claude sonnet-4").await;
        assert!(rejected.contains("not selected"), "{rejected}");
        assert_eq!(
            ModelPicks::new(&dir.path().join("agents")).get("claude"),
            None,
            "a rejected pick must not be written"
        );

        command(&mut chat, "/agent claude").await;
        let saved = command(&mut chat, "/agent model claude sonnet-4").await;
        assert!(saved.contains("sonnet-4"), "{saved}");
        assert_eq!(
            ModelPicks::new(&dir.path().join("agents")).get("claude"),
            Some("sonnet-4")
        );
    }

    #[tokio::test]
    async fn usage_errors_are_answered_verbatim() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut chat = control(dir.path()).await;
        let reply = command(&mut chat, "/agent ls extra").await;
        assert!(reply.contains("`/agent ls all`"), "{reply}");
        assert!(chat.roster().is_empty());
    }

    #[test]
    fn the_agent_command_variants_are_all_handled() {
        // Compile-time exhaustiveness guard: adding a variant must update the
        // handler, and each spelling maps to the documented variant.
        for (text, expected) in [
            ("/agent", AgentCommand::List),
            ("/agent ls", AgentCommand::List),
            ("/agent ls all", AgentCommand::ListAll),
            ("/agent off", AgentCommand::Off),
            ("/agent reset claude", AgentCommand::Reset("claude".into())),
            (
                "/agent model claude sonnet-4",
                AgentCommand::Model {
                    name: "claude".into(),
                    model: "sonnet-4".into(),
                },
            ),
        ] {
            assert_eq!(
                commands::parse(text).expect("parsed").action(),
                Action::Agent(expected)
            );
        }
    }
}
