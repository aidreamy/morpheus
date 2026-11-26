//! Persistence and catalog for tool controls shared by the TUI and GUI.

use std::{collections::HashSet, io, path::Path};

use crate::setup::Setup;

const PROJECT_FILE: &str = ".morpheus-tools.json";

#[derive(serde::Deserialize, serde::Serialize)]
struct ProjectTools {
    #[serde(default)]
    disabled: Vec<String>,
}

pub fn load_disabled_tools(setup: &Setup, cwd: &str, global: bool) -> HashSet<String> {
    if global {
        return setup.tools.disabled.iter().cloned().collect();
    }
    match std::fs::read_to_string(Path::new(cwd).join(PROJECT_FILE)) {
        Ok(text) => parse_project_tools(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            setup.tools.disabled.iter().cloned().collect()
        }
        Err(_) => HashSet::new(),
    }
}

pub fn save_disabled_tools(
    setup: &mut Setup,
    disabled: &HashSet<String>,
    cwd: &str,
    global: bool,
) -> io::Result<()> {
    let mut names: Vec<_> = disabled.iter().cloned().collect();
    names.sort_unstable();
    if global {
        setup.tools.disabled = names;
        return setup
            .save()
            .then_some(())
            .ok_or_else(|| io::Error::other("could not save setup.json"));
    }
    let cwd = Path::new(cwd);
    let file = tempfile::NamedTempFile::new_in(cwd)?;
    serde_json::to_writer_pretty(file.as_file(), &ProjectTools { disabled: names })?;
    file.persist(cwd.join(PROJECT_FILE))?;
    Ok(())
}

fn parse_project_tools(text: &str) -> HashSet<String> {
    serde_json::from_str::<ProjectTools>(text)
        .map(|project| project.disabled.into_iter().collect())
        .unwrap_or_default()
}

pub fn internal_tools() -> &'static [(&'static str, &'static str)] {
    &[
        ("bash_run", "Execute shell commands"),
        ("fs_read", "Read file contents"),
        ("fs_write", "Write file contents"),
        ("fs_edit", "Edit file contents (exact string replacement)"),
        ("fs_edit_lines", "Edit file lines (line/block operations)"),
        ("fs_ast_edit", "Edit file contents (structural AST rewrite)"),
        ("fs_rollback", "Rollback file changes"),
        ("find_glob", "Find files by glob pattern"),
        ("find_grep", "Search file contents"),
        ("web_fetch", "Fetch web page content"),
        ("web_search", "Search the web"),
        ("plan_todo_write", "Write todo items"),
        ("ask_questions", "Ask the user questions"),
        #[cfg(feature = "embed")]
        (
            "recall_search",
            "Search knowledge bases for semantically similar entries",
        ),
        ("skills_list", "List available skills"),
        ("skills_read", "Read a skill"),
        ("skills_read_asset", "Read a skill asset"),
        ("skills_match_skills", "Match skills to task"),
        ("subagent_call", "Run a sub-agent for a delegated task"),
        (
            "subagent_status",
            "Check background sub-agent task status (read-only)",
        ),
        (
            "agents_call",
            "Send a message to a /agents coworker pane (block or background)",
        ),
        (
            "agents_worktree",
            "Move a /agents coworker into/out of a git worktree (User-bound: needs approval)",
        ),
        (
            "agents_compact",
            "Compact a /agents coworker's context ahead of the automatic trigger",
        ),
        (
            "computer_apps",
            "List desktop apps with PIDs (accessibility tree)",
        ),
        ("computer_snapshot", "Capture an app's accessibility tree"),
        (
            "computer_wait",
            "Block until an element reaches a state (visible/enabled/detached/...); read-only",
        ),
        (
            "computer_screenshot",
            "Capture screen as an image; annotate=true for boxes + selector legend (Build-safe)",
        ),
        (
            "computer_windows",
            "List one app's open windows, or minimize/maximize/close/move/resize one (action = needs approval)",
        ),
        (
            "computer_act",
            "Act AND type as one pipeline: semantic action on an element, then keyboard/wait via `then`",
        ),
        (
            "computer_control",
            "Pointer AND keyboard as one pipeline: click an element, then type/press via `then`",
        ),
        (
            "lsp",
            "Language server tools (diagnostics, definitions, references, rename, ...)",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

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
