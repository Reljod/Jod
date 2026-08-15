//! `jod team ls` lists the teams, and `jod team list` still does too.
//!
//! Every other noun in this CLI spells its listing subcommand `ls` — `jod ls`,
//! `jod goal ls`, `jod schedule ls`, `jod project ls`, `jod root ls`, and nine
//! others. `team` alone was spelled `list`, so somebody who had learned the
//! pattern typed `jod team ls` and got clap's exit code 2 and an
//! "unrecognized subcommand" message instead of the teams.
//!
//! Both spellings are asserted here, and the second assertion is the one that
//! stops this fix from becoming a worse bug than the one it fixes. `jod team
//! list` shipped in v0.2.0 and is in released binaries, so it may be sitting
//! in somebody's script or notes. Making `ls` work by taking `list` away would
//! trade a person typing the wrong word once for a script that stops working;
//! the alias costs nothing and keeps both.
//!
//! This runs the real binary rather than testing the parser, because the
//! failure was in what the program does when a word is typed at it.

use std::path::PathBuf;
use std::process::Command;

/// A fresh, empty `JOD_HOME`, so the listing has a database of its own and
/// never reads the live one.
fn scratch(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "jod-team-ls-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).expect("a scratch JOD_HOME");
    home
}

/// Run `jod team <SPELLING>` against a scratch home and hand back what it
/// printed. Fails the test if the command did not exit successfully, which is
/// what an unrecognized subcommand does.
fn list_teams(spelling: &str) -> String {
    let home = scratch(spelling);
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["team", spelling])
        .env("JOD_HOME", &home)
        .output()
        .expect("the built jod binary runs");
    assert!(
        out.status.success(),
        "jod team {spelling} exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("the listing is utf-8")
}

/// The spelling every other noun uses. On an empty database the listing says
/// so in words, which is both the proof that the command parsed and the proof
/// that it reached the arm that reads the teams table rather than some other
/// arm that happens to exit zero.
#[test]
fn team_ls_lists_the_teams() {
    assert_eq!(list_teams("ls").trim(), "no teams yet");
}

/// The spelling that shipped first. It has to keep printing the same thing,
/// because anything already written down that says `list` has to carry on
/// working.
#[test]
fn team_list_still_lists_the_teams() {
    assert_eq!(list_teams("list").trim(), "no teams yet");
}

/// `jod team --help` names one listing command, not two.
///
/// A visible alias would put `ls` and `list` on separate rows of the help,
/// which reads as two commands that might do two different things. The alias
/// is hidden instead and the canonical spelling's own text says the old word
/// still works, so there is one row and no ambiguity about which to type.
#[test]
fn the_help_names_one_listing_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["team", "--help"])
        .output()
        .expect("the built jod binary runs");
    assert!(out.status.success(), "jod team --help exited {}", out.status);
    let help = String::from_utf8(out.stdout).expect("help output is utf-8");

    let listing_rows: Vec<&str> = help
        .lines()
        .filter(|line| {
            let word = line.split_whitespace().next().unwrap_or("");
            word == "ls" || word == "list"
        })
        .collect();
    assert_eq!(
        listing_rows.len(),
        1,
        "jod team --help should show exactly one listing command, showed: {listing_rows:?}"
    );
    assert!(
        listing_rows[0].trim().starts_with("ls"),
        "the one it shows should be ls, the spelling every other noun uses: {:?}",
        listing_rows[0]
    );
}
