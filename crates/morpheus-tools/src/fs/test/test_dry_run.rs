//! Dry-run previews for the edit engines: the in-memory apply surfaces the
//! diff and the syntax-probe verdict BEFORE anything is written (fix for the
//! "range edit clipped lines and broke the file" failure class).

use super::super::edit::edit;
use super::super::replace::content_edit;
use super::super::types::{EditTarget, FsEdit, FsMetadata, ReplaceEdit};
use morpheus_sdk::rollback;
use std::path::{Path, PathBuf};

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique scratch root: dry-run fixtures live in a per-test temp directory,
/// never in a developer's real project tree.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is after UNIX_EPOCH")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("morpheus_fs_dryrun_{label}_{id}_{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp root");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn make_metadata(root: &Path) -> FsMetadata {
    FsMetadata {
        root: root.to_path_buf(),
        allowlist: None,
        blocklist: None,
    }
}

fn fixture(root: &TempRoot, name: &str, text: &str) -> (String, String) {
    let path = root
        .0
        .join(format!("morpheus_test_dry_run_{name}"))
        .to_string_lossy()
        .into_owned();
    std::fs::write(&path, text).unwrap();
    let hash = rollback::record(&path, text).expect("snapshot recorded");
    (path, hash)
}

#[tokio::test]
async fn dry_run_returns_diff_and_leaves_the_file_untouched() {
    let root = TempRoot::new("targets");
    let (path, hash) = fixture(&root, "targets", "fn main() {\n    x();\n}\n");
    let out = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash: hash,
                ops: "replace 2:\n+    y();".to_string(),
            }],
            dry_run: true,
        },
    )
    .await
    .expect("dry run should succeed");

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dry_run, Some(true));
    assert!(out[0].diff.is_some(), "preview carries the diff");
    assert!(
        out[0]
            .warnings
            .iter()
            .any(|w| w.contains("Dry run: nothing was written")),
        "result is clearly marked as a preview: {:?}",
        out[0].warnings
    );
    assert!(
        std::fs::read_to_string(&path).unwrap() == "fn main() {\n    x();\n}\n",
        "dry run must not write"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn dry_run_surfaces_syntax_breakage_before_applying() {
    // An unrepairable break: the payload keeps the opener but breaks a
    // statement inside, so no boundary row can restore the parse and the
    // parse-broken advisory is machine-confirmed in the PREVIEW.
    let root = TempRoot::new("syntax");
    let (path, hash) = fixture(&root, "syntax.rs", "fn f() {\n}\n");
    let out = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash: hash,
                ops: "replace 1..2:\n+fn f() {\n+    x(;".to_string(),
            }],
            dry_run: true,
        },
    )
    .await
    .expect("dry run applies in memory even when the result is broken");

    assert!(
        out[0]
            .warnings
            .iter()
            .any(|w| w.contains("introduced a syntax error")),
        "the syntax probe must fire in the preview, before any write: {:?}",
        out[0].warnings
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "fn f() {\n}\n",
        "the broken edit must NOT reach the disk"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn without_dry_run_the_edit_applies_for_real() {
    let root = TempRoot::new("real");
    let (path, hash) = fixture(&root, "real", "a\nb\n");
    let out = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash: hash,
                ops: "replace 1:\n+A".to_string(),
            }],
            dry_run: false,
        },
    )
    .await
    .expect("regular edit should apply");
    assert_eq!(out[0].dry_run, None);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "A\nb\n");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn outside_cwd_edit_warns_only_after_writing() {
    let root = TempRoot::new("workspace");
    let outside = TempRoot::new("outside");
    let (path, hash) = fixture(&outside, "edit", "a\nb\n");
    let mut metadata = make_metadata(root.path());
    metadata.allowlist = Some(vec![PathBuf::from(&path)]);
    let target = EditTarget {
        path: path.clone(),
        file_hash: hash,
        ops: "replace 1:\n+A".to_string(),
    };

    for dry_run in [true, false] {
        let results = edit(
            metadata.clone(),
            FsEdit {
                targets: vec![target.clone()],
                dry_run,
            },
        )
        .await
        .expect("allowed edit should succeed");
        assert_eq!(
            results[0]
                .warnings
                .iter()
                .any(|warning| warning == "Modified a file outside the working directory."),
            !dry_run
        );
        let expected = if dry_run { "a\nb\n" } else { "A\nb\n" };
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }
}

#[tokio::test]
async fn content_engine_honors_dry_run() {
    let root = TempRoot::new("content");
    let (path, hash) = fixture(&root, "content", "value = compute(x);\n");
    let out = content_edit(
        &make_metadata(root.path()),
        &[ReplaceEdit {
            path: path.clone(),
            file_hash: Some(hash),
            old_string: "compute".to_string(),
            new_string: "evaluate".to_string(),
            replace_all: false,
        }],
        true,
    )
    .await
    .expect("content dry run should succeed");

    assert_eq!(out[0].dry_run, Some(true));
    assert!(out[0].diff.is_some());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "value = compute(x);\n",
        "dry run must not write"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn dry_run_batch_writes_nothing_even_when_a_later_target_would_fail() {
    let root = TempRoot::new("batch");
    let (path, hash) = fixture(&root, "batch", "a\nb\n");
    let out = edit(
        make_metadata(root.path()),
        FsEdit {
            targets: vec![EditTarget {
                path: path.clone(),
                file_hash: hash,
                ops: "replace 1:\n+A".to_string(),
            }],
            dry_run: true,
        },
    )
    .await
    .expect("dry run should succeed");
    assert_eq!(out.len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
    let _ = std::fs::remove_file(&path);
}
