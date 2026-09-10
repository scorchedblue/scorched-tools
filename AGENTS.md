# scorched-tools -- working agreements

What is true now. The reasoning and decision history live in the
`scorched-planning` repository; this file states conclusions without re-arguing
them.

## Do not reopen

- **Rust owns everything that runs on a booted machine.** Not Go, not Python.
- **The image's build scripts stay bash.** They run inside a Containerfile.
  Porting them would mean compiling a binary during the build in order to run
  the build. This is a deliberate exception, not an oversight.
- **Recipes orchestrate; the binary works.** A `ujust` recipe is a few lines
  calling a subcommand.

## There is no linker on a ScorchedBlue machine

The target is an immutable Fedora Atomic desktop. It has no `cc`, no `gcc` and
no libc development stack, so `cargo test` and `cargo build` **cannot link
here**. `cargo check` and `cargo clippy` do not link and work fine, which is why
`just lint` is split from `just test`.

Do not "fix" this by layering a compiler into the image. A build toolchain is
not something that needs to work before login or as root.

## How the binaries reach the image

Settled in issue #1. These binaries ship into `/usr`, so they must not be built
against whatever glibc a developer machine happens to have. `just build`
cross-compiles to `x86_64-unknown-linux-musl`, statically linked, which removes
the glibc question rather than answering it.

The `release` workflow runs on a `v*` tag, calls `just dist`, and publishes the
binary plus a `SHA256SUMS` file. **The image vendors that published binary by
pinned hash**, the way it already vendors starship and mise -- so the image
build never grows a Rust toolchain and never waits on a Rust compile.

The rejected alternative was a builder stage in the image's Containerfile, the
Quickshell pattern. Correct by construction, but it puts a compile on the
critical path of every image build.

The cost accepted here is that a version now lives in two places, the tag and
`Cargo.toml`. The release workflow fails if they disagree; do not weaken that
check.

**`just ci` runs `dist`, and that is the point.** The release workflow was once
the only caller, which meant a release path that had stopped building would have
announced itself at tag time -- publicly, mid-release. Running it per pull
request moves that failure to review. Do not drop it from `ci` to save a
cross-compile.

The release workflow runs `just ci`, not `just dist`. A tag can be pushed at any
commit, including one that never passed, and publishing an asset built from a
tree nobody gated is the thing a release process exists to prevent. Because `ci`
ends in `dist`, proving the tree also produces the assets -- one build, not two.

## Dependencies

There are none yet, which is why no dependency auditor runs. Adding the first
dependency means adding `cargo-deny` in the same change, not later.

## Unattended sessions

Work may be picked up by an unattended agent from the issue queue. The landing
path is a pull request with auto-merge, never a push to `main`.

**Never, at any authority level:**

- cosign keys, or anything in the signing path
- publishing to GHCR
- `bootc switch` / `rpm-ostree rebase` on the running machine
- adding a dependency that fails the vetting bar
- making `scorched-planning` public
- committing when `gitleaks` fires

Anything that cannot be settled alone becomes a `needs-decision` issue rather
than a guess. Report by commenting on the issue, not by committing a report.
