# scorched-tools -- task runner.
#
# Everything CI runs lives in the `ci` recipe, and CI calls that recipe, so the
# two cannot drift.

default:
    @just --list

# Install pinned tooling.
setup:
    mise install
    git config core.hooksPath .githooks

fmt:
    cargo fmt

# `check` and `clippy` type-check without linking, which is why they are split
# from `test`. A ScorchedBlue machine is an immutable Atomic desktop with no
# `cc` and no libc development stack, so nothing here can *link* locally --
# `just test` therefore runs in CI and on a machine with a toolchain. See
# AGENTS.md; how these binaries get built for shipping is a P3 decision.
lint:
    cargo fmt --check
    cargo check --all-targets
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

# Full-history secret scan. The pre-commit hook runs `protect --staged`, which
# only ever sees one commit; this is what catches anything already landed.
secrets:
    gitleaks detect --no-banner --redact

clean:
    cargo clean

ci: lint secrets test
