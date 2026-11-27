use super::*;

fn task(id: &str, dependencies: &[&str]) -> TaskSpec {
    TaskSpec {
        id: id.into(),
        input: format!("Run {id}"),
        dependencies: dependencies.iter().map(|id| (*id).into()).collect(),
        verification: None,
    }
}

fn success(report: &str) -> Outcome {
    Outcome::Succeeded {
        report: report.into(),
    }
}

// LangGraph test_pending_writes_resume, ported to task attempts and a real
// checkpoint file. The successful sibling never becomes dispatchable again.
// https://github.com/langchain-ai/langgraph/blob/3e3dbd42c5329dd404669ccd923c005c814bee3a/libs/langgraph/tests/test_pregel.py#L891
#[test]
fn reopen_reuses_completed_work_and_recovers_only_interrupted_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.json");
    let mut run = Workflow::create(
        &path,
        vec![
            task("left", &[]),
            task("right", &[]),
            task("join", &["left", "right"]),
        ],
    )
    .unwrap();
    let left = run.claim("left").unwrap();
    let old_right = run.claim("right").unwrap();
    assert!(run.claim("join").is_err());
    assert!(
        run.complete("left", left, success("left artifact"))
            .unwrap()
    );
    drop(run);

    let mut run = Workflow::open(&path).unwrap();
    assert_eq!(run.recover_interrupted().unwrap(), ["right"]);
    assert_eq!(run.ready(), ["right"]);
    assert_eq!(
        run.pending_reports(),
        [("left", left, &success("left artifact"))]
    );
    let right = run.claim("right").unwrap();
    assert!(right > old_right);
    assert!(
        run.complete("right", old_right, success("late result"))
            .is_err()
    );
    assert!(run.ready().is_empty());
    assert!(
        run.complete("right", right, success("right artifact"))
            .unwrap()
    );
    assert_eq!(run.ready(), ["join"]);
}

// Temporal TestSignalWorkflow_DuplicateRequest uses a durable id to keep
// repeated delivery from recording a second result.
// https://github.com/temporalio/temporal/blob/c2ec24c01c519234de9dd01881b968c1b14473f2/tests/signal_workflow_test.go#L246
#[test]
fn unacknowledged_result_survives_reopen_and_duplicate_delivery_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.json");
    let mut run = Workflow::create(&path, vec![task("worker", &[])]).unwrap();
    let attempt = run.claim("worker").unwrap();
    let report = success("original result");
    assert!(run.complete("worker", attempt, report.clone()).unwrap());
    drop(run);

    let mut run = Workflow::open(&path).unwrap();
    assert_eq!(run.pending_reports(), [("worker", attempt, &report)]);
    assert!(!run.complete("worker", attempt, report.clone()).unwrap());
    assert!(
        run.complete("worker", attempt, success("conflicting result"))
            .is_err()
    );
    assert!(run.acknowledge("worker", attempt).unwrap());
    drop(run);

    let mut run = Workflow::open(&path).unwrap();
    assert!(run.pending_reports().is_empty());
    assert!(!run.acknowledge("worker", attempt).unwrap());
    assert!(!run.complete("worker", attempt, report).unwrap());
}

#[test]
fn dependent_task_stays_blocked_on_failure_or_unverified_patch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.json");
    let mut patch = task("patch", &[]);
    patch.verification = Some(VerificationSpec {
        base_ref: "refs/heads/main".into(),
        command: vec!["cargo".into(), "test".into()],
    });
    let mut run = Workflow::create(&path, vec![patch, task("publish", &["patch"])]).unwrap();
    let attempt = run.claim("patch").unwrap();
    assert!(
        run.complete("patch", attempt, success("worker says green"))
            .is_err()
    );
    assert!(run.ready().is_empty());
    assert!(
        run.complete(
            "patch",
            attempt,
            Outcome::Failed {
                report: "test failed".into()
            }
        )
        .unwrap()
    );
    assert!(run.ready().is_empty());
    assert!(run.claim("publish").is_err());
    assert!(run.recover_interrupted().unwrap().is_empty());
    run.retry_failed("patch").unwrap();
    let new_attempt = run.claim("patch").unwrap();
    assert!(new_attempt > attempt);
    assert!(matches!(
        run.pending_reports().as_slice(),
        [("patch", _, Outcome::Failed { .. })]
    ));
}

#[test]
fn invalid_graph_never_creates_a_checkpoint_and_active_writer_is_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.json");
    for invalid in [
        vec![task("a", &["unknown"])],
        vec![task("a", &["b"]), task("b", &["a"])],
        vec![task("a", &[]), task("a", &[])],
    ] {
        assert!(Workflow::create(&path, invalid).is_err());
        assert!(!path.exists());
    }
    let run = Workflow::create(&path, vec![task("a", &[])]).unwrap();
    assert!(Workflow::open(&path).is_err());
    drop(run);
    assert!(Workflow::open(&path).is_ok());
}

#[test]
fn checkpoint_names_cannot_replace_another_workflows_lock() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("run");
    let second = dir.path().join("run.lock");
    let run = Workflow::create(&first, vec![task("first", &[])]).unwrap();
    let mut other = Workflow::create(&second, vec![task("second", &[])]).unwrap();
    other.claim("second").unwrap();
    assert!(Workflow::open(&first).is_err());
    let reserved = dir.path().join(LOCK_DIRECTORY).join("new");
    assert!(Workflow::create(&reserved, vec![task("new", &[])]).is_err());
    drop(run);
    assert_eq!(Workflow::open(&first).unwrap().ready(), ["first"]);
}

#[cfg(unix)]
#[test]
fn symlink_uses_the_real_checkpoint_and_hard_links_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.json");
    let alias = dir.path().join("alias.json");
    let run = Workflow::create(&path, vec![task("worker", &[])]).unwrap();
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    assert!(Workflow::open(&alias).is_err());
    drop(run);
    let mut run = Workflow::open(&alias).unwrap();
    assert_eq!(run.claim("worker").unwrap(), 1);
    drop(run);
    assert!(alias.is_symlink());
    assert!(Workflow::open(&path).unwrap().running("worker", 1).is_ok());

    let hard_link = dir.path().join("hard-link.json");
    std::fs::hard_link(&path, &hard_link).unwrap();
    assert!(Workflow::open(&path).is_err());
    assert!(Workflow::open(&hard_link).is_err());
}

#[test]
fn repeated_dependency_delivers_a_single_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut run = Workflow::create(
        &dir.path().join("run.json"),
        vec![task("one", &[]), task("two", &["one", "one"])],
    )
    .unwrap();
    let attempt = run.claim("one").unwrap();
    run.complete("one", attempt, success("result")).unwrap();
    assert_eq!(
        run.dependency_results("two").unwrap(),
        [("one", &success("result"))]
    );
}
