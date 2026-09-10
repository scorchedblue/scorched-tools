# scorched-tools -- task runner.
#
# Everything CI runs lives in the `ci` recipe, and CI calls that recipe, so the
# two cannot drift.

# What the image installs. Static musl: these binaries land in /usr on an image
# whose glibc is not this machine's, and a static build has no glibc question at
# all. See AGENTS.md.
musl_target := "x86_64-unknown-linux-musl"

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
# `just test`, `just build` and `just dist` therefore run in CI or a container,
# not on a booted machine. See AGENTS.md.
lint:
    cargo fmt --check
    cargo check --all-targets
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

# `rustup target add` is idempotent, and mise exports RUSTUP_TOOLCHAIN, so the
# target lands on the pinned toolchain rather than on whatever rustup defaults
# to.
# Cross-compile the static binary the image ships. Needs a linker.
build:
    rustup target add {{ musl_target }}
    cargo build --release --target {{ musl_target }}

# The binary the image downloads and the checksum it pins, exactly as the
# release workflow uploads them.
# Build the release assets. Run this to reproduce a published hash.
dist: build
    rm -rf dist
    mkdir dist
    cp target/{{ musl_target }}/release/scorched dist/scorched-{{ musl_target }}
    cd dist && sha256sum scorched-{{ musl_target }} > SHA256SUMS

# Full-history secret scan. The pre-commit hook runs `protect --staged`, which
# only ever sees one commit; this is what catches anything already landed.
secrets:
    gitleaks detect --no-banner --redact

clean:
    cargo clean
    rm -rf dist

# `dist` is in here deliberately. The release workflow is the only thing that
# ever ran it, so a release path that had stopped building would have failed at
# tag time -- publicly, mid-release, with nothing else going on to explain it.
# Running it per pull request moves that failure to review, where it is cheap.
#
# It costs one cross-compile. The crate has no dependencies, so that is seconds;
# if it ever stops being seconds, that is worth knowing too.
ci: lint secrets test dist
