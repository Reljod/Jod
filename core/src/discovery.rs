//! Finding CLI binaries from inside a GUI app.
//!
//! A Tauri app launched from Finder inherits a minimal `PATH` — no nvm, no
//! Homebrew, no `~/.opencode/bin`. So we look in three places, in order:
//! an explicit env override, the inherited `PATH`, then a list of well-known
//! install locations (globbed, because nvm buries binaries under a version).

use std::path::{Path, PathBuf};

/// Resolve a binary to an absolute path.
///
/// `env_override` wins outright, so the user can always point Jod at a specific
/// install. Returns `None` when nothing executable was found.
pub fn find_binary(env_override: &str, names: &[&str], well_known: &[&str]) -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(env_override) {
        let p = PathBuf::from(shellexpand_home(&explicit));
        if is_executable(&p) {
            return Some(p);
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                let candidate = dir.join(name);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }

    for pattern in well_known {
        if let Some(found) = expand_single_star(&shellexpand_home(pattern)) {
            return Some(found);
        }
    }

    None
}

/// Expand a leading `~` using `$HOME`. We deliberately do not support the full
/// shell expansion grammar — these are our own hard-coded patterns.
fn shellexpand_home(input: &str) -> String {
    match input.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => input.to_string(),
        },
        None => input.to_string(),
    }
}

/// Resolve a path containing at most one `*` segment, e.g.
/// `~/.nvm/versions/node/*/bin/claude`. Picks the highest-sorting match so a
/// machine with several Node versions lands on the newest.
fn expand_single_star(pattern: &str) -> Option<PathBuf> {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        let p = PathBuf::from(pattern);
        return is_executable(&p).then_some(p);
    };

    let prefix_dir = Path::new(prefix.trim_end_matches('/'));
    let suffix = suffix.trim_start_matches('/');

    let mut matches: Vec<PathBuf> = std::fs::read_dir(prefix_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    matches.sort();

    matches
        .into_iter()
        .rev()
        .map(|dir| dir.join(suffix))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_expansion_only_touches_a_leading_tilde() {
        let _guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("HOME").ok();
        std::env::set_var("HOME", "/home/x");
        assert_eq!(shellexpand_home("~/a/b"), "/home/x/a/b");
        assert_eq!(shellexpand_home("/abs/~/b"), "/abs/~/b");
        assert_eq!(shellexpand_home("plain"), "plain");
        match saved {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn a_starless_pattern_resolves_only_if_executable() {
        assert!(expand_single_star("/definitely/not/here").is_none());
        // `/bin/sh` exists and is executable on every unix CI box.
        #[cfg(unix)]
        assert_eq!(
            expand_single_star("/bin/sh"),
            Some(PathBuf::from("/bin/sh"))
        );
    }

    #[test]
    fn missing_binary_yields_none_rather_than_panicking() {
        assert!(find_binary(
            "JOD_TEST_NO_SUCH_OVERRIDE",
            &["jod-nonexistent-binary"],
            &["/definitely/not/here/*/bin/nope"],
        )
        .is_none());
    }
}
