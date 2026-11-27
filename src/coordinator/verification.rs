//! Verify an exact batch of Git commits in an isolated integration checkout.

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use super::workflow::{Outcome, VerificationSpec};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum VerificationError {
    #[error("git failed: {0}")]
    Git(String),
    #[error("patches conflict in {0:?}")]
    Conflict(Vec<String>),
    #[error("checks failed on the integrated candidate")]
    CheckFailed,
    #[error("the base branch moved during verification; verify against its new tip")]
    StaleBase,
    #[error("the check changed the candidate commit or tracked files")]
    ChangedCandidate,
    #[error("{0}")]
    Io(String),
}

#[derive(Debug)]
pub struct PreparedIntegration {
    checkout: IntegrationCheckout,
    spec: VerificationSpec,
    commits: Vec<String>,
    base: String,
    commit: String,
}

impl PreparedIntegration {
    pub(super) fn spec(&self) -> &VerificationSpec {
        &self.spec
    }

    pub(super) fn commits(&self) -> &[String] {
        &self.commits
    }

    pub(super) fn publish(&self) -> Result<(), VerificationError> {
        pin_candidate(
            &self.checkout.repo,
            &self.spec.base_ref,
            &self.base,
            &self.reference(),
            &self.commit,
        )
    }

    pub(super) fn outcome(&self, report: String) -> Outcome {
        Outcome::Verified {
            report,
            base_ref: self.spec.base_ref.clone(),
            base: self.base.clone(),
            commit: self.commit.clone(),
            reference: self.reference(),
            commits: self.commits.clone(),
        }
    }

    fn reference(&self) -> String {
        format!("refs/morpheus/verified/{}", self.commit)
    }
}

/// Run the planned command on the exact integrated candidate. The coordinator
/// pins the candidate after revalidating the attempt and the current base tip.
pub fn verify_patches(
    repo: &Path,
    spec: &VerificationSpec,
    commits: &[String],
) -> Result<PreparedIntegration, VerificationError> {
    let [program, args @ ..] = spec.command.as_slice() else {
        return Err(VerificationError::Io("empty verification command".into()));
    };
    prepare_integration(repo, spec, commits, |checkout| {
        Ok(check_command(program.as_ref(), checkout)
            .args(args)
            .stdin(Stdio::null())
            .stdout(io::stderr())
            .status()?
            .success())
    })
}

fn prepare_integration(
    repo: &Path,
    spec: &VerificationSpec,
    commits: &[String],
    check: impl FnOnce(&Path) -> io::Result<bool>,
) -> Result<PreparedIntegration, VerificationError> {
    let repo = repo.canonicalize().map_err(io_error)?;
    let base_ref = &spec.base_ref;
    if !base_ref.starts_with("refs/heads/") {
        return Err(VerificationError::Git(
            "base must be a full refs/heads/ branch name".into(),
        ));
    }
    check_git(&repo, &["check-ref-format", base_ref])?;
    if commits.is_empty()
        || commits.iter().any(|commit| {
            !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    {
        return Err(VerificationError::Git(
            "provide at least one full commit object ID".into(),
        ));
    }
    let resolved = commits
        .iter()
        .map(|commit| {
            let resolved = revision(&repo, commit)?;
            if !resolved.eq_ignore_ascii_case(commit) {
                return Err(VerificationError::Git(format!(
                    "'{commit}' is not a full commit object ID"
                )));
            }
            Ok(resolved)
        })
        .collect::<Result<Vec<_>, VerificationError>>()?;
    let base = revision(&repo, base_ref)?;
    let checkout = IntegrationCheckout::new(repo, &base)?;
    for commit in &resolved {
        let output = git(&checkout.path)
            .args(["merge", "--no-ff", "--no-edit", "--", commit])
            .output()
            .map_err(io_error)?;
        if !output.status.success() {
            let conflicts = check_git(
                &checkout.path,
                &["diff", "--name-only", "--diff-filter=U", "-z"],
            )?;
            let paths: Vec<_> = conflicts
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .collect();
            return Err(if paths.is_empty() {
                git_error(&output)
            } else {
                VerificationError::Conflict(paths)
            });
        }
    }
    let commit = revision(&checkout.path, "HEAD")?;
    if !check(&checkout.path).map_err(io_error)? {
        return Err(VerificationError::CheckFailed);
    }
    if revision(&checkout.path, "HEAD")? != commit || !tracked_files_match(&checkout, &commit)? {
        return Err(VerificationError::ChangedCandidate);
    }
    Ok(PreparedIntegration {
        checkout,
        spec: spec.clone(),
        commits: resolved,
        base,
        commit,
    })
}

fn tracked_files_match(
    checkout: &IntegrationCheckout,
    commit: &str,
) -> Result<bool, VerificationError> {
    let status_args = [
        "status",
        "--porcelain",
        "--untracked-files=no",
        "--ignore-submodules=none",
    ];
    if !check_git(&checkout.path, &status_args)?.is_empty() {
        return Ok(false);
    }
    // Compare tracked files against a fresh index as well. The check could have
    // set skip-worktree or assume-unchanged in its own index.
    let index = checkout.temp.path().join("comparison-index");
    for args in [&["read-tree", commit][..], &status_args] {
        let output = git(&checkout.path)
            .env("GIT_INDEX_FILE", &index)
            .args(args)
            .output()
            .map_err(io_error)?;
        if !output.status.success() {
            return Err(git_error(&output));
        }
        if !output.stdout.is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn pin_candidate(
    repo: &Path,
    base_ref: &str,
    base: &str,
    reference: &str,
    commit: &str,
) -> Result<(), VerificationError> {
    // Git locks the base and receipt refs in the same transaction. A separate
    // rev-parse followed by update-ref would leave a race here.
    let mut child = git(repo)
        .args(["update-ref", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_error)?;
    let write_result = {
        let mut stdin = child.stdin.take().expect("piped git stdin");
        write!(
            stdin,
            "verify {base_ref} {base}\noption no-deref\nupdate {reference} {commit}\n"
        )
    };
    let output = child.wait_with_output().map_err(io_error)?;
    if !output.status.success() {
        return match revision(repo, base_ref) {
            Ok(current) if current == base => Err(git_error(&output)),
            _ => Err(VerificationError::StaleBase),
        };
    }
    write_result.map_err(io_error)
}

#[derive(Debug)]
struct IntegrationCheckout {
    repo: PathBuf,
    path: PathBuf,
    temp: tempfile::TempDir,
}

impl IntegrationCheckout {
    fn new(repo: PathBuf, base: &str) -> Result<Self, VerificationError> {
        let temp = tempfile::Builder::new()
            .prefix("morpheus-verify-")
            .tempdir()
            .map_err(io_error)?;
        let path = temp.path().join("checkout");
        let output = git(&repo)
            .args(["worktree", "add", "--detach", "--"])
            .arg(&path)
            .arg(base)
            .output()
            .map_err(io_error)?;
        if !output.status.success() {
            return Err(git_error(&output));
        }
        Ok(Self { repo, path, temp })
    }
}

impl Drop for IntegrationCheckout {
    fn drop(&mut self) {
        match git(&self.repo)
            .args(["worktree", "remove", "--force", "--"])
            .arg(&self.path)
            .output()
        {
            Ok(output) if output.status.success() => {}
            Ok(output) => log::warn!("clean up integration worktree: {}", git_error(&output)),
            Err(error) => log::warn!("clean up integration worktree: {error}"),
        }
    }
}

const REPO_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
];

fn check_command(program: &std::ffi::OsStr, checkout: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(checkout);
    for variable in REPO_ENV {
        command.env_remove(variable);
    }
    command.env("GIT_NO_REPLACE_OBJECTS", "1");
    command
}

fn git(repo: &Path) -> Command {
    let mut command = check_command("git".as_ref(), repo);
    #[cfg(unix)]
    command.args(["-c", "core.fileMode=true"]);
    command.args([
        "-c",
        "core.hooksPath=",
        "-c",
        "commit.gpgSign=false",
        "-c",
        "merge.gpgSign=false",
        "-c",
        "core.sparseCheckout=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "user.name=Morpheus",
        "-c",
        "user.email=morpheus@localhost",
    ]);
    command
}

fn revision(repo: &Path, revision: &str) -> Result<String, VerificationError> {
    Ok(check_git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ],
    )?
    .trim()
    .to_owned())
}

fn check_git(repo: &Path, args: &[&str]) -> Result<String, VerificationError> {
    let output = git(repo).args(args).output().map_err(io_error)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(git_error(&output))
    }
}

fn git_error(output: &Output) -> VerificationError {
    VerificationError::Git(String::from_utf8_lossy(&output.stderr).trim().to_owned())
}

fn io_error(error: io::Error) -> VerificationError {
    VerificationError::Io(error.to_string())
}

#[cfg(test)]
mod tests;
