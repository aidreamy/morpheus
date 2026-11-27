use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use morpheus::coordinator::{
    verification::{PreparedIntegration, VerificationError, verify_patches},
    workflow::{Outcome, Workflow},
};
use serde_json::{Value, json};

use super::{CliError, Invocation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    state: PathBuf,
    action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Init(PathBuf),
    Status,
    Claim(String),
    Recover,
    Retry(String),
    Finish {
        task: String,
        attempt: u64,
        report: PathBuf,
        failed: bool,
    },
    Ack {
        task: String,
        attempt: u64,
    },
    Verify {
        task: String,
        attempt: u64,
        commits: Vec<String>,
    },
}

pub(super) fn parse(args: &[OsString]) -> Result<Invocation, CliError> {
    if matches!(
        args.first().and_then(|arg| arg.to_str()),
        Some("-h" | "--help")
    ) {
        return Ok(Invocation::Help(Some("workflow".into())));
    }
    let error = || CliError::new("invalid workflow arguments; see 'morpheus workflow --help'");
    let [verb, state, rest @ ..] = args else {
        return Err(error());
    };
    let arg = |value: &OsString| {
        value
            .to_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(error)
    };
    let attempt = |value: &OsString| {
        value
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|attempt| *attempt > 0)
            .ok_or_else(|| CliError::new("workflow attempt must be a positive integer"))
    };
    let action = match (verb.to_str(), rest) {
        (Some("init"), [plan]) => Action::Init(plan.into()),
        (Some("status"), []) => Action::Status,
        (Some("claim"), [task]) => Action::Claim(arg(task)?),
        (Some("recover"), []) => Action::Recover,
        (Some("retry"), [task]) => Action::Retry(arg(task)?),
        (Some("complete" | "fail"), [task, id, report]) => Action::Finish {
            task: arg(task)?,
            attempt: attempt(id)?,
            report: report.into(),
            failed: verb == "fail",
        },
        (Some("ack"), [task, id]) => Action::Ack {
            task: arg(task)?,
            attempt: attempt(id)?,
        },
        (Some("verify"), [task, id, commits @ ..]) if !commits.is_empty() => Action::Verify {
            task: arg(task)?,
            attempt: attempt(id)?,
            commits: commits.iter().map(arg).collect::<Result<_, _>>()?,
        },
        _ => return Err(error()),
    };
    Ok(Invocation::Workflow(Options {
        state: state.into(),
        action,
    }))
}

pub fn run(options: Options) -> i32 {
    match execute(options) {
        Ok(result) => {
            println!("{result}");
            0
        }
        Err(error) => {
            eprintln!("morpheus workflow: {error}");
            1
        }
    }
}

fn execute(Options { state, action }: Options) -> Result<Value, String> {
    if let Action::Init(path) = action {
        let file =
            fs::File::open(&path).map_err(|error| format!("open {}: {error}", path.display()))?;
        let plan = serde_json::from_reader(std::io::BufReader::new(file))
            .map_err(|error| format!("read plan {}: {error}", path.display()))?;
        let workflow = Workflow::create(&state, plan)?;
        return Ok(json!({ "ready": workflow.ready() }));
    }
    if let Action::Finish {
        task,
        attempt,
        report,
        failed,
    } = action
    {
        let report = fs::read_to_string(&report)
            .map_err(|error| format!("read report {}: {error}", report.display()))?;
        let outcome = if failed {
            Outcome::Failed { report }
        } else {
            Outcome::Succeeded { report }
        };
        let recorded = Workflow::open_wait(&state)?.complete(&task, attempt, outcome)?;
        return Ok(json!({ "recorded": recorded }));
    }
    let mut workflow = Workflow::open_wait(&state)?;
    match action {
        Action::Init(_) | Action::Finish { .. } => unreachable!(),
        Action::Status => Ok(json!({
            "ready": workflow.ready(),
            "workflow": workflow.status(),
            "pending_reports": workflow.pending_reports(),
        })),
        Action::Claim(id) => {
            let attempt = workflow.claim(&id)?;
            Ok(json!({
                "attempt": attempt, "task": workflow.spec(&id)?,
                "dependencies": workflow.dependency_results(&id)?,
            }))
        }
        Action::Recover => Ok(json!({ "recovered": workflow.recover_interrupted()? })),
        Action::Retry(id) => {
            workflow.retry_failed(&id)?;
            Ok(json!({ "retried": id }))
        }
        Action::Ack { task, attempt } => {
            Ok(json!({ "acknowledged": workflow.acknowledge(&task, attempt)? }))
        }
        Action::Verify {
            task,
            attempt,
            commits,
        } => {
            if let Some(outcome) = workflow.verified_result(&task, attempt, &commits)? {
                return Ok(verification_json(outcome));
            }
            let verification = workflow
                .spec(&task)?
                .verification
                .clone()
                .ok_or_else(|| format!("task '{task}' has no verification plan"))?;
            // Let other tasks and status readers progress during the check.
            // Recording a result revalidates the attempt.
            drop(workflow);
            let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
            record_verification(
                &state,
                &task,
                attempt,
                verify_patches(&cwd, &verification, &commits),
            )
        }
    }
}

fn record_verification(
    state: &Path,
    task: &str,
    attempt: u64,
    result: Result<PreparedIntegration, VerificationError>,
) -> Result<Value, String> {
    match result {
        Ok(evidence) => {
            let outcome = Workflow::open_wait(state)?.complete_verified(
                task,
                attempt,
                &evidence,
                "Integration check passed".into(),
            )?;
            Ok(verification_json(&outcome))
        }
        Err(
            error @ (VerificationError::Conflict(_)
            | VerificationError::CheckFailed
            | VerificationError::ChangedCandidate),
        ) => {
            let report = error.to_string();
            Workflow::open_wait(state)?.complete(
                task,
                attempt,
                Outcome::Failed {
                    report: report.clone(),
                },
            )?;
            Err(report)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn verification_json(outcome: &Outcome) -> Value {
    let Outcome::Verified {
        base,
        commit,
        reference,
        ..
    } = outcome
    else {
        unreachable!("expected a verified outcome");
    };
    json!({ "base": base, "commit": commit, "ref": reference })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_commands_persist_results_across_separate_invocations() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("run.json");
        let plan = dir.path().join("plan.json");
        let report = dir.path().join("report.txt");
        fs::write(
            &plan,
            r#"[{"id":"first","input":"Collect facts"},{"id":"next","dependencies":["first"]}]"#,
        )
        .unwrap();
        fs::write(&report, "facts collected").unwrap();
        let state = state.to_str().unwrap();
        let invoke = |args: &[&str]| {
            let Invocation::Workflow(options) = super::super::parse(
                std::iter::once(OsString::from("workflow")).chain(args.iter().map(OsString::from)),
            )
            .unwrap() else {
                panic!("expected workflow action")
            };
            execute(options).unwrap()
        };

        assert_eq!(
            invoke(&["init", state, plan.to_str().unwrap()])["ready"],
            json!(["first"])
        );
        let claimed = invoke(&["claim", state, "first"]);
        assert_eq!(claimed["attempt"], 1);
        assert_eq!(claimed["task"]["input"], "Collect facts");
        assert_eq!(
            invoke(&["complete", state, "first", "1", report.to_str().unwrap()])["recorded"],
            true
        );
        assert_eq!(invoke(&["status", state])["ready"], json!(["next"]));
        assert_eq!(
            invoke(&["claim", state, "next"])["dependencies"][0][1]["report"],
            "facts collected"
        );
        invoke(&["ack", state, "first", "1"]);
        assert_eq!(invoke(&["status", state])["pending_reports"], json!([]));
    }

    #[test]
    fn workflow_verify_requires_a_candidate_and_an_attempt() {
        let parse_args =
            |args: &[&str]| parse(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert!(parse_args(&["verify", "run.json", "patch", "1"]).is_err());
        assert!(parse_args(&["ack", "run.json", "patch", "0"]).is_err());
        let invocation = parse_args(&["verify", "run.json", "patch", "1", "hash"]).unwrap();
        let Invocation::Workflow(Options {
            action: Action::Verify { commits, .. },
            ..
        }) = invocation
        else {
            panic!("expected verify action");
        };
        assert_eq!(commits, ["hash"]);
    }

    #[test]
    fn verification_that_changes_the_candidate_records_a_retryable_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("run.json");
        let plan = dir.path().join("plan.json");
        fs::write(&plan, r#"[{"id":"patch","verification":{"base_ref":"refs/heads/main","command":["git","status"]}}]"#).unwrap();
        let create = Options {
            state: state.clone(),
            action: Action::Init(plan),
        };
        execute(create).unwrap();
        let attempt = Workflow::open(&state).unwrap().claim("patch").unwrap();
        let error = record_verification(
            &state,
            "patch",
            attempt,
            Err(VerificationError::ChangedCandidate),
        )
        .unwrap_err();
        let mut workflow = Workflow::open(&state).unwrap();
        assert_eq!(
            workflow.pending_reports(),
            [("patch", attempt, &Outcome::Failed { report: error })]
        );
        workflow.retry_failed("patch").unwrap();
        assert_eq!(workflow.ready(), ["patch"]);
    }
}
