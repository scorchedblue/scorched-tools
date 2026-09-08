//! `scorched` -- the command-line entry point.
//!
//! Recipes orchestrate; this binary works. A `ujust` recipe stays a few lines
//! calling a subcommand, so that parsing, error handling and state live here
//! where they can be tested.

use std::process::ExitCode;

mod apps;
mod audio_levels;
mod brew_setup;
mod greeter;
mod performance;
mod screen_power;

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Prints the launcher's `.desktop` entries as JSON, the `apps` subcommand.
fn run_apps() {
    let dirs = apps::default_data_dirs();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let entries = apps::scan(&dirs, &path_var);
    println!("{}", apps::to_json(&entries));
}

fn main() -> ExitCode {
    let mut args = std::env::args();
    match args.nth(1).as_deref() {
        None => {
            println!("scorched {}", version());
            ExitCode::SUCCESS
        }
        Some("apps") => {
            run_apps();
            ExitCode::SUCCESS
        }
        Some("audio-levels") => audio_levels::run(args.next().as_deref()),
        Some("screen-power") => screen_power::run(args.next().as_deref()),
        Some("greeter") => greeter::run(),
        Some("performance") => performance::run(),
        Some("brew-setup") => brew_setup::run(),
        Some(other) => {
            eprintln!("scorched: unknown subcommand '{other}'");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
