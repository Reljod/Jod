//! `jod upgrade` — take the newest release of the binaries running on this box,
//! downloaded rather than compiled.
//!
//! The sibling of [`crate::update`], and the difference between them is the
//! reason both exist. `jod update` rebuilds from the checkout `install.sh` left
//! behind: it needs git and a Rust toolchain, and it only ever moves within the
//! installed MAJOR.MINOR. `jod upgrade` downloads `jod-<target>.tar.gz` from a
//! GitHub release — the artifact the Release workflow already built from the
//! tag — checks it against the `.sha256` published beside it, and renames the
//! binaries into place. It needs curl and tar, and it moves to the newest
//! release whatever its major and minor.
//!
//! That covers the case `jod update` structurally cannot. The README's first
//! install path is the prebuilt tarball, advertised as needing no Rust
//! toolchain; a box installed that way has no checkout, so `jod update` there
//! fails on the missing `install.sh` and there is no way at all to take a new
//! release short of reinstalling by hand.
//!
//! As with `update`, the work is in a shell script rather than reimplemented
//! here — `bin/jod-upgrade.sh`. This module answers only what the script
//! cannot: where the binaries currently live, which platform this build is
//! actually for, and what version it is. The script is *embedded* rather than
//! read from a checkout, because the box that most needs it is precisely the
//! one with no copy of the repo on disk.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use tokio::sync::mpsc::UnboundedSender;

pub use crate::update::Outcome;
use crate::update::{file_identity, running_binary, stream};

/// The upgrader, and the semver helpers it shares with `install.sh`. Compiled
/// in so that `jod upgrade` works on a box holding nothing but the binaries —
/// which is the whole population this command is for.
const UPGRADE_SH: &str = include_str!("../../bin/jod-upgrade.sh");
const SEMVER_SH: &str = include_str!("../../bin/lib/semver.sh");

/// The platform triple this binary was compiled for, stamped by `build.rs`.
///
/// Not `uname`: that reports the kernel's architecture, which is not always
/// the one the running binary was built for — an x86_64 build under Rosetta on
/// an Apple Silicon Mac would otherwise be told to fetch an aarch64 tarball it
/// cannot run.
const TARGET: &str = env!("JOD_BUILD_TARGET");

/// Download and install a release. `check` reports and changes nothing;
/// `version` names a release instead of taking the newest; `force` reinstalls
/// even when already on the target.
pub fn run(check: bool, version: Option<String>, force: bool) -> Result<Outcome> {
    execute(check, version, force, None)
}

/// The same, with every line the script writes handed to `lines` as it
/// arrives — for the console, which cannot give up its terminal to a
/// subprocess. Blocking; run it on a thread that is allowed to block.
pub fn run_streaming(
    check: bool,
    version: Option<String>,
    force: bool,
    lines: UnboundedSender<String>,
) -> Result<Outcome> {
    execute(check, version, force, Some(lines))
}

fn execute(
    check: bool,
    version: Option<String>,
    force: bool,
    lines: Option<UnboundedSender<String>>,
) -> Result<Outcome> {
    let staged = StagedScript::write().context("unpacking the bundled upgrade script")?;

    let exe = running_binary()?;
    let bin_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", exe.display()))?
        .to_path_buf();
    let before = file_identity(&exe);

    let mut cmd = Command::new("bash");
    cmd.arg(staged.script());
    if check {
        cmd.arg("--check");
    }
    if force {
        cmd.arg("--force");
    }
    // Where *this* binary is, not a default: an upgrade has to replace the
    // binaries the box is actually running, whether that is ~/.local/bin or
    // the /usr/local/bin a systemd unit points at.
    cmd.env("JOD_BIN_DIR", &bin_dir)
        .env("JOD_TARGET", TARGET)
        // What this build claims to be. The release workflow stamps it into
        // Cargo.toml on the tag, so a released binary reports its own release
        // — which is what makes "already up to date" answerable with no
        // checkout and no state file to go stale.
        .env("JOD_CURRENT_VERSION", env!("CARGO_PKG_VERSION"))
        .env("JOD_UPGRADE_VERSION", version.as_deref().unwrap_or("latest"));
    // An upgrade must not quietly drop a binary this machine has. jod-api is
    // opt-in at install time precisely because it is an endpoint that spawns
    // agents — but a box that already made that choice keeps it.
    if bin_dir.join("jod-api").exists() {
        cmd.env("JOD_WITH_API", "1");
    }

    let status = match lines {
        None => cmd.status().context("running the upgrade script")?,
        Some(tx) => stream(cmd, tx).context("running the upgrade script")?,
    };
    if !status.success() {
        let how = status
            .code()
            .map_or_else(|| "a signal".to_string(), |c| c.to_string());
        bail!("upgrade failed — the upgrade script exited with {how}");
    }

    Ok(Outcome {
        replaced: !check && file_identity(&exe) != before,
    })
}

/// The bundled script written to a private directory, removed when it drops.
///
/// `bin/lib/semver.sh` is laid out beside it exactly as it sits in the repo,
/// so the script's own `source .../lib/semver.sh` line works unchanged whether
/// it runs from here or straight out of a checkout. One copy of the version
/// rules, one code path, and nothing to keep in step.
struct StagedScript {
    dir: PathBuf,
}

impl StagedScript {
    fn write() -> Result<Self> {
        let dir = Self::private_dir()?;
        let lib = dir.join("lib");
        std::fs::create_dir(&lib).with_context(|| format!("creating {}", lib.display()))?;

        let script = dir.join("jod-upgrade.sh");
        std::fs::write(&script, UPGRADE_SH)
            .with_context(|| format!("writing {}", script.display()))?;
        std::fs::write(lib.join("semver.sh"), SEMVER_SH)
            .with_context(|| format!("writing {}", lib.join("semver.sh").display()))?;

        Ok(Self { dir })
    }

    /// A directory only this user can reach, created rather than reused.
    ///
    /// `create_dir` fails if the path exists, and mkdir is atomic — so this
    /// cannot be handed a directory (or a symlink to one) that somebody else
    /// put in a shared `/tmp` first, which for a file about to be executed as
    /// a shell script is the difference between a temp file and a foothold.
    fn private_dir() -> Result<PathBuf> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("jod-upgrade-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("restricting {}", dir.display()))?;
        }
        Ok(dir)
    }

    fn script(&self) -> PathBuf {
        self.dir.join("jod-upgrade.sh")
    }
}

impl Drop for StagedScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary has to know which tarball to ask for. A build that could not
    /// tell would send every box to the same asset.
    #[test]
    fn the_build_names_the_platform_it_is_for() {
        assert!(TARGET.contains('-'), "not a target triple: {TARGET}");
        assert!(
            TARGET.contains("linux") || TARGET.contains("darwin") || TARGET.contains("windows"),
            "unrecognised target triple: {TARGET}"
        );
    }

    /// The point of embedding: the script and the version rules it sources
    /// both have to be *in* the binary, or `jod upgrade` fails on exactly the
    /// checkout-less box it exists to serve.
    #[test]
    fn the_upgrade_script_is_compiled_in_with_its_semver_helpers() {
        assert!(UPGRADE_SH.contains("jod-upgrade.sh"), "wrong script bundled");
        assert!(
            UPGRADE_SH.contains("lib/semver.sh"),
            "the script no longer sources the shared version rules"
        );
        assert!(
            SEMVER_SH.contains("highest_semver_tag"),
            "wrong semver library bundled"
        );
    }

    /// The staged layout is what makes the script's `source` line work
    /// unchanged here and in a checkout — so it is asserted, not assumed.
    #[test]
    fn staging_reproduces_the_repository_layout() {
        let staged = StagedScript::write().expect("staging the bundled script");
        assert!(staged.script().is_file(), "no script at {:?}", staged.script());
        assert!(
            staged.dir.join("lib/semver.sh").is_file(),
            "semver.sh is not beside the script where its source line looks for it"
        );
        let dir = staged.dir.clone();
        drop(staged);
        assert!(!dir.exists(), "the staging directory outlived the upgrade");
    }

    /// Written where only this user can read it: it is executed as a shell
    /// script, so a world-writable staging directory would be a way to run
    /// code as whoever typed `jod upgrade`.
    #[cfg(unix)]
    #[test]
    fn the_staging_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let staged = StagedScript::write().expect("staging the bundled script");
        let mode = std::fs::metadata(&staged.dir)
            .expect("the staging directory exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "group/other can reach {:?}", staged.dir);
    }
}

// What the script itself does — version resolution, checksum refusal, the
// rename over a running binary — is asserted end to end against a file://
// release fixture in `tests/upgrade.test.sh`, where it can be exercised
// without a network and without env vars that every other test in this
// process would race.
