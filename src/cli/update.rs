//! `morpheus update` - install the latest (or a pinned) release.
//!
//! The work is delegated to the release installer (`download.sh` /
//! `download.ps1`), exactly as a manual install would: it knows the archive
//! layout, the DLLs and the uninstaller that ships next to the binary, and it
//! is versioned WITH the release it installs. What this module adds is the
//! standard command line around it - version resolution, confirmation and
//! `MORPHEUS_VERSION`/`MORPHEUS_VARIANT` pinned to the running build.
//!
//! The installer is downloaded into a private temp file and executed directly
//! instead of being piped through a shell: no tag or path is ever
//! interpolated into a command line. The installer is started detached and
//! this process exits right away: on Windows the running `morpheus.exe` image is
//! locked, so it could not be replaced while we were still alive. The user
//! reopens morpheus once the install finishes.
//!
//! Exit status therefore means "the installer was started", not "the update
//! landed": the installer reports its own outcome on the inherited terminal
//! after morpheus has exited, and its exit code is not observable from here. Only
//! a failure to fetch or start the installer is reported as an error (status
//! 1); a rejected version is a usage error (status 2).

use std::io::{IsTerminal, Write};
use std::process::Command;

use morpheus::update::{REPO, fetch_latest_tag, is_update_available, normalize_tag};

use crate::cli::VERSION;
use crate::cli::style::Palette;

/// Options of `morpheus update`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateOptions {
    /// Release to install (`v0.2.3` or `0.2.3`). `None` targets the latest
    /// published release.
    pub target: Option<String>,
    /// `--yes`: skip the confirmation prompt.
    pub assume_yes: bool,
}

/// Run the command and return the process exit code.
pub async fn run(options: UpdateOptions) -> i32 {
    let palette = Palette::load();

    let target = match resolve_target(&options, &palette).await {
        Ok(Some(tag)) => tag,
        // "already up to date" was reported; nothing to do.
        Ok(None) => return 0,
        Err(code) => return code,
    };

    match confirm(&palette, &target, options.assume_yes) {
        Confirmation::Accepted => {}
        Confirmation::Declined => {
            println!("{}", palette.muted("Cancelled."));
            return 1;
        }
        Confirmation::RequiresYes => {
            eprintln!(
                "{}",
                palette.error(
                    "refusing to update without confirmation - pass --yes for a non-interactive run"
                )
            );
            return 2;
        }
    }

    println!(
        "{}",
        palette.body(&format!(
            "Updating morpheus v{VERSION} → {target} ({} variant)",
            variant()
        ))
    );

    if let Err(error) = install(&target).await {
        eprintln!("{}", palette.error(&error));
        return 1;
    }

    println!(
        "{}",
        palette.success(&format!(
            "Update started - morpheus is exiting; reopen it to use {target}."
        ))
    );
    0
}

/// Resolve the release tag to install. `Ok(None)` = already up to date.
async fn resolve_target(options: &UpdateOptions, palette: &Palette) -> Result<Option<String>, i32> {
    if let Some(raw) = &options.target {
        return match normalize_tag(raw) {
            Some(tag) => Ok(Some(tag)),
            None => {
                eprintln!(
                    "{}",
                    palette.error(&format!(
                        "invalid version '{raw}' - expected a release like v0.2.3"
                    ))
                );
                Err(2)
            }
        };
    }

    println!("{}", palette.muted("Checking the latest release…"));
    let Some(latest) = fetch_latest_tag().await else {
        eprintln!(
            "{}",
            palette.error(
                "could not reach the GitHub releases API - pass a version explicitly, e.g. `morpheus update v0.2.3`"
            )
        );
        return Err(1);
    };
    if !is_update_available(VERSION, &latest.tag) {
        println!(
            "{}",
            palette.success(&format!(
                "morpheus v{VERSION} is already up to date (latest: {})",
                latest.tag
            ))
        );
        return Ok(None);
    }
    Ok(Some(latest.tag))
}

enum Confirmation {
    Accepted,
    Declined,
    RequiresYes,
}

fn confirm(palette: &Palette, target: &str, assume_yes: bool) -> Confirmation {
    if assume_yes {
        return Confirmation::Accepted;
    }
    if !std::io::stdin().is_terminal() {
        return Confirmation::RequiresYes;
    }

    println!(
        "{}",
        palette.body(&format!(
            "This replaces the running morpheus (v{VERSION}) with {target}."
        ))
    );
    print!("{} ", palette.body("Continue? [y/N]"));
    let _ = std::io::stdout().flush();

    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Confirmation::Declined;
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Confirmation::Accepted,
        _ => Confirmation::Declined,
    }
}

/// Download the pinned installer, start it with inherited stdio (so its
/// progress and errors land in the user's terminal) and return.
///
async fn install(target: &str) -> Result<(), String> {
    let script_name = if cfg!(windows) {
        "download.ps1"
    } else {
        "download.sh"
    };
    let url = format!("https://raw.githubusercontent.com/{REPO}/{target}/{script_name}");
    let response = reqwest::get(&url)
        .await
        .map_err(|error| format!("could not download the installer ({url}): {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "could not download the installer ({url}): HTTP {}",
            response.status()
        ));
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("could not read the installer ({url}): {error}"))?;

    // Process-unique name: a predictable shared path would let a local
    // process swap the script between the download and its execution.
    let extension = if cfg!(windows) { "ps1" } else { "sh" };
    let script = std::env::temp_dir().join(format!(
        "morpheus-update-{}.{extension}",
        std::process::id()
    ));
    // The copy cannot be deleted from here: the installer is still reading it
    // while we exit. Ask the script to remove itself as its last statement
    // (`$PSCommandPath`/`$0` are this same temp path), so repeated updates do
    // not leave one file per run behind in the temp directory.
    let mut script_body = body.to_vec();
    script_body.extend_from_slice(if cfg!(windows) {
        b"\r\nRemove-Item -LiteralPath $PSCommandPath -Force -ErrorAction SilentlyContinue\r\n"
    } else {
        b"\nrm -f -- \"$0\"\n"
    });
    std::fs::write(&script, &script_body)
        .map_err(|error| format!("could not write {}: {error}", script.display()))?;

    let mut command = if cfg!(windows) {
        let mut command = Command::new("powershell");
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File");
        command
    } else {
        Command::new("bash")
    };
    command
        .arg(&script)
        .env("MORPHEUS_VERSION", target)
        .env("MORPHEUS_VARIANT", variant());

    // An in-app update spawns `morpheus update` without a console; a console
    // program started from a console-less process (powershell) would get a
    // fresh console window allocated for it, sitting empty on screen for the
    // whole download. Suppress it when there is no terminal to report into.
    #[cfg(windows)]
    if !std::io::stdout().is_terminal() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    // Spawn the installer and return immediately WITHOUT waiting for it.
    //
    // On Windows the running morpheus.exe image is locked by the OS: if we
    // stayed alive while the installer ran, it could not replace the binary
    // (and would fail with an access-denied error). By exiting here first,
    // the installer finds the image unlocked and can replace it freely, even
    // with older installers that delete the file outright. The user reopens
    // morpheus once the update finishes.
    match command.spawn() {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("could not run the installer: {error}")),
    }
}

/// The release variant this binary was built from - the installers default to
/// `slim`, which would silently drop local RAG from an `embed` build.
fn variant() -> &'static str {
    if cfg!(feature = "embed") {
        "slim-embed"
    } else {
        "slim"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variant_matches_the_build() {
        // A slim-embed release must keep installing slim-embed: the installer
        // defaults to slim, which would silently drop local RAG.
        #[cfg(feature = "embed")]
        assert_eq!(variant(), "slim-embed");
        #[cfg(not(feature = "embed"))]
        assert_eq!(variant(), "slim");
    }
}
