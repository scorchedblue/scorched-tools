//! Greeter launch for the `greeter` subcommand.
//!
//! Ported from `files/usr/libexec/scorched-greeter` in `scorchedblue/scorchedblue`.
//! That script existed so that greetd's `command` -- documented only as
//! "the command-line", with no stated splitting rule -- is a single path with
//! no arguments: the greeter is the one process where getting the split wrong
//! means nobody can log in. Two of the arguments below contain spaces (the
//! time format and the greeting), which is exactly what a naive split would
//! shatter.
//!
//! Colours are ANSI names because that is all `tuigreet --theme` accepts;
//! they cannot be the shell's hex values. These are the nearest equivalents
//! to the bar's palette: blue for the accent, cyan for the clock, grey for
//! de-emphasised hints. Components and colour names are documented in
//! `/usr/share/doc/tuigreet/README.md`.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const TUIGREET: &str = "/usr/bin/tuigreet";

const THEME: &str = "border=blue;title=lightblue;greet=lightblue;text=white;time=cyan;prompt=white;input=lightcyan;action=gray;button=lightblue;container=black";

/// Builds the exact program and argv that launch the greeter, so the command
/// line that will run can be proven correct -- byte for byte -- without a
/// working greeter, a login screen, or root.
///
/// `--asterisks`: without it a typed password gives no feedback whatsoever,
/// which reads as a frozen greeter -- the biggest usability difference on
/// this screen. `--time-format` matches the bar's clock, so the same moment
/// is written the same way before and after login. `--sessions` reads the
/// list from what is actually installed rather than hardcoding a command, so
/// a new session file is picked up for free.
#[must_use]
pub fn tuigreet_command() -> (&'static str, Vec<String>) {
    (
        TUIGREET,
        vec![
            "--remember".to_string(),
            "--remember-session".to_string(),
            "--asterisks".to_string(),
            "--time".to_string(),
            "--time-format".to_string(),
            "%a %-d %b   %I:%M %p".to_string(),
            "--greeting".to_string(),
            "ScorchedBlue".to_string(),
            "--window-padding".to_string(),
            "2".to_string(),
            "--theme".to_string(),
            THEME.to_string(),
            "--sessions".to_string(),
            "/usr/share/wayland-sessions".to_string(),
        ],
    )
}

/// Replaces this process with `program`/`args`, mirroring the original
/// script's `exec`. `exec` is injected so the wiring from subcommand to
/// syscall can be proven in a test: on success this never returns, so the
/// only observable outcome in production is either a running greeter or the
/// error path below.
fn launch(exec: impl FnOnce(&str, &[String]) -> io::Error) -> ExitCode {
    let (program, args) = tuigreet_command();
    let err = exec(program, &args);
    eprintln!("scorched: failed to exec {program}: {err}");
    ExitCode::FAILURE
}

#[must_use]
pub fn run() -> ExitCode {
    launch(|program, args| Command::new(program).args(args).exec())
}

#[cfg(test)]
mod tests {
    use super::{launch, tuigreet_command};
    use std::io;
    use std::process::ExitCode;

    #[test]
    fn argv_matches_the_original_script_exactly() {
        let (program, args) = tuigreet_command();
        assert_eq!(program, "/usr/bin/tuigreet");
        assert_eq!(
            args,
            vec![
                "--remember",
                "--remember-session",
                "--asterisks",
                "--time",
                "--time-format",
                "%a %-d %b   %I:%M %p",
                "--greeting",
                "ScorchedBlue",
                "--window-padding",
                "2",
                "--theme",
                "border=blue;title=lightblue;greet=lightblue;text=white;time=cyan;\
                 prompt=white;input=lightcyan;action=gray;button=lightblue;container=black",
                "--sessions",
                "/usr/share/wayland-sessions",
            ]
        );
    }

    #[test]
    fn launch_execs_the_greeter_with_that_exact_argv() {
        let mut seen = None;
        let code = launch(|program, args| {
            seen = Some((program.to_string(), args.to_vec()));
            io::Error::other("exec is not available in a test")
        });
        let (program, args) = tuigreet_command();
        assert_eq!(seen, Some((program.to_string(), args)));
        assert_eq!(code, ExitCode::FAILURE);
    }
}
