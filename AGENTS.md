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

**Unsettled, and a P3 decision:** these binaries ship into `/usr`, so they must
be built against the image's glibc rather than against whatever a developer
machine has. Building in a container stage -- the pattern the image's
Containerfile already uses for Quickshell -- is the likely answer, but it has
not been decided. Do not pick one silently.

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
