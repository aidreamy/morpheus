use super::*;
use crate::coordinator::workflow::{TaskSpec, Workflow};
use std::{fs, process::Command};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    git(dir.path(), &["config", "core.autocrlf", "false"]);
    fs::write(dir.path().join("shared"), "base\n").unwrap();
    git(dir.path(), &["add", "shared"]);
    git(dir.path(), &["commit", "-m", "base"]);
    dir
}

fn patch(repo: &Path, branch: &str, name: &str, value: &str) -> String {
    git(repo, &["switch", "-c", branch, "main"]);
    fs::write(repo.join(name), value).unwrap();
    git(repo, &["add", name]);
    git(repo, &["commit", "-m", branch]);
    let commit = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["switch", "main"]);
    commit
}

const BASE: &str = "refs/heads/main";

fn plan() -> VerificationSpec {
    VerificationSpec {
        base_ref: BASE.into(),
        command: ["git", "diff", "--quiet", "HEAD"].map(String::from).into(),
    }
}

fn workflow(path: &Path, spec: VerificationSpec) -> Workflow {
    Workflow::create(
        path,
        vec![
            TaskSpec {
                id: "patch".into(),
                input: String::new(),
                dependencies: Vec::new(),
                verification: Some(spec),
            },
            TaskSpec {
                id: "publish".into(),
                input: String::new(),
                dependencies: vec!["patch".into()],
                verification: None,
            },
        ],
    )
    .unwrap()
}

// Zuul's dependent gate checks changes together, then discards results for a
// composition that included a failed change.
// https://opendev.org/zuul/zuul/src/commit/898b7ab13fa6f422aad39a0f2c3be4d0caa26ffc/tests/unit/test_scheduler.py#L883
#[test]
fn independently_green_patches_can_fail_as_a_batch() {
    let dir = fixture();
    let repo = dir.path();
    let base = git(repo, &["rev-parse", BASE]);
    let a = patch(repo, "a", "left", "left");
    let b = patch(repo, "b", "right", "right");
    let check = |tree: &Path| Ok(!(tree.join("left").exists() && tree.join("right").exists()));
    assert!(prepare_integration(repo, &plan(), std::slice::from_ref(&a), check).is_ok());
    assert!(prepare_integration(repo, &plan(), std::slice::from_ref(&b), check).is_ok());
    let result = prepare_integration(repo, &plan(), &[a, b], check);
    assert_eq!(result.unwrap_err(), VerificationError::CheckFailed);
    assert_eq!(git(repo, &["rev-parse", BASE]), base);
    assert!(!repo.join("left").exists());
    assert!(!repo.join("right").exists());
}

// https://opendev.org/zuul/zuul/src/commit/898b7ab13fa6f422aad39a0f2c3be4d0caa26ffc/tests/unit/test_scheduler.py#L1605
#[test]
fn conflicting_patches_leave_the_user_checkout_untouched() {
    let dir = fixture();
    let repo = dir.path();
    let a = patch(repo, "a", "shared", "left\n");
    let b = patch(repo, "b", "shared", "right\n");
    fs::write(repo.join("shared"), "uncommitted user work").unwrap();
    let worktrees = git(repo, &["worktree", "list", "--porcelain"]);
    let result = prepare_integration(repo, &plan(), &[a, b], |_| {
        panic!("conflict must bypass checks")
    });
    assert_eq!(
        result.unwrap_err(),
        VerificationError::Conflict(vec!["shared".into()])
    );
    assert_eq!(
        fs::read_to_string(repo.join("shared")).unwrap(),
        "uncommitted user work"
    );
    assert_eq!(git(repo, &["worktree", "list", "--porcelain"]), worktrees);
}

// A green result belongs to the exact base and candidate tested. Zuul
// test_new_patchset_dequeues_old_on_head exercises invalidation of stale work.
// https://opendev.org/zuul/zuul/src/commit/898b7ab13fa6f422aad39a0f2c3be4d0caa26ffc/tests/unit/test_scheduler.py#L3243
#[test]
fn advancing_the_base_during_checks_invalidates_the_green_result() {
    let dir = fixture();
    let repo = dir.path();
    let a = patch(repo, "a", "left", "left");
    let result = prepare_integration(repo, &plan(), &[a], |_| {
        git(repo, &["commit", "--allow-empty", "-m", "advance base"]);
        Ok(true)
    })
    .unwrap();
    assert_eq!(result.publish().unwrap_err(), VerificationError::StaleBase);
    assert_eq!(git(repo, &["for-each-ref", "refs/morpheus/verified/"]), "");
}

#[test]
fn verified_reference_points_to_the_commit_that_ran_the_checks() {
    let dir = fixture();
    let repo = dir.path();
    let base = git(repo, &["rev-parse", BASE]);
    let a = patch(repo, "a", "left", "left");
    let b = patch(repo, "b", "right", "right");
    let spec = plan();
    let result = verify_patches(repo, &spec, &[a, b]).unwrap();
    let checked = git(&result.checkout.path, &["rev-parse", "HEAD"]);
    assert!(result.checkout.path.join("left").exists());
    assert!(result.checkout.path.join("right").exists());
    assert_eq!(git(repo, &["for-each-ref", "refs/morpheus/verified/"]), "");
    let state = tempfile::tempdir().unwrap();
    let path = state.path().join("run.json");
    let mut workflow = workflow(&path, spec);
    let attempt = workflow.claim("patch").unwrap();
    let Outcome::Verified { reference, .. } = workflow
        .complete_verified("patch", attempt, &result, "combined checks passed".into())
        .unwrap()
    else {
        panic!("expected verified outcome")
    };
    assert_eq!(git(repo, &["rev-parse", &reference]), checked);
    assert_eq!(result.base, base);
    assert_eq!(git(repo, &["rev-parse", BASE]), base);
    drop(workflow);
    let workflow = Workflow::open(&path).unwrap();
    assert_eq!(workflow.ready(), ["publish"]);
    assert!(matches!(
        workflow.pending_reports().as_slice(),
        [("patch", _, Outcome::Verified { commit, reference: saved, .. })]
            if commit == &checked && saved == &reference
    ));
}

#[test]
fn a_check_that_rewrites_tracked_files_leaves_no_verification_receipt() {
    let dir = fixture();
    let repo = dir.path();
    let a = patch(repo, "a", "left", "left");
    let result = prepare_integration(repo, &plan(), &[a], |tree| {
        fs::write(tree.join("left"), "rewritten by checks")?;
        Ok(true)
    });
    assert_eq!(result.unwrap_err(), VerificationError::ChangedCandidate);
    assert_eq!(git(repo, &["for-each-ref", "refs/morpheus/verified/"]), "");
}

#[test]
fn index_flags_cannot_hide_tracked_changes_from_the_verifier() {
    let dir = fixture();
    let repo = dir.path();
    let a = patch(repo, "a", "left", "left");
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let result = prepare_integration(repo, &plan(), std::slice::from_ref(&a), |tree| {
            git(tree, &["update-index", flag, "left"]);
            fs::write(tree.join("left"), "rewritten by checks")?;
            assert_eq!(git(tree, &["status", "--porcelain"]), "");
            Ok(true)
        });
        assert_eq!(result.unwrap_err(), VerificationError::ChangedCandidate);
    }
}

#[test]
fn verification_checks_original_objects_even_with_replace_refs() {
    let dir = fixture();
    let repo = dir.path();
    let main = git(repo, &["rev-parse", BASE]);
    let original = patch(repo, "original", "shared", "original\n");
    let replacement = patch(repo, "replacement", "shared", "replacement\n");
    git(repo, &["merge", "--ff-only", &original]);
    git(repo, &["replace", &original, &replacement]);
    assert_eq!(git(repo, &["show", "HEAD:shared"]), "replacement");
    let prepared = verify_patches(repo, &plan(), &[main]).unwrap();
    assert_eq!(
        fs::read_to_string(prepared.checkout.path.join("shared")).unwrap(),
        "original\n"
    );
    assert_eq!(prepared.commit, original);
}

#[test]
fn publication_updates_a_symbolic_receipt_without_advancing_its_target() {
    let dir = fixture();
    let repo = dir.path();
    let base = git(repo, &["rev-parse", BASE]);
    git(repo, &["branch", "other"]);
    let a = patch(repo, "a", "left", "left");
    let prepared = verify_patches(repo, &plan(), &[a]).unwrap();
    let reference = format!("refs/morpheus/verified/{}", prepared.commit);
    git(repo, &["symbolic-ref", &reference, "refs/heads/other"]);
    prepared.publish().unwrap();
    assert_eq!(git(repo, &["rev-parse", "refs/heads/other"]), base);
    assert_eq!(git(repo, &["rev-parse", &reference]), prepared.commit);
}

#[test]
fn commit_input_rejects_tags_and_sha256_abbreviations() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-b", "main", "--object-format=sha256"]);
    git(repo, &["commit", "--allow-empty", "-m", "base"]);
    git(
        repo,
        &["-c", "tag.gpgSign=false", "tag", "-m", "annotated", "test"],
    );
    let commit = git(repo, &["rev-parse", "HEAD"]);
    let tag = git(repo, &["rev-parse", "refs/tags/test"]);
    for wrong in [tag, commit[..40].into()] {
        let result = verify_patches(repo, &plan(), &[wrong]);
        assert!(matches!(result, Err(VerificationError::Git(_))));
    }
    assert!(verify_patches(repo, &plan(), &[commit]).is_ok());
}

#[test]
fn only_the_current_attempts_planned_submission_can_publish() {
    let dir = fixture();
    let repo = dir.path();
    let a = patch(repo, "a", "left", "left");
    let b = patch(repo, "b", "right", "right");
    let spec = plan();
    let first = verify_patches(repo, &spec, std::slice::from_ref(&a)).unwrap();
    let second = verify_patches(repo, &spec, &[b]).unwrap();
    let different_check = VerificationSpec {
        command: ["git", "diff", "--quiet"].map(String::from).into(),
        ..spec.clone()
    };
    let wrong_plan = verify_patches(repo, &different_check, std::slice::from_ref(&a)).unwrap();
    let state = tempfile::tempdir().unwrap();
    let path = state.path().join("run.json");
    let mut run = workflow(&path, spec);
    let obsolete = run.claim("patch").unwrap();
    run.recover_interrupted().unwrap();
    let attempt = run.claim("patch").unwrap();
    assert!(
        run.complete_verified("patch", obsolete, &first, String::new())
            .is_err()
    );
    assert!(
        run.complete_verified("patch", attempt, &wrong_plan, String::new())
            .is_err()
    );
    assert_eq!(git(repo, &["for-each-ref", "refs/morpheus/verified/"]), "");

    let report = run
        .complete_verified("patch", attempt, &first, "passed".into())
        .unwrap();
    assert!(
        run.complete_verified("patch", attempt, &second, String::new())
            .is_err()
    );
    assert_eq!(
        git(
            repo,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/morpheus/verified/"
            ]
        ),
        first.commit
    );
    drop(run);
    let run = Workflow::open(&path).unwrap();
    assert_eq!(
        run.verified_result("patch", attempt, &[a]).unwrap(),
        Some(&report)
    );
}

#[cfg(unix)]
#[test]
fn verification_detects_executable_mode_changes_when_repo_ignores_them() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixture();
    let repo = dir.path();
    git(repo, &["config", "core.filemode", "false"]);
    let a = patch(repo, "a", "left", "left");
    let result = prepare_integration(repo, &plan(), &[a], |tree| {
        fs::set_permissions(tree.join("left"), fs::Permissions::from_mode(0o755))?;
        assert_eq!(git(tree, &["status", "--porcelain"]), "");
        Ok(true)
    });
    assert_eq!(result.unwrap_err(), VerificationError::ChangedCandidate);
}

#[cfg(unix)]
#[test]
fn verification_cannot_publish_when_the_checkpoint_cannot_be_staged() {
    let dir = fixture();
    let repo = dir.path();
    let a = patch(repo, "a", "left", "left");
    let spec = plan();
    let result = verify_patches(repo, &spec, &[a]).unwrap();
    let state = tempfile::tempdir().unwrap();
    let original = state.path().join("original");
    fs::create_dir(&original).unwrap();
    let mut run = workflow(&original.join("run.json"), spec);
    let attempt = run.claim("patch").unwrap();
    fs::rename(&original, state.path().join("moved")).unwrap();
    assert!(
        run.complete_verified("patch", attempt, &result, String::new())
            .is_err()
    );
    assert!(run.running("patch", attempt).is_ok());
    assert_eq!(git(repo, &["for-each-ref", "refs/morpheus/verified/"]), "");
}
