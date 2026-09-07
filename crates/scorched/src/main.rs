//! `scorched` -- the command-line entry point.
//!
//! Recipes orchestrate; this binary works. A `ujust` recipe stays a few lines
//! calling a subcommand, so that parsing, error handling and state live here
//! where they can be tested.
//!
//! The subcommands themselves arrive with P3. This is deliberately a skeleton:
//! the repository exists now so that CI, the ruleset and the issue queue are
//! in place before there is code worth guarding.

fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn main() {
    println!("scorched {}", version());
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
