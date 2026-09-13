//! `theme set <name>` -- writes the theme state and fans the palette out to
//! every target, for the `theme` subcommand.
//!
//! # The targets are not six of a kind
//!
//! Five targets take the whole palette, live: Quickshell (QML file watching,
//! free), starship (re-reads per prompt, free), Hyprland (`hyprctl reload`),
//! tmux (`tmux source-file`) and Ghostty (`SIGUSR2`).
//!
//! GTK/Qt cannot. GTK does not watch `~/.config/gtk-4.0/gtk.css` (open
//! upstream request, GNOME/gtk#3409), and neither `qt6ct` nor Kvantum is in
//! the image. The one live channel is the Settings portal, which carries
//! `color-scheme` and `accent-color` and nothing richer. So GTK/Qt is a
//! *projection* target: it gets a scheme bit and one accent colour, live, and
//! the rest only at the next login. [`Target::mode`] states this up front so
//! the report can never claim a full palette landed where only a projection
//! did.
//!
//! # Dispatch success, not adoption success
//!
//! `hyprctl reload`, `tmux source-file`, a `SIGUSR2` and a `gsettings set` are
//! all fire-and-forget: each reports whether the command ran, not whether the
//! target actually repainted. This module reports the former. The
//! alternative -- polling each target to confirm it adopted the palette --
//! has no observable channel for any of these targets, so there is nothing
//! honest to poll.
//!
//! A target with nothing to reload (no running Ghostty surface, say) is
//! [`Outcome::Skipped`], not [`Outcome::Failed`]: skipping is not a desktop
//! that half-changed, it is a target that was not part of the session.
//! [`Outcome::Failed`] is reserved for a dispatch that was attempted and did
//! not go through, and that is the only thing that fails the exit code.
//!
//! Ghostty's `changeDefault` preserves palette entries a program set for
//! itself via OSC 4 until that program resets (OSC 104) or exits. A stale TUI
//! after a theme change is that program's own choice, not a partial failure
//! of this fan-out.

use crate::palette::{self, Color, Palette, Target as RenderTarget};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const SCORCHED_DARK: &str = include_str!("../palettes/scorched-dark.toml");
const SCORCHED_LIGHT: &str = include_str!("../palettes/scorched-light.toml");

/// The curated palettes this binary ships. `theme set <name>` only ever
/// resolves against these -- there is no installed-palette directory
/// convention yet, and this crate is deliberately dependency-free, so
/// embedding the two curated schemes keeps `set` self-contained.
fn builtin_sources() -> [&'static str; 2] {
    [SCORCHED_DARK, SCORCHED_LIGHT]
}

/// Parses a curated palette. The curated files are covered by
/// `palette::tests` and always parse; this exists only so `set` can share
/// that assumption without repeating the `expect` message at each call site.
fn parse_builtin(source: &str) -> Palette {
    palette::parse(source).expect("curated palettes always parse; see palette::tests")
}

fn builtin_palette(name: &str) -> Option<Palette> {
    builtin_sources()
        .into_iter()
        .map(parse_builtin)
        .find(|p| p.slug == name || p.name == name)
}

fn builtin_slugs() -> Vec<String> {
    builtin_sources()
        .into_iter()
        .map(|s| parse_builtin(s).slug)
        .collect()
}

/// What a target can actually do with a palette, stated up front so the
/// report never claims more than what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Takes every colour in the palette, live.
    FullPalette,
    /// Takes only a scheme bit and one accent colour, live.
    Projection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Quickshell,
    Starship,
    Hyprland,
    Tmux,
    Ghostty,
    GtkQt,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Self::Quickshell => "quickshell",
            Self::Starship => "starship",
            Self::Hyprland => "hyprland",
            Self::Tmux => "tmux",
            Self::Ghostty => "ghostty",
            Self::GtkQt => "gtk/qt",
        }
    }

    fn mode(self) -> Mode {
        match self {
            Self::GtkQt => Mode::Projection,
            _ => Mode::FullPalette,
        }
    }
}

/// The result of trying to reload one target.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// The reload was dispatched, or the target needed no dispatch at all.
    Reloaded,
    /// There was nothing to reload. Visible in the report, but not a
    /// failure: this is not a desktop that half-changed.
    Skipped(String),
    /// A dispatch was attempted and did not go through. The only outcome
    /// that fails the exit code.
    Failed(String),
}

/// Whether `background` reads as dark enough for `prefer-dark`, using
/// Rec. 601 luma -- more than accurate enough for a scheme bit.
fn is_dark(background: &Color) -> bool {
    let (red, green, blue) = background.rgb;
    let luma = 0.299 * f64::from(red) + 0.587 * f64::from(green) + 0.114 * f64::from(blue);
    luma / 255.0 < 0.5
}

fn color_scheme(background: &Color) -> &'static str {
    if is_dark(background) {
        "prefer-dark"
    } else {
        "prefer-light"
    }
}

/// Which channel is the largest, decided on the original integers so the hue
/// computation never has to compare floats for equality.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Largest {
    Red,
    Green,
    Blue,
}

fn largest_channel(red: u8, green: u8, blue: u8) -> Largest {
    if red >= green && red >= blue {
        Largest::Red
    } else if green >= blue {
        Largest::Green
    } else {
        Largest::Blue
    }
}

/// `(hue in 0..360, saturation in 0..1, lightness in 0..1)`.
fn to_hsl((red, green, blue): (u8, u8, u8)) -> (f64, f64, f64) {
    let largest = largest_channel(red, green, blue);

    let red = f64::from(red) / 255.0;
    let green = f64::from(green) / 255.0;
    let blue = f64::from(blue) / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let lightness = f64::midpoint(max, min);
    let delta = max - min;
    if delta == 0.0 {
        return (0.0, 0.0, lightness);
    }
    let saturation = if lightness < 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };
    let raw_hue = match largest {
        Largest::Red => 60.0 * (((green - blue) / delta) % 6.0),
        Largest::Green => 60.0 * (((blue - red) / delta) + 2.0),
        Largest::Blue => 60.0 * (((red - green) / delta) + 4.0),
    };
    let hue = if raw_hue < 0.0 {
        raw_hue + 360.0
    } else {
        raw_hue
    };
    (hue, saturation, lightness)
}

fn hue_distance(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 360.0;
    d.min(360.0 - d)
}

/// GNOME's `accent-color` enum (Settings, Appearance, since GNOME 46) has
/// nine members. This buckets by hue rather than matching each member's
/// swatch hex, because the enum names are the stable public contract and the
/// swatch hex is a libadwaita implementation detail that has moved between
/// releases; a hue bucket degrades gracefully for a palette accent that
/// matches no swatch exactly, which any curated or custom palette's accent
/// will.
const ACCENT_HUES: [(&str, f64); 8] = [
    ("red", 0.0),
    ("orange", 30.0),
    ("yellow", 60.0),
    ("green", 130.0),
    ("teal", 180.0),
    ("blue", 220.0),
    ("purple", 275.0),
    ("pink", 320.0),
];

/// Below this saturation a colour reads as grey rather than any particular
/// hue, so it maps to `slate`, the desaturated member of the accent enum.
const SLATE_SATURATION: f64 = 0.15;

fn accent_name(accent: &Color) -> &'static str {
    let (hue, saturation, _lightness) = to_hsl(accent.rgb);
    if saturation < SLATE_SATURATION {
        return "slate";
    }
    ACCENT_HUES
        .iter()
        .min_by(|a, b| hue_distance(hue, a.1).total_cmp(&hue_distance(hue, b.1)))
        .map(|&(name, _)| name)
        .expect("ACCENT_HUES is non-empty")
}

/// `$XDG_STATE_HOME`, falling back to `~/.local/state` per the XDG Base
/// Directory spec.
fn xdg_state_home(home: &Path, env: Option<&str>) -> PathBuf {
    match env {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".local/state"),
    }
}

/// `$XDG_CONFIG_HOME`, falling back to `~/.config`.
fn xdg_config_home(home: &Path, env: Option<&str>) -> PathBuf {
    match env {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".config"),
    }
}

/// tmux checks `$XDG_CONFIG_HOME/tmux/tmux.conf` before `~/.tmux.conf`; this
/// mirrors that order so `source-file` reloads whichever file tmux itself
/// would have loaded.
fn tmux_conf_path(home: &Path, config_home: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let xdg_path = config_home.join("tmux/tmux.conf");
    if exists(&xdg_path) {
        return xdg_path;
    }
    home.join(".tmux.conf")
}

/// Writes the resolved palette to the theme's state directory: the slug that
/// is now current, and a rendering per consumer format so Quickshell (CSS
/// custom properties) and shell tooling (`SCORCHED_*` env vars) have
/// something to pick up -- their reload is free precisely because it is
/// triggered by these files changing, not by a command this module runs.
fn write_state(state_dir: &Path, palette: &Palette) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    fs::write(state_dir.join("theme"), format!("{}\n", palette.slug))?;
    fs::write(
        state_dir.join("theme.css"),
        palette::render(palette, RenderTarget::Css),
    )?;
    fs::write(
        state_dir.join("theme.env"),
        palette::render(palette, RenderTarget::Env),
    )?;
    Ok(())
}

/// PIDs of every running process whose `/proc/<pid>/comm` is exactly `name`.
/// Reading `/proc` directly avoids depending on `pgrep`, which the image does
/// not guarantee.
fn pids_named(name: &str) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| {
            fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|comm| comm.trim() == name)
        })
        .collect()
}

fn dispatch_hyprland(run: &mut impl FnMut(&str, &[&str]) -> io::Result<bool>) -> Outcome {
    match run("hyprctl", &["reload"]) {
        Ok(true) => Outcome::Reloaded,
        Ok(false) => Outcome::Failed("hyprctl reload exited with a non-zero status".to_string()),
        Err(err) => Outcome::Failed(format!("failed to run hyprctl: {err}")),
    }
}

fn dispatch_tmux(path: &Path, run: &mut impl FnMut(&str, &[&str]) -> io::Result<bool>) -> Outcome {
    let path = path.to_string_lossy();
    match run("tmux", &["source-file", &path]) {
        Ok(true) => Outcome::Reloaded,
        Ok(false) => Outcome::Failed(format!(
            "tmux source-file {path} exited with a non-zero status"
        )),
        Err(err) => Outcome::Failed(format!("failed to run tmux: {err}")),
    }
}

fn dispatch_ghostty(
    pids: &[u32],
    run: &mut impl FnMut(&str, &[&str]) -> io::Result<bool>,
) -> Outcome {
    if pids.is_empty() {
        return Outcome::Skipped("no running Ghostty process to signal".to_string());
    }
    for &pid in pids {
        match run("kill", &["-USR2", &pid.to_string()]) {
            Ok(true) => {}
            Ok(false) => {
                return Outcome::Failed(format!("kill -USR2 {pid} exited with a non-zero status"));
            }
            Err(err) => return Outcome::Failed(format!("failed to signal ghostty ({pid}): {err}")),
        }
    }
    Outcome::Reloaded
}

fn dispatch_gtk_qt(
    palette: &Palette,
    run: &mut impl FnMut(&str, &[&str]) -> io::Result<bool>,
) -> Outcome {
    let Some(background) = palette.colors.get("background") else {
        return Outcome::Failed(
            "palette has no \"background\" colour to derive a scheme from".to_string(),
        );
    };
    let Some(accent) = palette.colors.get("accent") else {
        return Outcome::Failed("palette has no \"accent\" colour to project".to_string());
    };

    let scheme = color_scheme(background);
    match run(
        "gsettings",
        &["set", "org.gnome.desktop.interface", "color-scheme", scheme],
    ) {
        Ok(true) => {}
        Ok(false) => {
            return Outcome::Failed(format!("gsettings failed to set color-scheme to {scheme}"));
        }
        Err(err) => return Outcome::Failed(format!("failed to run gsettings: {err}")),
    }

    let accent = accent_name(accent);
    match run(
        "gsettings",
        &["set", "org.gnome.desktop.interface", "accent-color", accent],
    ) {
        Ok(true) => Outcome::Reloaded,
        Ok(false) => Outcome::Failed(format!("gsettings failed to set accent-color to {accent}")),
        Err(err) => Outcome::Failed(format!("failed to run gsettings: {err}")),
    }
}

fn dispatch_all(
    palette: &Palette,
    tmux_conf: &Path,
    ghostty_pids: &[u32],
    mut run: impl FnMut(&str, &[&str]) -> io::Result<bool>,
) -> Vec<(Target, Outcome)> {
    vec![
        (Target::Quickshell, Outcome::Reloaded),
        (Target::Starship, Outcome::Reloaded),
        (Target::Hyprland, dispatch_hyprland(&mut run)),
        (Target::Tmux, dispatch_tmux(tmux_conf, &mut run)),
        (Target::Ghostty, dispatch_ghostty(ghostty_pids, &mut run)),
        (Target::GtkQt, dispatch_gtk_qt(palette, &mut run)),
    ]
}

fn format_report(palette: &Palette, report: &[(Target, Outcome)]) -> String {
    let mut out = format!("theme set to {} ({})\n", palette.name, palette.slug);
    for (target, outcome) in report {
        let mode = match target.mode() {
            Mode::FullPalette => "full palette",
            Mode::Projection => "projection",
        };
        let status = match outcome {
            Outcome::Reloaded => "reloaded".to_string(),
            Outcome::Skipped(reason) => format!("skipped: {reason}"),
            Outcome::Failed(reason) => format!("failed: {reason}"),
        };
        writeln!(out, "  {:<10} {mode:<13} {status}", target.label())
            .expect("writing to a String cannot fail");
    }
    out
}

fn run_set(name: Option<&str>) -> ExitCode {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        eprintln!("usage: scorched theme set <name>");
        return ExitCode::from(2);
    };

    let Some(palette) = builtin_palette(name) else {
        eprintln!(
            "scorched: unknown theme {name:?} (known: {})",
            builtin_slugs().join(", ")
        );
        return ExitCode::FAILURE;
    };

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("scorched: HOME is not set");
        return ExitCode::FAILURE;
    };

    let state_dir =
        xdg_state_home(&home, std::env::var("XDG_STATE_HOME").ok().as_deref()).join("scorched");
    if let Err(err) = write_state(&state_dir, &palette) {
        eprintln!(
            "scorched: failed to write theme state to {}: {err}",
            state_dir.display()
        );
        return ExitCode::FAILURE;
    }

    let config_home = xdg_config_home(&home, std::env::var("XDG_CONFIG_HOME").ok().as_deref());
    let tmux_conf = tmux_conf_path(&home, &config_home, Path::exists);
    let ghostty_pids = pids_named("ghostty");

    let report = dispatch_all(&palette, &tmux_conf, &ghostty_pids, |program, args| {
        Command::new(program)
            .args(args)
            .status()
            .map(|s| s.success())
    });

    print!("{}", format_report(&palette, &report));

    let failed = report
        .iter()
        .filter(|(_, outcome)| matches!(outcome, Outcome::Failed(_)))
        .count();
    if failed > 0 {
        eprintln!(
            "scorched: {failed} of {} targets failed to reload",
            report.len()
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Runs the `theme` subcommand: `set <name>` is the only action.
#[must_use]
pub fn run(action: Option<&str>, name: Option<&str>) -> ExitCode {
    if action == Some("set") {
        return run_set(name);
    }
    eprintln!("usage: scorched theme set <name>");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::{
        Mode, Outcome, Target, accent_name, builtin_palette, color_scheme, dispatch_all,
        dispatch_ghostty, dispatch_gtk_qt, dispatch_hyprland, dispatch_tmux, format_report,
        is_dark, tmux_conf_path, to_hsl, xdg_config_home, xdg_state_home,
    };
    use crate::palette::Color;
    use std::path::{Path, PathBuf};

    #[test]
    fn gtk_qt_is_the_only_projection_target() {
        for target in [
            Target::Quickshell,
            Target::Starship,
            Target::Hyprland,
            Target::Tmux,
            Target::Ghostty,
        ] {
            assert_eq!(target.mode(), Mode::FullPalette);
        }
        assert_eq!(Target::GtkQt.mode(), Mode::Projection);
    }

    #[test]
    fn builtin_palettes_resolve_by_slug_or_name() {
        assert!(builtin_palette("scorched-dark").is_some());
        assert!(builtin_palette("Scorched Light").is_some());
        assert!(builtin_palette("no-such-theme").is_none());
    }

    #[test]
    fn a_dark_background_reads_as_dark() {
        let bg = Color::from_hex("#11151cff").unwrap();
        assert!(is_dark(&bg));
        assert_eq!(color_scheme(&bg), "prefer-dark");
    }

    #[test]
    fn a_light_background_reads_as_light() {
        let bg = Color::from_hex("#f4f6fbff").unwrap();
        assert!(!is_dark(&bg));
        assert_eq!(color_scheme(&bg), "prefer-light");
    }

    #[test]
    fn pure_red_hue_is_zero_saturation_is_full() {
        let (h, s, l) = to_hsl((0xff, 0, 0));
        assert!((h - 0.0).abs() < 0.01);
        assert!((s - 1.0).abs() < 0.01);
        assert!((l - 0.5).abs() < 0.01);
    }

    #[test]
    fn grey_has_zero_saturation() {
        let (_, s, _) = to_hsl((0x80, 0x80, 0x80));
        assert!(s.abs() < f64::EPSILON);
    }

    #[test]
    fn a_desaturated_accent_maps_to_slate() {
        let accent = Color::from_hex("#888888ff").unwrap();
        assert_eq!(accent_name(&accent), "slate");
    }

    #[test]
    fn the_scorched_accents_are_blue() {
        let dark_accent = Color::from_hex("#5aa9e6ff").unwrap();
        let light_accent = Color::from_hex("#1f6fb2ff").unwrap();
        assert_eq!(accent_name(&dark_accent), "blue");
        assert_eq!(accent_name(&light_accent), "blue");
    }

    #[test]
    fn a_pure_green_accent_maps_to_green() {
        let accent = Color::from_hex("#00ff00ff").unwrap();
        assert_eq!(accent_name(&accent), "green");
    }

    #[test]
    fn xdg_state_home_prefers_the_environment_override() {
        let home = PathBuf::from("/home/napalm");
        assert_eq!(
            xdg_state_home(&home, Some("/custom/state")),
            PathBuf::from("/custom/state")
        );
        assert_eq!(
            xdg_state_home(&home, None),
            PathBuf::from("/home/napalm/.local/state")
        );
        assert_eq!(
            xdg_state_home(&home, Some("")),
            PathBuf::from("/home/napalm/.local/state")
        );
    }

    #[test]
    fn xdg_config_home_prefers_the_environment_override() {
        let home = PathBuf::from("/home/napalm");
        assert_eq!(
            xdg_config_home(&home, Some("/custom/config")),
            PathBuf::from("/custom/config")
        );
        assert_eq!(
            xdg_config_home(&home, None),
            PathBuf::from("/home/napalm/.config")
        );
    }

    #[test]
    fn tmux_conf_prefers_the_xdg_path_when_it_exists() {
        let home = Path::new("/home/napalm");
        let config_home = Path::new("/home/napalm/.config");
        let path = tmux_conf_path(home, config_home, |p| {
            p == Path::new("/home/napalm/.config/tmux/tmux.conf")
        });
        assert_eq!(path, PathBuf::from("/home/napalm/.config/tmux/tmux.conf"));
    }

    #[test]
    fn tmux_conf_falls_back_to_the_dotfile_when_no_xdg_conf_exists() {
        let home = Path::new("/home/napalm");
        let config_home = Path::new("/home/napalm/.config");
        let path = tmux_conf_path(home, config_home, |_| false);
        assert_eq!(path, PathBuf::from("/home/napalm/.tmux.conf"));
    }

    #[test]
    fn hyprland_reload_reports_the_exit_status() {
        assert_eq!(
            dispatch_hyprland(&mut |program, args| {
                assert_eq!(program, "hyprctl");
                assert_eq!(args, ["reload"]);
                Ok(true)
            }),
            Outcome::Reloaded
        );
        assert!(matches!(
            dispatch_hyprland(&mut |_, _| Ok(false)),
            Outcome::Failed(_)
        ));
        assert!(matches!(
            dispatch_hyprland(&mut |_, _| Err(std::io::Error::other("no hyprctl"))),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn tmux_reload_sources_the_resolved_path() {
        let path = Path::new("/home/napalm/.tmux.conf");
        let outcome = dispatch_tmux(path, &mut |program, args| {
            assert_eq!(program, "tmux");
            assert_eq!(args, ["source-file", "/home/napalm/.tmux.conf"]);
            Ok(true)
        });
        assert_eq!(outcome, Outcome::Reloaded);
    }

    #[test]
    fn ghostty_with_no_running_process_is_skipped_not_failed() {
        let outcome = dispatch_ghostty(&[], &mut |_, _| {
            panic!("should not run a command when there is nothing to signal")
        });
        assert!(matches!(outcome, Outcome::Skipped(_)));
    }

    #[test]
    fn ghostty_signals_every_running_pid() {
        let mut signalled = Vec::new();
        let outcome = dispatch_ghostty(&[111, 222], &mut |program, args| {
            assert_eq!(program, "kill");
            signalled.push(args[1].to_string());
            Ok(true)
        });
        assert_eq!(outcome, Outcome::Reloaded);
        assert_eq!(signalled, vec!["111", "222"]);
    }

    #[test]
    fn gtk_qt_sets_both_scheme_and_accent() {
        let palette = builtin_palette("scorched-dark").unwrap();
        let mut calls = Vec::new();
        let outcome = dispatch_gtk_qt(&palette, &mut |program, args| {
            calls.push((program.to_string(), args.join(" ")));
            Ok(true)
        });
        assert_eq!(outcome, Outcome::Reloaded);
        assert_eq!(
            calls,
            vec![
                (
                    "gsettings".to_string(),
                    "set org.gnome.desktop.interface color-scheme prefer-dark".to_string()
                ),
                (
                    "gsettings".to_string(),
                    "set org.gnome.desktop.interface accent-color blue".to_string()
                ),
            ]
        );
    }

    #[test]
    fn gtk_qt_stops_after_the_first_failed_gsettings_call() {
        let palette = builtin_palette("scorched-dark").unwrap();
        let mut calls = 0;
        let outcome = dispatch_gtk_qt(&palette, &mut |_, _| {
            calls += 1;
            Ok(false)
        });
        assert!(matches!(outcome, Outcome::Failed(_)));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_failed_target_fails_the_whole_dispatch_but_others_still_run() {
        let palette = builtin_palette("scorched-dark").unwrap();
        let tmux_conf = Path::new("/home/napalm/.tmux.conf");
        let report = dispatch_all(&palette, tmux_conf, &[], |program, _| {
            Ok(program != "hyprctl")
        });
        let hyprland = report
            .iter()
            .find(|(target, _)| *target == Target::Hyprland)
            .unwrap();
        assert!(matches!(hyprland.1, Outcome::Failed(_)));
        let tmux = report
            .iter()
            .find(|(target, _)| *target == Target::Tmux)
            .unwrap();
        assert_eq!(tmux.1, Outcome::Reloaded);
    }

    #[test]
    fn the_report_names_mode_and_status_for_every_target() {
        let palette = builtin_palette("scorched-dark").unwrap();
        let report = vec![
            (Target::Quickshell, Outcome::Reloaded),
            (
                Target::Ghostty,
                Outcome::Skipped("no running Ghostty process to signal".to_string()),
            ),
            (
                Target::GtkQt,
                Outcome::Failed("gsettings is missing".to_string()),
            ),
        ];
        let text = format_report(&palette, &report);
        assert!(text.starts_with("theme set to Scorched Dark (scorched-dark)\n"));
        assert!(text.contains("quickshell full palette  reloaded"));
        assert!(
            text.contains("ghostty    full palette  skipped: no running Ghostty process to signal")
        );
        assert!(text.contains("gtk/qt     projection    failed: gsettings is missing"));
    }
}
