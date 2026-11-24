//! The `--help` pages.
//!
//! Every page renders inside one frame: a title row carrying the running
//! version on the left, one section per concern, and a footer with the
//! repository. Colours come from [`Palette`], so the page carries the user's
//! own theme.
//!
//! The layout is plain text first — padding is computed on the visible width,
//! then fragments are painted — so a monochrome terminal gets the exact same
//! alignment without the escape bytes.

use crate::cli::VERSION;
use crate::cli::style::{Palette, display_width};

/// Width reserved for the left column of an item row.
const LABEL_WIDTH: usize = 23;
/// Indentation of rows inside a section.
const ROW_INDENT: usize = 2;

/// One command of [`crate::cli::COMMANDS`], with its one-line summary. The
/// overview and the `help` page both read this table, and a guard test keeps it
/// in lockstep with the parser's command list.
const COMMAND_HELP: &[(&str, &str)] = &[
    (
        "update [version]",
        "Install the latest release, or a pinned version",
    ),
    ("paths", "Print the data, config and log directories"),
    (
        "whatsapp",
        "Answer WhatsApp messages with the agent, pairing by QR code",
    ),
    ("help [command]", "Show this help, or one command's page"),
];

/// A row of a section.
enum Row {
    /// Usage or explanation text, wrapped and painted as body.
    Prose(&'static str),
    /// A two-column row: the emphasised left cell and its description.
    Item(&'static str, &'static str),
    /// An example: the command (emphasised) and a comment (muted).
    Example(&'static str, &'static str),
    /// Secondary prose (notes, hints), painted muted.
    Note(&'static str),
}

struct Section {
    title: &'static str,
    rows: &'static [Row],
}

struct Page {
    title: &'static str,
    sections: &'static [Section],
}

// ── Pages ────────────────────────────────────────────────────────────────

const COMMAND_ROWS: &[Row] = &[
    Row::Item(COMMAND_HELP[0].0, COMMAND_HELP[0].1),
    Row::Item(COMMAND_HELP[1].0, COMMAND_HELP[1].1),
    Row::Item(COMMAND_HELP[2].0, COMMAND_HELP[2].1),
    Row::Item(COMMAND_HELP[3].0, COMMAND_HELP[3].1),
];

const OVERVIEW: Page = Page {
    title: "morpheus",
    sections: &[
        Section {
            title: "Usage",
            rows: &[
                Row::Prose("morpheus [options]"),
                Row::Prose("morpheus <command> [options]"),
            ],
        },
        Section {
            title: "Session",
            rows: &[
                Row::Item(
                    "-c, --continue",
                    "Start in the most recently active session of this directory",
                ),
                Row::Item(
                    "-s, --session <id>",
                    "Start in a specific session of this directory",
                ),
            ],
        },
        Section {
            title: "Commands",
            rows: COMMAND_ROWS,
        },
        Section {
            title: "Other",
            rows: &[
                Row::Item("-h, --help", "Show this help"),
                Row::Item("-v, --version", "Print the version"),
                Row::Item("--check-keyring", "Report API-key storage status"),
            ],
        },
        Section {
            title: "Examples",
            rows: &[
                Row::Example("morpheus -c", "continue where you left off"),
                Row::Example("morpheus -s 1788063765396", "open a specific session"),
                Row::Example("morpheus update", "install the latest release"),
                Row::Example("morpheus whatsapp", "chat with the agent from WhatsApp"),
            ],
        },
        Section {
            title: "Notes",
            rows: &[
                Row::Note(
                    "Sessions are stored per directory: -c and -s only see sessions started in the current one.",
                ),
                Row::Note("Run `morpheus help <command>` for one command's page."),
            ],
        },
    ],
};

const UPDATE_PAGE: Page = Page {
    title: "morpheus update",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("morpheus update [version] [--yes]")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Installs a release through the official installer for this OS and variant, replacing the binaries next to the running morpheus. With no version, targets the latest GitHub release.",
            )],
        },
        Section {
            title: "Arguments",
            rows: &[Row::Item(
                "version",
                "Release to install, `v0.2.3` or `0.2.3`; the latest when omitted",
            )],
        },
        Section {
            title: "Flags",
            rows: &[
                Row::Item(
                    "--yes, -y",
                    "Skip the confirmation prompt (required without a terminal)",
                ),
                Row::Item("-h, --help", "Show this page"),
            ],
        },
        Section {
            title: "Examples",
            rows: &[
                Row::Example("morpheus update", "install the latest release"),
                Row::Example("morpheus update v0.2.3", "pin a specific release"),
                Row::Example(
                    "morpheus update --yes",
                    "unattended: scripts and containers",
                ),
            ],
        },
        Section {
            title: "Notes",
            rows: &[
                Row::Note(
                    "The update replaces the running binary; start morpheus again when it finishes.",
                ),
                Row::Note("Set MORPHEUS_BIN_DIR to install into a different directory."),
            ],
        },
    ],
};

const PATHS_PAGE: Page = Page {
    title: "morpheus paths",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("morpheus paths")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Prints the directories morpheus reads and writes, one per line, so other tools can use them.",
            )],
        },
        Section {
            title: "Prints",
            rows: &[
                Row::Item("data", "sessions, usage cache, telemetry queue"),
                Row::Item("config", "setup.json and its lock"),
                Row::Item("logs", "stdout.log and laya.log"),
                Row::Item("sessions", "this directory's session histories"),
                Row::Item("binary", "the running executable"),
            ],
        },
        Section {
            title: "Notes",
            rows: &[Row::Note(
                "MORPHEUS_DATA_DIR and MORPHEUS_CONFIG_DIR override the first two.",
            )],
        },
    ],
};

const WHATSAPP_PAGE: Page = Page {
    title: "morpheus whatsapp",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("morpheus whatsapp")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Links this machine to a personal WhatsApp account as a companion device and answers the linked chats with the morpheus agent. No TUI is opened: the pairing QR code is printed, then the terminal only reports connection events while messages are handled in the background.",
            )],
        },
        Section {
            title: "Commands (in chat)",
            rows: &[
                Row::Item("/help", "List the commands available on WhatsApp"),
                Row::Item(
                    "/mode <mode>",
                    "Switch between build, ask, yolo, command and manager",
                ),
                Row::Item(
                    "/agent [ls|ls all|off|reset <name>|model <name> <model>|<name>[,<name>...]]",
                    "Select external ACP agents, or reset one session / set its model",
                ),
                Row::Item("/stop", "Stop the turns running in this chat"),
            ],
        },
        Section {
            title: "Notes",
            rows: &[
                Row::Note(
                    "Commands are case-insensitive and must name a registered command; a path such as /usr/bin stays normal text.",
                ),
                Row::Note(
                    "A message without a prefix always goes to the main agent. Address an external agent explicitly with @name (or @name#2 for an instance); @all reaches every selected instance.",
                ),
                Row::Note(
                    "External replies are labeled with the agent's name; an unlabeled reply is always the main agent's. An unknown @name is kept as ordinary text.",
                ),
                Row::Note(
                    "With no approval dialog, tools that need permission are denied in build/ask mode. Use `/mode yolo` to allow them.",
                ),
                Row::Note("TUI-only commands (theme, spinner) are not available over WhatsApp."),
            ],
        },
    ],
};

const HELP_PAGE: Page = Page {
    title: "morpheus help",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("morpheus help [command]")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Prints the top-level help, or the page of the named command.",
            )],
        },
        Section {
            title: "Commands",
            rows: COMMAND_ROWS,
        },
        Section {
            title: "Examples",
            rows: &[Row::Example("morpheus help update", "the update page")],
        },
    ],
};

/// Render the help page for `subject` (`None` = the top-level page).
pub fn render(subject: Option<&str>, palette: &Palette) -> String {
    page_for(subject).render(palette)
}

fn page_for(subject: Option<&str>) -> &'static Page {
    match subject {
        Some("update") => &UPDATE_PAGE,
        Some("paths") => &PATHS_PAGE,
        Some("whatsapp") => &WHATSAPP_PAGE,
        Some("help") => &HELP_PAGE,
        _ => &OVERVIEW,
    }
}

// ── Rendering ────────────────────────────────────────────────────────────

impl Page {
    fn render(&self, palette: &Palette) -> String {
        // `│ ` + content + ` │`
        let outer = palette.width();
        let inner = outer - 4;
        let rule = "─".repeat(inner + 2);
        let mut out = String::new();

        out.push_str(&format!("{}\n", palette.border(&format!("┌{rule}┐"))));

        let version = format!("v{VERSION}");
        let title_line = format!("{} · {}", self.title, version);
        let title_line_width = display_width(&title_line);
        out.push_str(&framed(
            palette,
            &palette.title(&title_line),
            title_line_width,
            inner,
        ));

        for (index, section) in self.sections.iter().enumerate() {
            if index > 0 {
                out.push_str(&framed(palette, "", 0, inner));
            }
            out.push_str(&framed(
                palette,
                &palette.heading(section.title),
                display_width(section.title),
                inner,
            ));
            for row in section.rows {
                out.push_str(&self.render_row(row, palette, inner));
            }
        }

        // Footer: where the project lives, for anyone who wants the source.
        let repo = format!("https://github.com/{}", morpheus::update::REPO);
        out.push_str(&format!("{}\n", palette.border(&format!("├{rule}┤"))));
        out.push_str(&framed(
            palette,
            &palette.muted(&repo),
            display_width(&repo),
            inner,
        ));
        out.push_str(&format!("{}\n", palette.border(&format!("└{rule}┘"))));
        out
    }

    fn render_row(&self, row: &Row, palette: &Palette, inner: usize) -> String {
        let indent = " ".repeat(ROW_INDENT);
        match row {
            Row::Prose(text) => wrapped(
                text,
                |palette, line| palette.body(line),
                palette,
                inner,
                &indent,
            ),
            Row::Note(text) => wrapped(
                text,
                |palette, line| palette.muted(line),
                palette,
                inner,
                &indent,
            ),
            Row::Item(left, desc) => {
                let desc_width = inner.saturating_sub(ROW_INDENT + LABEL_WIDTH);
                wrap(desc, desc_width)
                    .into_iter()
                    .enumerate()
                    .map(|(line_index, line)| {
                        let cell = if line_index == 0 { *left } else { "" };
                        let padding = LABEL_WIDTH.saturating_sub(display_width(cell));
                        let plain = format!("{indent}{cell}{}{line}", " ".repeat(padding));
                        framed(
                            palette,
                            &format!(
                                "{indent}{}{}{}",
                                palette.item(cell),
                                " ".repeat(padding),
                                palette.body(&line)
                            ),
                            display_width(&plain),
                            inner,
                        )
                    })
                    .collect()
            }
            Row::Example(command, comment) => {
                let command_width = 30.min(inner.saturating_sub(ROW_INDENT + 4));
                let left = format!("{indent}{command}");
                let padding = command_width.saturating_sub(display_width(command));
                let plain = format!("{left}{}{comment}", " ".repeat(padding));
                framed(
                    palette,
                    &format!("{left}{}{}", " ".repeat(padding), palette.muted(comment)),
                    display_width(&plain),
                    inner,
                )
            }
        }
    }
}

/// Wrap `text` and frame every line, painted with `paint`.
fn wrapped(
    text: &str,
    paint: fn(&Palette, &str) -> String,
    palette: &Palette,
    inner: usize,
    indent: &str,
) -> String {
    wrap(text, inner.saturating_sub(ROW_INDENT))
        .into_iter()
        .map(|line| {
            let plain = format!("{indent}{line}");
            framed(
                palette,
                &format!("{indent}{}", paint(palette, &line)),
                display_width(&plain),
                inner,
            )
        })
        .collect()
}

/// One framed line: `│ ` + `content` + padding + ` │`, `content` already
/// painted and `content_width` its visible width.
fn framed(palette: &Palette, content: &str, content_width: usize, inner: usize) -> String {
    let padding = " ".repeat(inner.saturating_sub(content_width));
    format!(
        "{}{content}{}{}\n",
        palette.border("│ "),
        padding,
        palette.border(" │")
    )
}

/// Word-wrap `text` at `width` columns. Words longer than the width are kept
/// whole — a URL or a version never gets split mid-token.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate =
            display_width(&current) + usize::from(!current.is_empty()) + display_width(word);
        if !current.is_empty() && candidate > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::COMMANDS;
    use crate::theme::ThemeRegistry;

    fn palette() -> Palette {
        Palette::new(ThemeRegistry::new().default_theme().clone(), false)
    }

    fn plain(subject: Option<&str>) -> String {
        render(subject, &palette())
    }

    #[test]
    fn every_command_is_documented_and_has_a_page() {
        for command in COMMANDS {
            assert!(
                COMMAND_HELP
                    .iter()
                    .any(|(label, _)| label.split(' ').next() == Some(*command)),
                "command `{command}` is missing from COMMAND_HELP"
            );
            let page = plain(Some(command));
            assert!(
                page.contains(&format!("morpheus {command}")),
                "the `{command}` page does not name the command"
            );
        }
    }

    #[test]
    fn the_overview_lists_every_command_and_flag() {
        let page = plain(None);
        for needle in [
            "update [version]",
            "paths",
            "whatsapp",
            "help [command]",
            "-c, --continue",
            "-s, --session <id>",
            "-v, --version",
            "-h, --help",
            "--check-keyring",
            "morpheus",
            VERSION,
        ] {
            assert!(page.contains(needle), "the overview is missing {needle:?}");
        }
    }

    #[test]
    fn the_help_has_no_logo_mark_and_stays_compact() {
        let page = plain(None);
        let lines: Vec<&str> = page.lines().collect();
        // No leftover mark glyph anywhere on the page.
        for row in [" ▄███▄ ", "█▌ ▅ ▐█", "█▌ ▀ ▐█", " ▀███▀ "]
        {
            assert!(
                !page.contains(row),
                "page still carries the logo mark: {row:?}"
            );
        }
        // Title + version on the first row, Usage on the second, and the two
        // Usage rows on the third and fourth, so no empty row is left over.
        assert!(lines[1].contains("morpheus"));
        assert!(lines[1].contains(&format!("v{VERSION}")));
        assert!(lines[2].contains("Usage"));
        assert!(lines[3].contains("morpheus [options]"));
        assert!(lines[4].contains("morpheus <command> [options]"));
    }

    #[test]
    fn the_frame_stays_aligned_and_within_the_layout_width() {
        let page = plain(None);
        let widths: Vec<usize> = page.lines().map(display_width).collect();
        let expected = widths[0];
        assert!(
            widths.iter().all(|width| *width == expected),
            "ragged frame: {widths:?}"
        );
        assert!(expected <= palette().width());

        for line in page.lines() {
            assert!(
                line.starts_with(['│', '┌', '├', '└']),
                "row escapes the frame: {line:?}"
            );
            assert!(
                line.ends_with(['│', '┐', '┤', '┘']),
                "row escapes the frame: {line:?}"
            );
        }
    }

    #[test]
    fn coloured_output_keeps_the_same_visible_width() {
        let coloured = Palette::new(ThemeRegistry::new().default_theme().clone(), true);
        let widths: Vec<usize> = render(None, &coloured).lines().map(display_width).collect();
        assert!(widths.iter().all(|width| *width == widths[0]), "{widths:?}");
    }

    #[test]
    fn wrap_keeps_long_tokens_whole() {
        let lines = wrap("see https://example.com/a/very/long/path", 12);
        assert_eq!(lines[0], "see");
        assert_eq!(lines[1], "https://example.com/a/very/long/path");
    }
}
