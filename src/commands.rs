//! The built-in slash commands. Each interface exposes the commands it implements.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Models,
    Agents,
    Router,
    Download,
    Providers,
    Settings,
    Tools,
    Themes,
    Background,
    Wallpaper,
    Bell,
    Voice,
    Spinner,
    ToolCall,
    Compact,
    Export,
    New,
    Rename,
    Undo,
    Worktree,
    Confidence,
}

impl Command {
    pub const ALL: &[Self] = &[
        Self::Models,
        Self::Agents,
        Self::Router,
        Self::Download,
        Self::Providers,
        Self::Settings,
        Self::Tools,
        Self::Themes,
        Self::Background,
        Self::Wallpaper,
        Self::Bell,
        Self::Voice,
        Self::Spinner,
        Self::ToolCall,
        Self::Compact,
        Self::Export,
        Self::New,
        Self::Rename,
        Self::Undo,
        Self::Worktree,
        Self::Confidence,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|command| command.metadata().0 == name)
    }

    pub const fn metadata(self) -> (&'static str, &'static str, Scope) {
        use Scope::{Agent, Global};
        match self {
            Self::Models => ("models", "Manage models", Agent),
            Self::Agents => ("agents", "Split the chat into agent panes", Global),
            Self::Router => ("router", "Open the fallback router", Global),
            Self::Download => ("download", "Download local models", Global),
            Self::Providers => ("providers", "Add an API provider", Global),
            Self::Settings => ("settings", "Open settings", Global),
            Self::Tools => ("tools", "Open internal tools", Agent),
            Self::Themes => ("themes", "Change theme", Global),
            Self::Background => ("background", "Toggle terminal background", Global),
            Self::Wallpaper => (
                "wallpaper",
                "Set the ASCII wallpaper (image.png [x,y], off)",
                Global,
            ),
            Self::Bell => ("bell", "Toggle completion bell", Global),
            Self::Voice => ("voice", "Toggle automatic voice generation", Global),
            Self::Spinner => ("spinner", "Set spinner animation", Global),
            Self::ToolCall => ("toolcall", "Set tool call mode", Agent),
            Self::Compact => ("compact", "Compact context now", Agent),
            Self::Export => ("export", "Export transcript to Markdown", Agent),
            Self::New => ("new", "Start a new session", Global),
            Self::Rename => ("rename", "Rename session", Global),
            Self::Undo => ("undo", "Undo last revert", Global),
            Self::Worktree => (
                "worktree",
                "Isolate this agent in a git worktree (add <branch>|<path> <branch>, list, exit)",
                Agent,
            ),
            Self::Confidence => (
                "confidence",
                "Set confidence in an external agent for its current model",
                Global,
            ),
        }
    }
}
