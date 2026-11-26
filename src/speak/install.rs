//! Downloading a Kokoro bundle into the local model cache.
//!
//! The release page only ships `.tar.bz2` archives, one per published bundle.
//! [`download`] streams the archive to a temporary file beside the cache,
//! extracts it and verifies the resulting directory with
//! [`crate::speak::is_complete`], so a broken transfer never looks installed.
//! The bundle directory name is what the engine maps to a voice table, which
//! is why the archive is unpacked under its own name.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use bzip2::read::BzDecoder;

use crate::speak::voice;

/// One published bundle the router can fetch.
pub struct Bundle {
    /// Archive stem and the installed directory name.
    pub dir: &'static str,
    pub label: &'static str,
    /// Speakers the published table contains.
    pub voices: u16,
    /// Languages the bundle ships voices for, in the router's row copy.
    pub languages: &'static str,
}

/// The bundles the download router offers, recommended first.
pub const BUNDLES: &[Bundle] = &[
    Bundle {
        dir: "kokoro-multi-lang-v1_1",
        label: "Kokoro v1.1",
        voices: 103,
        languages: "English and Chinese",
    },
    Bundle {
        dir: "kokoro-multi-lang-v1_0",
        label: "Kokoro v1.0",
        voices: 54,
        languages: "every supported language, including Portuguese and Japanese",
    },
];

impl Bundle {
    /// The release asset for this bundle.
    #[must_use]
    pub fn url(&self) -> String {
        format!(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/{}.tar.bz2",
            self.dir
        )
    }

    /// Whether the bundle is already unpacked and complete under `models`.
    #[must_use]
    pub fn is_installed(&self, models: &Path) -> bool {
        voice::is_complete(&models.join(self.dir))
    }
}

/// Fetch `bundle` into `models` and return the installed directory.
///
/// `progress` receives `(downloaded_bytes, total_bytes)` while the archive
/// streams in; `total` is 0 when the server sends no content length. The
/// archive only becomes visible under its final name after extraction, and a
/// failed unpack removes whatever it wrote, so an interrupted run never leaves
/// a directory that [`Bundle::is_installed`] would accept.
pub fn download(
    bundle: &Bundle,
    models: &Path,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(models)
        .map_err(|e| format!("could not create the model cache: {e}"))?;
    let archive = models.join(format!(".{}.tar.bz2.part", bundle.dir));
    let dest = models.join(bundle.dir);
    let result = fetch(bundle, &archive, &mut progress).and_then(|()| {
        // A previous half-extracted directory is replaced, never merged with.
        let _ = std::fs::remove_dir_all(&dest);
        unpack(&archive, models).and_then(|()| {
            if voice::is_complete(&dest) {
                Ok(())
            } else {
                Err("the downloaded archive is missing the bundle files".to_string())
            }
        })
    });
    let _ = std::fs::remove_file(&archive);
    if let Err(reason) = result {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(reason);
    }
    Ok(dest)
}

/// Stream the release asset into `archive`, reporting progress about once per
/// mebibyte instead of on every socket read.
fn fetch(
    bundle: &Bundle,
    archive: &Path,
    progress: &mut impl FnMut(u64, u64),
) -> Result<(), String> {
    let mut response = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("could not create the HTTP client: {e}"))?
        .get(bundle.url())
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| format!("could not download {}: {e}", bundle.label))?;
    let total = response.content_length().unwrap_or(0);
    let mut file =
        File::create(archive).map_err(|e| format!("could not write the download: {e}"))?;
    let mut buffer = [0u8; 64 * 1024];
    let mut done = 0u64;
    let mut reported = 0u64;
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|e| format!("could not read the download: {e}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|e| format!("could not write the download: {e}"))?;
        done += read as u64;
        if done - reported >= 1024 * 1024 {
            reported = done;
            progress(done, total);
        }
    }
    file.flush()
        .map_err(|e| format!("could not write the download: {e}"))?;
    progress(done, total);
    Ok(())
}

/// Unpack a bundle archive into `root`, refusing entries that would land
/// outside it.
fn unpack(archive: &Path, root: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(|e| format!("could not open the archive: {e}"))?;
    let mut tar = tar::Archive::new(BzDecoder::new(file));
    for entry in tar
        .entries()
        .map_err(|e| format!("could not read the archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("could not read the archive: {e}"))?;
        let unpacked = entry
            .unpack_in(root)
            .map_err(|e| format!("could not extract the archive: {e}"))?;
        if !unpacked {
            return Err("the archive contains a path outside the model cache".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle directory with the files the loader requires.
    fn bundle_tree(root: &Path, name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.int8.onnx"), b"graph").unwrap();
        std::fs::write(dir.join("tokens.txt"), b"tokens").unwrap();
        std::fs::write(dir.join("voices.bin"), b"voices").unwrap();
        std::fs::write(dir.join("lexicon-us-en.txt"), b"lexicon").unwrap();
    }

    #[test]
    fn bundles_download_from_the_release_page() {
        for bundle in BUNDLES {
            assert_eq!(
                bundle.url(),
                format!(
                    "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/{}.tar.bz2",
                    bundle.dir
                )
            );
        }
    }

    #[test]
    fn unpack_extracts_a_complete_bundle() {
        let work = tempfile::tempdir().unwrap();
        let source = work.path().join("source");
        bundle_tree(&source, BUNDLES[1].dir);
        let archive = work.path().join("bundle.tar.bz2");
        let encoder = bzip2::write::BzEncoder::new(
            File::create(&archive).unwrap(),
            bzip2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(encoder);
        tar.append_dir_all(BUNDLES[1].dir, source.join(BUNDLES[1].dir))
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let models = work.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        unpack(&archive, &models).unwrap();
        assert!(BUNDLES[1].is_installed(&models));
        assert!(!BUNDLES[0].is_installed(&models));
    }

    #[test]
    fn unpack_refuses_entries_outside_the_cache() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("escape.tar.bz2");
        let encoder = bzip2::write::BzEncoder::new(
            File::create(&archive).unwrap(),
            bzip2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        let raw = b"../outside.txt\0";
        header.as_old_mut().name[..raw.len()].copy_from_slice(raw);
        header.set_size(1);
        header.set_cksum();
        tar.append(&header, &b"x"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();

        let models = work.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        assert!(unpack(&archive, &models).is_err());
        assert!(!work.path().join("outside.txt").exists());
    }
}
