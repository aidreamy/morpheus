//! Durable per-chat state for the WhatsApp interface.
//!
//! Each chat gets one small JSON file under
//! `{data_dir}/whatsapp/chats/<hash>.json`, keyed by the chat's canonical id.
//! The file holds what must survive a restart but never the transcript: the
//! selected external agents (`/agent`), their ACP session ids, and the main
//! agent's model-facing context.
//!
//! Writes are atomic: the JSON is written to a sibling temporary file, synced,
//! then renamed over the destination, so a crash leaves either the previous
//! file or the new one intact. Reads fail safe: a missing file is the default
//! state, while a corrupt or future-version file is logged and replaced by the
//! default rather than taking the chat (or the process) down.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::harness::ContextManagerState;
use crate::setup::data_dir_override;

/// Version of the on-disk schema. Bump it when a field changes meaning or is
/// removed; older/newer files are ignored instead of guessed at.
pub const STATE_VERSION: u32 = 1;

/// Everything persisted for one chat.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredChat {
    /// Schema version written by the producer.
    #[serde(default = "state_version")]
    pub version: u32,
    /// Canonical ids of the external agents selected with `/agent`. A name
    /// may repeat to create instances (`claude,claude` → `claude#1`,
    /// `claude#2`).
    #[serde(default)]
    pub roster: Vec<String>,
    /// ACP session id per instance key (`claude#1`, `claude#2`). The key
    /// includes the index even for a single instance, so deselecting and
    /// re-adding an agent resumes the same conversation.
    #[serde(default)]
    pub sessions: BTreeMap<String, String>,
    /// The main agent's model-facing context, so a restart keeps the
    /// conversation instead of starting from nothing.
    #[serde(default)]
    pub context: Option<ContextManagerState>,
    /// The chat's own simulated-typing choice (`/human`), or `None` when it
    /// has never chosen - the `whatsapp.humanize` default from `setup.json`
    /// applies until then. Absent means "no choice", which is the old
    /// behavior, so no schema bump is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub humanize: Option<bool>,
}

impl Default for StoredChat {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            roster: Vec::new(),
            sessions: BTreeMap::new(),
            context: None,
            humanize: None,
        }
    }
}

fn state_version() -> u32 {
    STATE_VERSION
}

/// The per-chat state directory.
#[derive(Clone, Debug)]
pub struct ChatStore {
    dir: PathBuf,
}

impl ChatStore {
    /// The real store: `{data_dir}/whatsapp/chats`.
    #[must_use]
    pub fn new() -> Self {
        Self::at(data_dir_override().join("whatsapp").join("chats"))
    }

    /// A store rooted at `dir` (tests, or an explicit data directory).
    #[must_use]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The file backing `chat_key`.
    ///
    /// The chat id is hashed so a JID (`…@s.whatsapp.net`, `…@g.us`) never
    /// maps to a path separator, a device suffix or a case-insensitive
    /// collision; the hash is a stable 64-bit FNV-1a value.
    #[must_use]
    pub fn path(&self, chat_key: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.json", fnv1a(chat_key)))
    }

    /// Load one chat, falling back to the default on any problem.
    pub async fn load(&self, chat_key: &str) -> StoredChat {
        let path = self.path(chat_key);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return StoredChat::default();
            }
            Err(error) => {
                log::warn!(
                    "morpheus whatsapp: could not read state at {}: {error}; using defaults",
                    path.display()
                );
                return StoredChat::default();
            }
        };
        let state = match serde_json::from_slice::<StoredChat>(&bytes) {
            Ok(state) => state,
            Err(error) => {
                log::warn!(
                    "morpheus whatsapp: state at {} is corrupt ({error}); using defaults",
                    path.display()
                );
                return StoredChat::default();
            }
        };
        if state.version != STATE_VERSION {
            log::warn!(
                "morpheus whatsapp: state at {} has version {} (expected {STATE_VERSION}); using defaults",
                path.display(),
                state.version
            );
            return StoredChat::default();
        }
        state
    }

    /// Persist one chat atomically.
    ///
    /// # Errors
    ///
    /// Returns the I/O or serialization error so the caller can keep the
    /// in-memory state unchanged when the write does not land.
    pub async fn save(&self, chat_key: &str, state: &StoredChat) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|error| format!("could not create {}: {error}", self.dir.display()))?;
        let path = self.path(chat_key);
        let temp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("could not encode chat state: {error}"))?;

        {
            use tokio::io::AsyncWriteExt as _;
            let mut file = tokio::fs::File::create(&temp)
                .await
                .map_err(|error| format!("could not create {}: {error}", temp.display()))?;
            file.write_all(&json)
                .await
                .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
            file.sync_all()
                .await
                .map_err(|error| format!("could not sync {}: {error}", temp.display()))?;
        }
        tokio::fs::rename(&temp, &path).await.map_err(|error| {
            format!(
                "could not replace {} with {}: {error}",
                path.display(),
                temp.display()
            )
        })?;
        // The rename is durable once the directory entry is synced. Not every
        // platform allows opening a directory as a file; failing here is not
        // worth failing the request, since the data file itself is synced.
        if let Ok(dir) = tokio::fs::File::open(&self.dir).await {
            let _ = dir.sync_all().await;
        }
        Ok(())
    }

    /// The directory backing this store (tests assert on it).
    #[cfg(test)]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

/// Stable 64-bit FNV-1a: deterministic across runs and platforms, unlike
/// `DefaultHasher`, whose output is not a stable contract.
fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_chat_is_the_default_state() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        let state = store.load("5511912345678@s.whatsapp.net").await;
        assert_eq!(state.version, STATE_VERSION);
        assert!(state.roster.is_empty());
    }

    #[tokio::test]
    async fn roster_round_trips_per_chat() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        let state = StoredChat {
            roster: vec!["claude".into(), "codex".into()],
            sessions: BTreeMap::from([("claude#1".to_string(), "session-1".to_string())]),
            ..StoredChat::default()
        };
        store
            .save("5511912345678@s.whatsapp.net", &state)
            .await
            .expect("save");

        let loaded = store.load("5511912345678@s.whatsapp.net").await;
        assert_eq!(loaded.roster, state.roster);
        assert_eq!(loaded.sessions["claude#1"], "session-1");

        // A different chat never sees it.
        assert!(
            store
                .load("120363012345678901@g.us")
                .await
                .roster
                .is_empty()
        );
    }

    #[tokio::test]
    async fn humanize_choice_round_trips_and_absent_means_no_choice() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        let key = "5511912345678@s.whatsapp.net";

        assert_eq!(
            store.load(key).await.humanize,
            None,
            "a fresh chat has no choice"
        );

        store
            .save(
                key,
                &StoredChat {
                    humanize: Some(true),
                    ..StoredChat::default()
                },
            )
            .await
            .expect("save");
        assert_eq!(store.load(key).await.humanize, Some(true));

        // State written before the field existed (or by hand without it) is
        // the old behavior, not an error.
        let legacy = serde_json::json!({
            "version": STATE_VERSION,
            "roster": ["claude"],
            "sessions": {},
        });
        tokio::fs::write(
            store.path("legacy"),
            serde_json::to_vec(&legacy).expect("json"),
        )
        .await
        .expect("write legacy state");
        assert_eq!(store.load("legacy").await.humanize, None);
    }

    #[tokio::test]
    async fn a_corrupt_file_degrades_to_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        let key = "5511912345678@s.whatsapp.net";
        tokio::fs::create_dir_all(store.dir())
            .await
            .expect("create store dir");
        tokio::fs::write(store.path(key), b"{not json")
            .await
            .expect("write corrupt state");

        let state = store.load(key).await;
        assert!(state.roster.is_empty());
        assert_eq!(state.version, STATE_VERSION);
    }

    #[tokio::test]
    async fn an_unknown_schema_version_is_ignored() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        let key = "chat";
        tokio::fs::create_dir_all(store.dir())
            .await
            .expect("create store dir");
        tokio::fs::write(store.path(key), br#"{"version": 99, "roster": ["claude"]}"#)
            .await
            .expect("write future state");

        assert!(store.load(key).await.roster.is_empty());
    }

    #[tokio::test]
    async fn the_main_context_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        let mut context = crate::harness::ContextManagerState::default();
        context.next_id = 7;
        context.max_tokens = 123_456;
        let state = StoredChat {
            context: Some(context),
            ..StoredChat::default()
        };
        store
            .save("5511912345678@s.whatsapp.net", &state)
            .await
            .expect("save");

        let loaded = store.load("5511912345678@s.whatsapp.net").await;
        let context = loaded.context.expect("the context was persisted");
        assert_eq!(context.next_id, 7);
        assert_eq!(context.max_tokens, 123_456);
    }

    #[tokio::test]
    async fn saves_leave_no_temporary_file_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = ChatStore::at(dir.path());
        store
            .save("chat", &StoredChat::default())
            .await
            .expect("save");

        let mut entries = tokio::fs::read_dir(store.dir()).await.expect("read dir");
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names.len(), 1, "only the state file remains: {names:?}");
        assert!(names[0].ends_with(".json"));
    }

    #[test]
    fn chat_filenames_are_stable_and_distinct() {
        let store = ChatStore::at("/tmp/whatsapp");
        assert_eq!(
            store.path("5511912345678@s.whatsapp.net"),
            store.path("5511912345678@s.whatsapp.net")
        );
        assert_ne!(
            store.path("5511912345678@s.whatsapp.net"),
            store.path("5511912345679@s.whatsapp.net")
        );
        let name = store
            .path("120363012345678901@g.us")
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .to_string();
        let stem = name.strip_suffix(".json").expect("json extension");
        assert_eq!(stem.len(), 16, "16 hex digits: {stem}");
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()), "{stem}");
    }

    #[test]
    fn fnv1a_is_the_documented_constant() {
        // FNV-1a's published offset basis and prime produce this value for an
        // empty input; a refactor of the loop must not silently change the
        // filenames of every existing chat.
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
    }
}
