//! Slash-command detection and the WhatsApp command registry.
//!
//! # What counts as a command
//!
//! A message is a slash command only when **all** of these hold:
//!
//! 1. the first non-whitespace character is `/`;
//! 2. the token right after `/` is a bare name (`A-Z`, `a-z`, `0-9`, `-`,
//!    `_`) - no `/`, `\`, `.` or `:`; and
//! 3. the lowercased name is a command this registry publishes.
//!
//! The registry gate is what keeps ordinary messages from being hijacked:
//! Unix paths (`/usr/bin/env`), file paths (`/home/me/report.txt`) and text
//! like `/ano que vem` start with a slash but name no command, so they are
//! sent to the agent as normal prose. This mirrors the conservative
//! detection used by coding agents such as Crush, OpenCode and Claude Code:
//! a slash command is an *explicit, known* command at the start of the
//! message, never a heuristic on the first character alone.
//!
//! Names are matched case-insensitively (`/MODE build` == `/mode build`),
//! and arguments are the trimmed tail after the first whitespace. A message
//! with only a command name is still a command: the handler reports the
//! missing arguments instead of guessing.

use crate::{
    agent_profiles::{CONFIDENCE_USAGE, ConfidenceCommand},
    harness::Mode,
};

use super::mode;

/// One command published to WhatsApp users.
pub struct CommandSpec {
    /// Canonical lowercase name (the only spelling shown in help).
    pub name: &'static str,
    /// One-line summary for `/help`.
    pub summary: &'static str,
    /// Usage line for `/help` and for argument errors.
    pub usage: &'static str,
}

/// The command surface exposed over WhatsApp.
///
/// This is deliberately an allowlist, not a copy of the TUI menu: a command
/// ships here only after its remote behavior exists. TUI-only presentation
/// commands stay out (see [`TUI_ONLY_COMMANDS`]).
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        summary: "List the commands available on WhatsApp",
        usage: "/help",
    },
    CommandSpec {
        name: "mode",
        summary: "Switch the harness mode",
        usage: "/mode <build|ask|yolo|command|manager>",
    },
    CommandSpec {
        name: "agent",
        summary: "Select the external ACP agents for this chat",
        usage: "/agent [ls|ls all|off|reset <name>|model <name> <model>|<name>[,<name>...]]",
    },
    CommandSpec {
        name: "human",
        summary: "Simulate typing before replying",
        usage: "/human [on|off]",
    },
    CommandSpec {
        name: "confidence",
        summary: "Check or set confidence in a selected external agent",
        usage: CONFIDENCE_USAGE,
    },
    CommandSpec {
        name: "stop",
        summary: "Stop the turns running in this chat",
        usage: "/stop",
    },
];

/// Commands that exist in the TUI but have no meaning in a remote chat:
/// they customize the terminal UI (theme, spinner), which the
/// WhatsApp interface does not render. Kept explicit so the exclusion is a
/// tested contract instead of an accident of the allowlist.
#[cfg(test)]
pub const TUI_ONLY_COMMANDS: &[&str] = &["theme", "themes", "spinner"];

/// A syntactically valid, registered command parsed out of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingCommand {
    /// Canonical lowercase name.
    pub name: String,
    /// Trimmed argument tail (empty when nothing followed the name).
    pub args: String,
}

/// What a parsed command asks the session to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Render the command list.
    Help,
    /// Switch the harness mode.
    SetMode(Mode),
    /// Inspect or change the external-agent roster.
    Agent(AgentCommand),
    Confidence(ConfidenceCommand),
    /// Inspect (`None`) or change the simulated-typing setting.
    Humanize(Option<bool>),
    /// Cancel the turns currently running in this chat.
    Stop,
    /// The arguments were missing or malformed: send this usage message
    /// back to the user instead of executing anything.
    Usage(String),
}

/// What `/agent` asks the chat controller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCommand {
    /// `/agent` or `/agent ls`: the main agent, roster and installed agents.
    List,
    /// `/agent ls all`: also show registered but not-installed agents.
    ListAll,
    /// `/agent off`: clear the roster and end external sessions.
    Off,
    /// `/agent reset <name>`: drop one agent's (or instance's) session.
    Reset(String),
    /// `/agent model <name> <model>`: persist the model for one agent.
    Model { name: String, model: String },
    /// `/agent model <name>`: report the model currently picked for one
    /// agent, without changing anything.
    ModelStatus(String),
    /// `/agent a,b`: replace the roster with the (still unvalidated)
    /// comma-separated expression.
    Set(String),
    /// A malformed `/agent` spelling: send this usage message back.
    Usage(String),
}

/// Look up a command by its canonical lowercase name.
#[must_use]
pub fn lookup(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|command| command.name == name)
}

/// Detect and parse a slash command. `None` means "ordinary message".
#[must_use]
pub fn parse(text: &str) -> Option<IncomingCommand> {
    let rest = text.trim_start().strip_prefix('/')?;

    // The name ends at the first whitespace; the tail is the argument text.
    let mut parts = rest.splitn(2, char::is_whitespace);
    let raw_name = parts.next().unwrap_or_default();
    let args = parts.next().unwrap_or_default().trim();

    // A bare name only. This is what rejects paths: `/usr/bin` has a `/`
    // inside the name and is never considered a command.
    if raw_name.is_empty()
        || !raw_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return None;
    }

    let name = raw_name.to_ascii_lowercase();
    lookup(&name)?;
    Some(IncomingCommand {
        name,
        args: args.to_string(),
    })
}

impl IncomingCommand {
    /// Resolve `/agent`'s arguments.
    ///
    /// The roster expression is kept raw here: validating names needs the
    /// installed set, which is runtime state, not grammar. Only the fixed
    /// subcommands are resolved at parse time so a misspelled one (`/agent ls
    /// foo`) gets a usage message instead of an "unknown agent" error.
    fn agent_command(&self) -> AgentCommand {
        let words: Vec<String> = self
            .args
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        match words.as_slice() {
            [] => AgentCommand::List,
            [word] if word == "ls" => AgentCommand::List,
            [ls, all] if ls == "ls" && all == "all" => AgentCommand::ListAll,
            [off] if off == "off" => AgentCommand::Off,
            [reset, name] if reset == "reset" => AgentCommand::Reset(name.clone()),
            [model, name, model_id] if model == "model" => AgentCommand::Model {
                name: name.clone(),
                model: model_id.clone(),
            },
            [model, name] if model == "model" => AgentCommand::ModelStatus(name.clone()),
            [ls, ..] if ls == "ls" => AgentCommand::Usage(
                "`/agent ls` lists the agents; `/agent ls all` also shows the ones that are not \
                 installed."
                    .to_string(),
            ),
            [off, ..] if off == "off" => {
                AgentCommand::Usage("`/agent off` takes no arguments.".to_string())
            }
            [reset, ..] if reset == "reset" => {
                AgentCommand::Usage("`/agent reset <name>` drops one agent's session.".to_string())
            }
            [model, ..] if model == "model" => AgentCommand::Usage(
                "`/agent model <name>` shows the model for one agent; \
                 `/agent model <name> <model>` sets it."
                    .to_string(),
            ),
            _ => AgentCommand::Set(self.args.clone()),
        }
    }

    /// Validate the arguments and resolve what to run.
    #[must_use]
    pub fn action(&self) -> Action {
        match self.name.as_str() {
            "help" => {
                if self.args.is_empty() {
                    Action::Help
                } else {
                    Action::Usage("`/help` takes no arguments.".to_string())
                }
            }
            "agent" => Action::Agent(self.agent_command()),
            "confidence" => match ConfidenceCommand::parse(&self.args) {
                Ok(command) => Action::Confidence(command),
                Err(usage) => Action::Usage(format!("Use `{usage}`.")),
            },
            "human" => {
                let mut words = self.args.split_whitespace();
                match (words.next(), words.next()) {
                    (None, _) => Action::Humanize(None),
                    (Some(word), None) => match word.to_ascii_lowercase().as_str() {
                        "on" => Action::Humanize(Some(true)),
                        "off" => Action::Humanize(Some(false)),
                        _ => Action::Usage(
                            "`/human` takes `on` or `off` (e.g. `/human on`).".to_string(),
                        ),
                    },
                    (Some(_), Some(_)) => {
                        Action::Usage("`/human` takes one argument: `on` or `off`.".to_string())
                    }
                }
            }
            "stop" => {
                if self.args.is_empty() {
                    Action::Stop
                } else {
                    Action::Usage("`/stop` takes no arguments.".to_string())
                }
            }
            "mode" => {
                let mut words = self.args.split_whitespace();
                match (words.next(), words.next()) {
                    (Some(word), None) => match mode::parse(word) {
                        Some(mode) => Action::SetMode(mode),
                        None => Action::Usage(format!(
                            "Unknown mode `{word}`. Available modes: {}.",
                            mode::options()
                        )),
                    },
                    (None, _) => Action::Usage(format!(
                        "Name the mode: {}. Expected `/mode <mode>`.",
                        mode::options()
                    )),
                    (Some(_), Some(_)) => Action::Usage(
                        "`/mode` takes exactly one mode name (e.g. `/mode build`).".to_string(),
                    ),
                }
            }
            // `parse` only returns registered names, so this is unreachable
            // in practice; answering with the usage line beats a silent
            // no-op if the registry and this match ever drift.
            other => Action::Usage(format!(
                "`/{other}` has no handler. Send `/help` for the command list."
            )),
        }
    }
}

/// The `/help` body.
#[must_use]
pub fn help_text() -> String {
    let mut text = String::from("*Morpheus on WhatsApp*\n\nCommands (case-insensitive):\n");
    for command in COMMANDS {
        text.push_str(&format!("• {} - {}\n", command.usage, command.summary));
    }
    text.push_str("\nModes:\n");
    for (name, description) in mode::ALL {
        text.push_str(&format!("• {name} - {description}\n"));
    }
    text.push_str(
        "\nExternal agents:\n• Select ACP agents with `/agent claude,codex` (up to 4).\n\
         • Address one explicitly: `@claude fix the build`; add `#2` for an instance.\n\
         • `@all` reaches every selected instance.\n\
         • No prefix means the main agent (`morpheus`), always.\n\
         • External replies are labeled; an unlabeled reply is the main agent's.\n\n\
         Humanized replies:\n\
         • `/human on` shows a simulated “typing…” for about 1–2.5 seconds before \
         each reply. It is off by default and does not change the answer.\n\
         • Commands and the self-chat are never delayed.\n\n\
         Send any other message to talk to the agent. A message that only \
         looks like a path (e.g. /usr/bin/env) is treated as normal text.",
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(name: &str, args: &str) -> IncomingCommand {
        IncomingCommand {
            name: name.to_string(),
            args: args.to_string(),
        }
    }

    #[test]
    fn registered_commands_are_detected() {
        assert_eq!(
            parse("/help"),
            Some(cmd("help", "")),
            "a bare command is still a command"
        );
        assert_eq!(parse("/mode build"), Some(cmd("mode", "build")));
        assert_eq!(parse("/agent"), Some(cmd("agent", "")));
        assert_eq!(
            parse("/agent claude, codex"),
            Some(cmd("agent", "claude, codex"))
        );
        assert_eq!(
            parse("   /mode   yolo   "),
            Some(cmd("mode", "yolo")),
            "leading whitespace and the argument tail are trimmed"
        );
    }

    #[test]
    fn commands_are_case_insensitive() {
        for spelling in ["/help", "/HELP", "/Help", "/hElP"] {
            assert_eq!(parse(spelling), Some(cmd("help", "")), "{spelling}");
        }
        // The command name is normalized; the argument tail is preserved as
        // typed (mode parsing is itself case-insensitive).
        for (spelling, args) in [
            ("/mode BUILD", "BUILD"),
            ("/MODE Build", "Build"),
            ("/Mode bUiLd", "bUiLd"),
        ] {
            assert_eq!(parse(spelling), Some(cmd("mode", args)), "{spelling}");
        }
    }

    #[test]
    fn paths_and_prose_are_not_commands() {
        // The false positives this grammar exists to avoid: a leading slash
        // is not enough, and the name must be a registered command.
        for text in [
            "/usr/bin/env",
            "/home/me/report.txt is broken",
            "/etc",
            "/ano que vem",
            "and/or",
            "https://example.com/mode",
            "/",
            "/ help",
            "/mode:build",
            "/mod build",
            "/mode.build",
            "/agentx",
            "/agent.foo",
            "/agent:build",
        ] {
            assert_eq!(parse(text), None, "{text:?} must stay a normal message");
        }
    }

    #[test]
    fn a_slash_inside_the_argument_is_allowed() {
        // Only the NAME is restricted; arguments can be anything the
        // command accepts (today `/mode build`, but the grammar must not
        // bake that in).
        assert_eq!(parse("/mode build/extra"), Some(cmd("mode", "build/extra")));
    }

    #[test]
    fn mode_arguments_are_validated_explicitly() {
        assert_eq!(cmd("mode", "build").action(), Action::SetMode(Mode::Build));
        assert_eq!(cmd("mode", "YOLO").action(), Action::SetMode(Mode::Yolo));

        match cmd("mode", "").action() {
            Action::Usage(message) => assert!(message.contains("Name the mode"), "{message}"),
            other => panic!("expected usage, got {other:?}"),
        }
        match cmd("mode", "turbo").action() {
            Action::Usage(message) => assert!(message.contains("Unknown mode"), "{message}"),
            other => panic!("expected usage, got {other:?}"),
        }
        match cmd("mode", "build extra").action() {
            Action::Usage(message) => assert!(message.contains("exactly one"), "{message}"),
            other => panic!("expected usage, got {other:?}"),
        }
    }

    #[test]
    fn stop_takes_no_arguments() {
        assert_eq!(cmd("stop", "").action(), Action::Stop);
        assert!(matches!(cmd("stop", "now").action(), Action::Usage(_)));
        assert_eq!(parse("/STOP"), Some(cmd("stop", "")));
    }

    #[test]
    fn agent_reset_and_model_are_resolved_at_parse_time() {
        assert_eq!(
            cmd("agent", "reset claude#2").action(),
            Action::Agent(AgentCommand::Reset("claude#2".to_string()))
        );
        assert_eq!(
            cmd("agent", "model claude sonnet-4").action(),
            Action::Agent(AgentCommand::Model {
                name: "claude".to_string(),
                model: "sonnet-4".to_string()
            })
        );
    }

    #[test]
    fn agent_lists_and_off_are_resolved_at_parse_time() {
        assert_eq!(cmd("agent", "").action(), Action::Agent(AgentCommand::List));
        assert_eq!(
            cmd("agent", "LS").action(),
            Action::Agent(AgentCommand::List),
            "subcommands are case-insensitive"
        );
        assert_eq!(
            cmd("agent", "ls all").action(),
            Action::Agent(AgentCommand::ListAll)
        );
        assert_eq!(
            cmd("agent", "off").action(),
            Action::Agent(AgentCommand::Off)
        );
    }

    #[test]
    fn agent_keeps_the_roster_expression_raw_for_runtime_validation() {
        assert_eq!(
            cmd("agent", " Claude , codex ").action(),
            Action::Agent(AgentCommand::Set(" Claude , codex ".to_string())),
            "canonicalization and installation are runtime checks"
        );
    }

    #[test]
    fn malformed_agent_spellings_get_usage_not_a_roster_change() {
        for args in ["ls foo", "ls all extra", "off extra", "reset", "reset a b"] {
            match cmd("agent", args).action() {
                Action::Agent(AgentCommand::Usage(message)) => {
                    assert!(message.contains("/agent"), "{args}: {message}");
                }
                other => panic!("{args}: expected usage, got {other:?}"),
            }
        }
    }

    #[test]
    fn agent_model_without_a_model_reports_the_current_pick() {
        assert_eq!(
            cmd("agent", "model claude").action(),
            Action::Agent(AgentCommand::ModelStatus("claude".to_string()))
        );
        assert_eq!(
            cmd("agent", "MODEL Claude").action(),
            Action::Agent(AgentCommand::ModelStatus("claude".to_string()))
        );
    }

    #[test]
    fn human_toggles_and_reports() {
        assert_eq!(cmd("human", "").action(), Action::Humanize(None));
        assert_eq!(cmd("human", "on").action(), Action::Humanize(Some(true)));
        assert_eq!(cmd("human", "OFF").action(), Action::Humanize(Some(false)));
        assert_eq!(parse("/HUMAN On"), Some(cmd("human", "On")));

        for args in ["maybe", "on off", "on extra"] {
            match cmd("human", args).action() {
                Action::Usage(message) => assert!(message.contains("/human"), "{args}: {message}"),
                other => panic!("{args}: expected usage, got {other:?}"),
            }
        }
    }

    #[test]
    fn help_mentions_the_simulated_typing_setting() {
        let help = help_text();
        assert!(help.contains("/human on"), "{help}");
        assert!(help.contains("off by default"), "{help}");
        assert!(help.contains("self-chat are never delayed"), "{help}");
    }

    #[test]
    fn help_mentions_external_agents_and_the_no_prefix_rule() {
        let help = help_text();
        assert!(help.contains("/agent"), "{help}");
        assert!(help.contains("No prefix means the main agent"), "{help}");
        assert!(help.contains("@all"), "{help}");
        assert!(help.contains("#2"), "{help}");
        assert!(help.contains("labeled"), "{help}");
    }

    #[test]
    fn help_takes_no_arguments() {
        assert_eq!(cmd("help", "").action(), Action::Help);
        assert!(matches!(cmd("help", "mode").action(), Action::Usage(_)));
    }

    #[test]
    fn tui_only_commands_are_not_published() {
        for name in TUI_ONLY_COMMANDS {
            assert!(
                lookup(name).is_none(),
                "TUI-only command `/{name}` must not be exposed on WhatsApp"
            );
            assert_eq!(parse(&format!("/{name}")), None);
            assert!(!help_text().contains(&format!("/{name}")));
        }
    }

    #[test]
    fn help_lists_every_published_command_and_mode() {
        let help = help_text();
        for command in COMMANDS {
            assert!(
                help.contains(command.usage),
                "help is missing {}",
                command.usage
            );
        }
        for (name, _) in mode::ALL {
            assert!(help.contains(name), "help is missing mode {name}");
        }
    }
}
