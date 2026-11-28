use std::path::{Path, PathBuf};

use crate::harness::{ContextManager, ContextManagerState};

/// Export the agent-visible history to a new Markdown file in the project.
pub fn write_transcript(
    state: &ContextManagerState,
    title: &str,
    cwd: &Path,
) -> std::io::Result<PathBuf> {
    let mut manager = ContextManager::new(state.max_tokens);
    manager.restore_state(state);
    let transcript = manager.export_markdown();
    if transcript.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the agent-visible transcript is empty",
        ));
    }
    let uuid = crate::telemetry::events::uuid_v4()
        .ok_or_else(|| std::io::Error::other("OS entropy unavailable for the export file name"))?;
    let path = cwd.join(format!("{}-{uuid}.md", slugify(title)));
    std::fs::write(&path, transcript)?;
    Ok(path)
}

/// Slugify a session title into a file-name-safe base: lowercase ASCII
/// alphanumerics, runs of anything else collapsed into single dashes. Empty
/// titles fall back to a neutral base so the file is still nameable.
pub fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut dashed = true; // trim leading dashes
    for ch in title.chars() {
        let lower: Option<char> = match ch {
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        };
        match lower {
            Some(c) => {
                slug.push(c);
                dashed = false;
            }
            None if !dashed => {
                slug.push('-');
                dashed = true;
            }
            None => {}
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    const MAX_SLUG: usize = 60;
    while slug.len() > MAX_SLUG {
        slug.pop();
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "session".to_string()
    } else {
        slug
    }
}
