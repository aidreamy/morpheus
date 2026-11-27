//! Durable task state for a local coordinator run.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::verification::PreparedIntegration;

const FORMAT_VERSION: u32 = 1;
const LOCK_DIRECTORY: &str = ".morpheus-workflow-locks";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub id: String,
    #[serde(default)]
    pub input: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub verification: Option<VerificationSpec>,
}

/// The user defines the Git target and check before the task is claimed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationSpec {
    pub base_ref: String,
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Succeeded {
        report: String,
    },
    Failed {
        report: String,
    },
    Verified {
        report: String,
        base_ref: String,
        base: String,
        commit: String,
        reference: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        commits: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Report {
    attempt: u64,
    outcome: Outcome,
    acknowledged: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Task {
    spec: TaskSpec,
    phase: Phase,
    attempt: u64,
    reports: Vec<Report>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct State {
    version: u32,
    tasks: Vec<Task>,
}

/// An exclusively opened workflow. Every successful mutation is persisted
/// before the caller receives a dispatch token or acknowledges an output.
pub struct Workflow {
    path: PathBuf,
    state: State,
    // Lock a separate inode so replacing the checkpoint never drops the lock.
    _lock: File,
}

impl Workflow {
    pub fn create(path: &Path, tasks: Vec<TaskSpec>) -> Result<Self, String> {
        validate_plan(tasks.iter())?;
        let path = new_state_path(path)?;
        let lock = lock_state(&path, false)?;
        let state = State {
            version: FORMAT_VERSION,
            tasks: tasks
                .into_iter()
                .map(|spec| Task {
                    spec,
                    phase: Phase::Pending,
                    attempt: 0,
                    reports: Vec::new(),
                })
                .collect(),
        };
        install_state(&path, stage_state(&path, &state)?, true)?;
        Ok(Self {
            path,
            state,
            _lock: lock,
        })
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        Self::open_locked(path, false)
    }

    /// Wait for an in-flight state update, such as a separate CLI invocation.
    pub fn open_wait(path: &Path) -> Result<Self, String> {
        Self::open_locked(path, true)
    }

    fn open_locked(path: &Path, wait: bool) -> Result<Self, String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("open workflow {}: {error}", path.display()))?;
        let lock = lock_state(&path, wait)?;
        let metadata = path
            .metadata()
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "workflow {} must be a regular file",
                path.display()
            ));
        }
        let file =
            File::open(&path).map_err(|error| format!("open {}: {error}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() != 1 {
                return Err(format!("workflow {} has hard links", path.display()));
            }
        }
        let state: State = serde_json::from_reader(std::io::BufReader::new(file))
            .map_err(|error| format!("read workflow {}: {error}", path.display()))?;
        if state.version != FORMAT_VERSION {
            return Err(format!("unsupported workflow format {}", state.version));
        }
        validate_plan(state.tasks.iter().map(|task| &task.spec))?;
        Ok(Self {
            path,
            state,
            _lock: lock,
        })
    }

    pub fn status(&self) -> impl Serialize + '_ {
        &self.state
    }

    pub fn spec(&self, id: &str) -> Result<&TaskSpec, String> {
        Ok(&self.state.tasks[self.index(id)?].spec)
    }

    pub fn dependency_results(&self, id: &str) -> Result<Vec<(&str, &Outcome)>, String> {
        let mut seen = HashSet::new();
        Ok(self
            .spec(id)?
            .dependencies
            .iter()
            .filter(|dependency| seen.insert(dependency.as_str()))
            .filter_map(|dependency| {
                let task = &self.state.tasks[self.index(dependency).ok()?];
                if task.phase != Phase::Succeeded {
                    return None;
                }
                Some((task.spec.id.as_str(), &task.reports.last()?.outcome))
            })
            .collect())
    }

    pub fn ready(&self) -> Vec<&str> {
        let succeeded: HashSet<_> = self
            .state
            .tasks
            .iter()
            .filter(|task| task.phase == Phase::Succeeded)
            .map(|task| task.spec.id.as_str())
            .collect();
        self.state
            .tasks
            .iter()
            .filter(|task| {
                task.phase == Phase::Pending
                    && task
                        .spec
                        .dependencies
                        .iter()
                        .all(|id| succeeded.contains(id.as_str()))
            })
            .map(|task| task.spec.id.as_str())
            .collect()
    }

    pub fn claim(&mut self, id: &str) -> Result<u64, String> {
        let index = self.index(id)?;
        if !self.ready().contains(&id) {
            return Err(format!("task '{id}' is not ready"));
        }
        let mut next = self.state.clone();
        let task = &mut next.tasks[index];
        task.attempt = task
            .attempt
            .checked_add(1)
            .ok_or("task attempt counter exhausted")?;
        task.phase = Phase::Running;
        let attempt = task.attempt;
        self.save(next, None)?;
        Ok(attempt)
    }

    pub fn running(&self, id: &str, attempt: u64) -> Result<(), String> {
        let task = &self.state.tasks[self.index(id)?];
        if task.phase == Phase::Running && task.attempt == attempt {
            Ok(())
        } else {
            Err(format!("task '{id}' has no running attempt {attempt}"))
        }
    }

    pub fn complete(&mut self, id: &str, attempt: u64, outcome: Outcome) -> Result<bool, String> {
        if matches!(outcome, Outcome::Verified { .. }) {
            return Err("verified results must come from the integration gate".into());
        }
        self.record(id, attempt, outcome, None)
    }

    pub fn verified_result(
        &self,
        id: &str,
        attempt: u64,
        submitted_commits: &[String],
    ) -> Result<Option<&Outcome>, String> {
        let task = &self.state.tasks[self.index(id)?];
        if let Some(Report {
            outcome: outcome @ Outcome::Verified { commits, .. },
            ..
        }) = task.reports.iter().find(|report| report.attempt == attempt)
        {
            if commits.len() == submitted_commits.len()
                && commits
                    .iter()
                    .zip(submitted_commits)
                    .all(|(saved, supplied)| saved.eq_ignore_ascii_case(supplied))
            {
                return Ok(Some(outcome));
            }
            return Err(format!(
                "task '{id}' already verified a different submission for attempt {attempt}"
            ));
        }
        self.running(id, attempt)?;
        Ok(None)
    }

    pub fn complete_verified(
        &mut self,
        id: &str,
        attempt: u64,
        evidence: &PreparedIntegration,
        report: String,
    ) -> Result<Outcome, String> {
        let expected = self
            .spec(id)?
            .verification
            .as_ref()
            .ok_or_else(|| format!("task '{id}' has no verification plan"))?;
        if expected != evidence.spec() {
            return Err(format!(
                "verification does not match the plan for task '{id}'"
            ));
        }
        if let Some(outcome) = self.verified_result(id, attempt, evidence.commits())? {
            return Ok(outcome.clone());
        }
        let outcome = evidence.outcome(report);
        self.record(id, attempt, outcome.clone(), Some(evidence))?;
        Ok(outcome)
    }

    fn record(
        &mut self,
        id: &str,
        attempt: u64,
        outcome: Outcome,
        evidence: Option<&PreparedIntegration>,
    ) -> Result<bool, String> {
        let index = self.index(id)?;
        let task = &self.state.tasks[index];
        if let Some(report) = task.reports.iter().find(|report| report.attempt == attempt) {
            return if report.outcome == outcome {
                Ok(false)
            } else {
                Err(format!(
                    "task '{id}' already recorded a different result for attempt {attempt}"
                ))
            };
        }
        self.running(id, attempt)?;
        if task.spec.verification.is_some() && matches!(outcome, Outcome::Succeeded { .. }) {
            return Err(format!("task '{id}' requires a verified patch"));
        }
        let mut next = self.state.clone();
        let task = &mut next.tasks[index];
        task.phase = if matches!(outcome, Outcome::Failed { .. }) {
            Phase::Failed
        } else {
            Phase::Succeeded
        };
        task.reports.push(Report {
            attempt,
            outcome,
            acknowledged: false,
        });
        self.save(next, evidence)?;
        Ok(true)
    }

    /// The caller declares that the old workers are gone. Their attempt
    /// numbers remain reserved so late replies cannot complete a retry.
    pub fn recover_interrupted(&mut self) -> Result<Vec<String>, String> {
        let mut next = self.state.clone();
        let recovered: Vec<_> = next
            .tasks
            .iter_mut()
            .filter(|task| task.phase == Phase::Running)
            .map(|task| {
                task.phase = Phase::Pending;
                task.spec.id.clone()
            })
            .collect();
        if !recovered.is_empty() {
            self.save(next, None)?;
        }
        Ok(recovered)
    }

    pub fn retry_failed(&mut self, id: &str) -> Result<(), String> {
        let index = self.index(id)?;
        if self.state.tasks[index].phase != Phase::Failed {
            return Err(format!("task '{id}' has no failed attempt to retry"));
        }
        let mut next = self.state.clone();
        next.tasks[index].phase = Phase::Pending;
        self.save(next, None)
    }

    pub fn pending_reports(&self) -> Vec<(&str, u64, &Outcome)> {
        self.state
            .tasks
            .iter()
            .flat_map(|task| {
                task.reports
                    .iter()
                    .filter(|report| !report.acknowledged)
                    .map(|report| (task.spec.id.as_str(), report.attempt, &report.outcome))
            })
            .collect()
    }

    pub fn acknowledge(&mut self, id: &str, attempt: u64) -> Result<bool, String> {
        let index = self.index(id)?;
        let mut next = self.state.clone();
        let report = next.tasks[index]
            .reports
            .iter_mut()
            .find(|report| report.attempt == attempt)
            .ok_or_else(|| format!("task '{id}' has no report for attempt {attempt}"))?;
        if report.acknowledged {
            return Ok(false);
        }
        report.acknowledged = true;
        self.save(next, None)?;
        Ok(true)
    }

    fn index(&self, id: &str) -> Result<usize, String> {
        self.state
            .tasks
            .iter()
            .position(|task| task.spec.id == id)
            .ok_or_else(|| format!("unknown workflow task '{id}'"))
    }

    fn save(&mut self, next: State, evidence: Option<&PreparedIntegration>) -> Result<(), String> {
        let checkpoint = stage_state(&self.path, &next)?;
        if let Some(evidence) = evidence {
            // Stage and sync the complete checkpoint before publishing its ref.
            // The workflow lock covers both publication and installation.
            evidence.publish().map_err(|error| error.to_string())?;
        }
        install_state(&self.path, checkpoint, false)?;
        self.state = next;
        Ok(())
    }
}

fn validate_plan<'a>(tasks: impl Iterator<Item = &'a TaskSpec>) -> Result<(), String> {
    let tasks: Vec<_> = tasks.collect();
    if tasks.is_empty() {
        return Err("the workflow must have at least one task".into());
    }
    let mut by_id = HashMap::new();
    for (index, task) in tasks.iter().enumerate() {
        if task.id.trim().is_empty() || task.id.trim() != task.id {
            return Err(format!("invalid workflow task id '{}'", task.id));
        }
        if by_id.insert(task.id.as_str(), index).is_some() {
            return Err(format!("duplicate workflow task '{}'", task.id));
        }
        if let Some(verification) = &task.verification
            && (!verification.base_ref.starts_with("refs/heads/")
                || verification
                    .command
                    .first()
                    .is_none_or(|program| program.trim().is_empty()))
        {
            return Err(format!(
                "task '{}' requires a full base branch and a verification command",
                task.id
            ));
        }
    }
    let mut children = vec![Vec::new(); tasks.len()];
    let mut remaining = vec![0; tasks.len()];
    let mut ready = VecDeque::new();
    for (index, task) in tasks.iter().enumerate() {
        let deps: HashSet<_> = task.dependencies.iter().collect();
        if deps.is_empty() {
            ready.push_back(index);
        }
        remaining[index] = deps.len();
        for dependency in deps {
            let parent = by_id.get(dependency.as_str()).ok_or_else(|| {
                format!("task '{}' depends on unknown task '{dependency}'", task.id)
            })?;
            children[*parent].push(index);
        }
    }
    let mut visited = 0;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for &child in &children[index] {
            remaining[child] -= 1;
            if remaining[child] == 0 {
                ready.push_back(child);
            }
        }
    }
    if visited != tasks.len() {
        return Err("workflow dependencies contain a cycle".into());
    }
    Ok(())
}

fn new_state_path(path: &Path) -> Result<PathBuf, String> {
    let filename = path.file_name().ok_or("workflow path must name a file")?;
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = directory
        .canonicalize()
        .map_err(|error| format!("open workflow directory {}: {error}", directory.display()))?;
    Ok(directory.join(filename))
}

fn lock_state(path: &Path, wait: bool) -> Result<File, String> {
    if path
        .components()
        .any(|part| part.as_os_str() == LOCK_DIRECTORY)
    {
        return Err(format!("workflow paths reserve {LOCK_DIRECTORY}"));
    }
    let directory = path.parent().ok_or("workflow path must name a file")?;
    let filename = path.file_name().ok_or("workflow path must name a file")?;
    let locks = directory.join(LOCK_DIRECTORY);
    std::fs::create_dir_all(&locks)
        .map_err(|error| format!("create workflow lock directory: {error}"))?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(locks.join(filename))
        .map_err(|error| format!("open workflow lock: {error}"))?;
    let error =
        |error: &dyn std::fmt::Display| format!("lock workflow {}: {error}", path.display());
    if wait {
        lock.lock().map_err(|err| error(&err))?;
    } else {
        lock.try_lock().map_err(|err| error(&err))?;
    }
    Ok(lock)
}

fn stage_state(path: &Path, state: &State) -> Result<tempfile::NamedTempFile, String> {
    let directory = path.parent().expect("canonical workflow parent");
    let mut file = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("create workflow checkpoint: {error}"))?;
    serde_json::to_writer(file.as_file_mut(), state)
        .map_err(|error| format!("encode workflow checkpoint: {error}"))?;
    file.as_file()
        .sync_all()
        .map_err(|error| format!("sync workflow checkpoint: {error}"))?;
    Ok(file)
}

fn install_state(path: &Path, file: tempfile::NamedTempFile, create: bool) -> Result<(), String> {
    let installed = if create {
        file.persist_noclobber(path)
    } else {
        file.persist(path)
    };
    installed.map_err(|error| format!("write workflow {}: {error}", path.display()))?;
    #[cfg(unix)]
    if let Err(error) =
        File::open(path.parent().expect("canonical workflow parent")).and_then(|dir| dir.sync_all())
    {
        // The rename is already visible. Keep memory consistent with disk.
        log::warn!("sync workflow directory for {}: {error}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
