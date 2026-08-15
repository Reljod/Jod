//! `jod goal`'s four quiet subcommands have to explain themselves.
//!
//! `ls`, `pause`, `resume` and `rm` carried no doc comment, so `jod goal
//! --help` listed them with an empty description and `jod goal pause --help`
//! printed a usage line and nothing else. That is bad for all four, and worse
//! for `pause` and `rm`, because the thing a reader most needs to know about
//! them is the thing neither name suggests: neither one stops the iteration
//! the goal already has running. The run carries on working, unattended, and
//! carries on being billed.
//!
//! So this asserts two different things. First the weak claim from the task —
//! that each of the four prints more than a bare usage line. On its own that
//! would be satisfied by four words of filler, so it is followed by the claim
//! that actually matters: that the pause and resume text says what becomes of
//! the iteration in flight. This runs the real binary, because the failure was
//! in what the program prints.

use std::process::Command;

/// What `jod goal <SUB> --help` prints, as one string.
fn help_for(subcommand: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["goal", subcommand, "--help"])
        .output()
        .expect("the built jod binary runs");
    assert!(out.status.success(), "jod goal {subcommand} --help exited {}", out.status);
    String::from_utf8(out.stdout).expect("help output is utf-8")
}

/// The lines clap prints whatever a command says about itself: the usage line,
/// the `Arguments:`/`Options:` headings, the argument and flag rows, and the
/// blank lines between them. What is left is the command's own prose, and a
/// command with no doc comment leaves nothing.
fn prose_in(help: &str) -> Vec<&str> {
    help.lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("Usage:")
                && !line.starts_with('-')
                && !line.starts_with('<')
                && !line.ends_with(':')
        })
        .collect()
}

/// The task's own check: none of the four may be a bare usage line any more.
#[test]
fn each_quiet_goal_subcommand_says_what_it_does() {
    for subcommand in ["ls", "pause", "resume", "rm"] {
        let help = help_for(subcommand);
        let prose = prose_in(&help);
        assert!(
            !prose.is_empty(),
            "jod goal {subcommand} --help is still a bare usage line:\n{help}"
        );
        // Filler would pass the check above, so the explanation has to be long
        // enough to be an explanation. The shortest of the four is `ls`.
        let words: usize = prose.iter().map(|line| line.split_whitespace().count()).sum();
        assert!(
            words >= 15,
            "jod goal {subcommand} --help says only {words} words, which is not an \
             explanation:\n{help}"
        );
    }
}

/// The claim the whole task rests on, and the one filler cannot fake. Pausing
/// a goal does not stop the run it already started, and a reader who is
/// pausing to stop the spending has to be told so by the help itself.
#[test]
fn pause_and_resume_say_what_happens_to_the_iteration_in_flight() {
    let pause = help_for("pause");
    assert!(
        pause.contains("does not stop the iteration already in flight"),
        "jod goal pause --help does not say the in-flight run survives:\n{pause}"
    );
    assert!(
        pause.contains("billed"),
        "jod goal pause --help does not say the surviving run keeps costing money:\n{pause}"
    );
    assert!(
        pause.contains("jod kill"),
        "jod goal pause --help does not say how to stop the run it leaves:\n{pause}"
    );

    let resume = help_for("resume");
    assert!(
        resume.contains("left running when the goal was paused"),
        "jod goal resume --help does not say the paused iteration is picked up:\n{resume}"
    );
}

/// `jod goal rm` is the other command whose name promises more than it does.
/// It leaves the run going for the same reason, and it does not clear what the
/// goal learned — the facts stay in memory and `jod recall` still finds them.
#[test]
fn rm_says_what_it_leaves_behind() {
    let help = help_for("rm");
    assert!(
        help.contains("does not stop the iteration already in flight"),
        "jod goal rm --help does not say the in-flight run survives:\n{help}"
    );
    assert!(
        help.contains("jod recall"),
        "jod goal rm --help does not say the goal's facts outlive it:\n{help}"
    );
}

/// The parent listing is the other half of the bug. `jod goal --help` showed
/// `ls`, `pause`, `resume` and `rm` with the description column blank beside
/// names that did carry one, which reads as though those four do nothing worth
/// describing.
#[test]
fn the_goal_listing_has_no_blank_descriptions() {
    let out = Command::new(env!("CARGO_BIN_EXE_jod"))
        .args(["goal", "--help"])
        .output()
        .expect("the built jod binary runs");
    let help = String::from_utf8(out.stdout).expect("help output is utf-8");
    let commands = help
        .split_once("Commands:")
        .expect("jod goal --help lists its subcommands")
        .1;
    for name in ["ls", "add", "pause", "resume", "run", "rm", "log"] {
        let row = commands
            .lines()
            .map(str::trim)
            .find(|line| line.split_whitespace().next() == Some(name))
            .unwrap_or_else(|| panic!("no row for {name} in:\n{help}"));
        assert!(
            row.split_whitespace().count() > 1,
            "{name} is listed with a blank description: {row:?}"
        );
    }
}
