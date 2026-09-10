# scorched-tools

Rust. Owns everything that runs on a booted ScorchedBlue machine.

`ujust` recipes orchestrate; these binaries do the work. Parsing, error handling
and state belong here, where they can be tested.

## What lives here

| Crate | Owns | State |
| --- | --- | --- |
| `scorched` | The CLI. `ujust` recipes call subcommands | skeleton |

Sequenced but not yet written: the theme engine, and the helpers the Quickshell
shell currently spawns as shell scripts per interaction -- `.desktop` parsing on
the launcher's hot path, audio levels, screen power.

Deliberately **not** here: the image's build scripts. They run inside a
Containerfile, so porting them would mean compiling a binary during the build in
order to run the build.

## Usage

```
just setup   # install pinned tooling, wire the pre-commit hook
just lint    # fmt --check, check, clippy -D warnings
just test    # needs a linker; see AGENTS.md
just ci      # everything CI runs
just dist    # the release assets; needs a linker
```

## How this ships

A `v*` tag runs the `release` workflow, which publishes a static
`x86_64-unknown-linux-musl` binary and a `SHA256SUMS` file. The image vendors
that binary by pinned hash, as it already does for starship and mise, so an
image build needs no Rust toolchain. Tag and `Cargo.toml` version must agree --
the workflow refuses to publish otherwise.

## Related repositories

- **scorchedblue** -- the image: base, packages, drivers, CI.
- **scorched-desktop** -- Hyprland configuration and the Quickshell shell.
- **scorched-planning** (private) -- decisions, specs and reasoning.
