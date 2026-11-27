//! The decision-model weights installer: what to download and how to install
//! it.
//!
//! Lives in the library, gated behind the `onnx` feature: it depends on the
//! setup config (`crate::setup`) and the `morpheus-onnx` hub.
//!
//! The install itself is `morpheus_onnx::hub::install`: cache-first,
//! `sha256sums.txt` verified before success. Downloads are driven by the TUI's
//! Download router — never by a chat turn — so this module exposes a plain
//! blocking call with a progress callback and no event plumbing of its own.

#[cfg(feature = "onnx")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "onnx")]
use crate::setup::{DecisionHub, DecisionModel};

/// The graph candidates, quantized first (the mirror may not ship the int8
/// twin yet — the installer falls back to fp32).
const GRAPHS: &[&str] = &["laya.int8.onnx", "laya.onnx"];

/// One install target resolved from the setup config: which repo, which
/// checkpoint inside it, which revision pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeightsTarget {
    /// The mirror repo id (the effective one: env override applied).
    pub repo: String,
    /// The checkpoint's subfolder inside the mirror (`None` = repo root).
    pub subfolder: Option<String>,
    /// The revision pin. `Some` when the config pins one; `None` resolves
    /// the mirror's current revision on the first install — the snapshot
    /// stays cached, so later sessions reuse it without re-resolving.
    pub revision: Option<String>,
    /// The model's user-facing name (upstream family + checkpoint, or the
    /// repo id) for the download router's row copy.
    pub display_name: String,
}

impl WeightsTarget {
    /// The wire kind the decision daemon loads through: a `Custom` kind
    /// addressing the mirror checkpoint directly. Building it HERE (not by
    /// serde round-trip of the configured kind) is the whole point of the
    /// mirror indirection: the daemon's loader cache-probes the mirror
    /// snapshot the installer filled, and never touches the upstream repos.
    pub fn to_kind(&self) -> crate::daemon::decision::Kind {
        crate::daemon::decision::Kind::Custom {
            repo: self.repo.clone(),
            subfolder: self.subfolder.clone(),
        }
    }
}

/// Resolve the install target from the setup config. `None` = nothing to
/// install: a `Custom` kind pointing at a LOCAL directory loads directly
/// (no mirror, no download).
///
/// The revision pin is scoped to the MIRROR: `pinned_revision` records the
/// SHA the mirror resolved to on its first download, so it applies only
/// when the target's repo IS the effective mirror. A `Custom` kind
/// pointing at a different hub repo never carries it — a stale mirror SHA
/// would 404 against that repo and permanently disable the audit.
#[cfg(feature = "onnx")]
pub fn resolve_target(model: &DecisionModel, hub: &DecisionHub) -> Option<WeightsTarget> {
    let repo = hub.effective_repo();
    // The pin is repo-scoped (DecisionHub::pin_for): it applies only when
    // the target's repo is the one the SHA was resolved from.
    let mirror_pin = hub.pin_for(&repo);
    let display_name = model.display_name().to_string();
    match model {
        // The named kinds map onto the mirror's per-kind subfolders. The
        // mirror nests every kind (including `english/`, unlike the
        // upstream bundle where the English root sits at the repo root).
        DecisionModel::English => Some(WeightsTarget {
            repo: repo.clone(),
            subfolder: Some("english".to_string()),
            revision: mirror_pin,
            display_name,
        }),
        DecisionModel::Multilingual => Some(WeightsTarget {
            repo: repo.clone(),
            subfolder: Some("multilingual".to_string()),
            revision: mirror_pin,
            display_name,
        }),
        DecisionModel::TypedDecisions => Some(WeightsTarget {
            repo: repo.clone(),
            subfolder: Some("typed-decisions".to_string()),
            revision: mirror_pin,
            display_name,
        }),
        DecisionModel::Custom {
            repo: custom,
            subfolder,
        } => {
            if std::path::Path::new(custom).is_dir() {
                // Local checkpoint: the loader resolves it directly.
                return None;
            }
            // The pin travels ONLY with the mirror: a custom repo has (and
            // needs) its own revision story.
            let revision = (*custom == repo).then_some(mirror_pin).flatten();
            Some(WeightsTarget {
                repo: custom.clone(),
                subfolder: subfolder.clone(),
                revision,
                display_name,
            })
        }
    }
}

/// The revision the target is already cached under, if any.
#[cfg(feature = "onnx")]
fn cached_revision(target: &WeightsTarget) -> Option<String> {
    morpheus_onnx::hub::cache_status(
        &target.repo,
        target.subfolder.as_deref(),
        GRAPHS,
        target.revision.as_deref(),
    )
    .map(|(_, revision, _)| revision)
}

/// Is the target already fully cached (no download needed)?
#[cfg(feature = "onnx")]
pub fn is_cached(target: &WeightsTarget) -> bool {
    cached_revision(target).is_some()
}

/// Install the target's checkpoint into the shared hf-hub cache and return the
/// revision it was installed under.
///
/// Blocking: the TUI runs it on a worker thread so the render loop never
/// stalls on the network. `progress` receives `(downloaded_bytes,
/// total_bytes)` across every file of the install (`total` grows as further
/// files start). A cache hit returns without touching the network.
#[cfg(feature = "onnx")]
pub fn install(
    target: &WeightsTarget,
    mut progress: impl FnMut(u64, u64) + Send + 'static,
) -> Result<String, String> {
    if let Some(revision) = cached_revision(target) {
        return Ok(revision);
    }

    let files = Arc::new(Mutex::new(Files::default()));
    let init_files = Arc::clone(&files);
    let update_files = Arc::clone(&files);
    let callbacks = morpheus_onnx::hub::ProgressCallbacks::new(
        move |size, filename| {
            if let Ok(mut files) = init_files.lock() {
                files.start_file(filename.to_string(), size as u64);
            }
        },
        move |delta| {
            let Ok(mut files) = update_files.lock() else {
                return;
            };
            files.advance(delta as u64);
            progress(files.done.min(files.total), files.total);
        },
        move || {},
    );

    let request = morpheus_onnx::hub::InstallRequest {
        repo: &target.repo,
        subfolder: target.subfolder.as_deref(),
        revision: target.revision.as_deref(),
        graphs: GRAPHS,
        expected_sha256: None,
        progress: callbacks,
    };
    morpheus_onnx::hub::install(request)
        .map(|installed| installed.revision)
        .map_err(|e| e.message().to_string())
}

/// Per-file download accounting: totals per repo path, the current file's
/// position, and the derived sums.
#[cfg(feature = "onnx")]
#[derive(Default)]
struct Files {
    /// `(total_bytes, downloaded_bytes)` per file, insertion-ordered (the
    /// current file is the last one `start_file` saw).
    table: std::collections::HashMap<String, (u64, u64)>,
    current: Option<String>,
    done: u64,
    total: u64,
}

#[cfg(feature = "onnx")]
impl Files {
    /// `init(size, filename)`: a file starts (or resumes). A NEW file adds
    /// its size to the running total; a RE-init (resume/retry) replaces the
    /// file's previous accounting — its old size and position leave the
    /// sums, the resumed window re-accumulates from zero.
    fn start_file(&mut self, name: String, size: u64) {
        match self.table.get_mut(&name) {
            Some(entry) => {
                let (prev_size, prev_pos) = *entry;
                self.done = self.done.saturating_sub(prev_pos);
                self.total = self.total.saturating_sub(prev_size) + size;
                *entry = (size, 0);
            }
            None => {
                self.table.insert(name.clone(), (size, 0));
                self.total += size;
            }
        }
        self.current = Some(name);
    }

    /// `update(delta)`: advance the current file.
    fn advance(&mut self, delta: u64) {
        if let Some(name) = self.current.clone()
            && let Some((_, pos)) = self.table.get_mut(&name)
        {
            *pos += delta;
            self.done += delta;
        }
    }
}

#[cfg(all(test, feature = "onnx"))]
mod tests {
    use super::*;

    fn hub() -> DecisionHub {
        DecisionHub::default()
    }

    #[test]
    fn named_kinds_map_to_mirror_subfolders() {
        let h = hub();
        let t = resolve_target(&DecisionModel::English, &h).unwrap();
        assert_eq!(t.repo, "Zaluski/laya-onnx");
        assert_eq!(t.subfolder.as_deref(), Some("english"));
        assert!(t.revision.is_none());

        let t = resolve_target(&DecisionModel::Multilingual, &h).unwrap();
        assert_eq!(t.subfolder.as_deref(), Some("multilingual"));

        let t = resolve_target(&DecisionModel::TypedDecisions, &h).unwrap();
        assert_eq!(t.subfolder.as_deref(), Some("typed-decisions"));
    }

    #[test]
    fn local_custom_kind_needs_no_install() {
        let dir = std::env::temp_dir().join("morpheus-weights-local-test");
        std::fs::create_dir_all(&dir).unwrap();
        let model = DecisionModel::Custom {
            repo: dir.to_string_lossy().into_owned(),
            subfolder: None,
        };
        assert!(resolve_target(&model, &hub()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_custom_kind_installs_from_its_own_repo() {
        let model = DecisionModel::Custom {
            repo: "org/their-checkpoint".to_string(),
            subfolder: None,
        };
        let t = resolve_target(&model, &hub()).unwrap();
        assert_eq!(t.repo, "org/their-checkpoint");
        assert_eq!(t.subfolder, None);
    }

    #[test]
    fn config_pin_flows_into_the_target() {
        let mut h = hub();
        h.pinned_repo = h.effective_repo();
        h.pinned_revision = "abc123".to_string();
        let t = resolve_target(&DecisionModel::English, &h).unwrap();
        assert_eq!(t.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn a_foreign_pin_never_applies() {
        // A pin recorded from ANOTHER repo (or a legacy pre-scoping pin)
        // must not constrain the current mirror: the next install
        // re-resolves and re-records a properly scoped pin.
        let mut h = hub();
        h.pinned_revision = "abc123".to_string();
        assert_eq!(h.pin_for(&h.effective_repo()), None, "no pinned_repo");

        h.pinned_repo = "other/mirror".to_string();
        assert_eq!(h.pin_for(&h.effective_repo()), None, "different repo");
        assert_eq!(h.pin_for("other/mirror").as_deref(), Some("abc123"));

        let t = resolve_target(&DecisionModel::English, &h).unwrap();
        assert!(
            t.revision.is_none(),
            "foreign pin does not reach the target"
        );

        // A custom repo (even equal to the mirror) carries the pin only
        // when the pin belongs to that repo.
        h.pinned_repo = h.effective_repo();
        let t = resolve_target(
            &DecisionModel::Custom {
                repo: h.effective_repo(),
                subfolder: None,
            },
            &h,
        )
        .unwrap();
        assert_eq!(t.revision.as_deref(), Some("abc123"));
    }

    #[test]
    fn target_maps_to_a_custom_wire_kind() {
        let t = resolve_target(&DecisionModel::English, &hub()).unwrap();
        assert_eq!(t.to_kind().name(), "Zaluski/laya-onnx/english");
    }

    #[test]
    fn named_kinds_carry_their_display_name() {
        for (model, name) in [
            (DecisionModel::English, "Laya"),
            (DecisionModel::Multilingual, "Laya-Multilingual"),
            (DecisionModel::TypedDecisions, "Laya-Typed-Decisions"),
        ] {
            let t = resolve_target(&model, &hub()).unwrap();
            assert_eq!(t.display_name, name);
        }
    }

    #[test]
    fn file_accounting_survives_reinit_and_sequential_files() {
        let mut f = Files::default();
        f.start_file("a.json".into(), 100);
        f.advance(40);
        assert_eq!((f.done, f.total), (40, 100));
        // Resume: init resets the file's position without double-counting.
        f.start_file("a.json".into(), 100);
        f.advance(100);
        assert_eq!((f.done, f.total), (100, 100));
        // The next file starts after the first finished.
        f.start_file("graph.onnx".into(), 1_000_000);
        f.advance(500_000);
        assert_eq!((f.done, f.total), (500_100, 1_000_100));
    }
}
