//! The palette contract -- the theme engine's only input.
//!
//! A resolved palette is a small TOML file like the ones under `palettes/`.
//! Wallpaper extraction, if ever built, is a separate front-end that emits
//! one of these; this module never does colour science, which is what keeps
//! rendering deterministic and testable.
//!
//! # Schema
//!
//! ```toml
//! # Comment lines start with '#'.
//! name = "Scorched Dark"
//! slug = "scorched-dark"
//!
//! [colors]
//! background = "#11151cff"
//! foreground = "#cdd6f4ff"
//! ```
//!
//! - `name` and `slug` are required, quoted strings.
//! - `[colors]` maps an arbitrary colour name to a hex string: `#rrggbb`
//!   (alpha defaults to `0xff`) or `#rrggbbaa`. Each resolves to a [`Color`]
//!   carrying the canonical lowercase hex, the `(r, g, b)` triple and
//!   `alpha` as `0.0..=1.0` -- hex, RGB and alpha, the contract this schema
//!   promises.
//! - Only this subset of TOML is understood: no arrays, no nested tables
//!   beyond `[colors]`, no inline comments. That is enough for a curated
//!   scheme file and keeps this crate dependency-free -- see `AGENTS.md`.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::process::ExitCode;

/// One colour, resolved from its hex literal into every form the engine
/// needs: the canonical lowercase `#rrggbbaa` hex, the `(r, g, b)` triple,
/// and `alpha` as `0.0..=1.0`.
#[derive(Debug, Clone, PartialEq)]
pub struct Color {
    pub hex: String,
    pub rgb: (u8, u8, u8),
    pub alpha: f32,
}

impl Color {
    /// Resolves a `#rrggbb` or `#rrggbbaa` literal. Alpha defaults to `0xff`
    /// (fully opaque) when only six digits are given.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidHex`] when `hex` is not `#` followed by exactly six
    /// or eight hex digits.
    pub fn from_hex(hex: &str) -> Result<Self, InvalidHex> {
        let Some(digits) = hex.strip_prefix('#') else {
            return Err(InvalidHex(format!("{hex:?} does not start with '#'")));
        };
        let valid_len = digits.len() == 6 || digits.len() == 8;
        if !valid_len || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(InvalidHex(format!(
                "{hex:?} is not '#' followed by 6 or 8 hex digits"
            )));
        }

        // SAFETY net for the indexing below: every char above was just
        // proven to be an ASCII hex digit, so byte offsets are char
        // boundaries and `from_str_radix` cannot fail.
        let byte = |range: std::ops::Range<usize>| u8::from_str_radix(&digits[range], 16).unwrap();
        let r = byte(0..2);
        let g = byte(2..4);
        let b = byte(4..6);
        let a = if digits.len() == 8 { byte(6..8) } else { 0xff };

        Ok(Self {
            hex: format!("#{r:02x}{g:02x}{b:02x}{a:02x}"),
            rgb: (r, g, b),
            alpha: f32::from(a) / 255.0,
        })
    }
}

/// `Color::from_hex` rejected its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidHex(String);

impl fmt::Display for InvalidHex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InvalidHex {}

/// A resolved palette: a name, a slug, and the named colours the engine
/// renders per target.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub name: String,
    pub slug: String,
    pub colors: BTreeMap<String, Color>,
}

/// [`parse`] rejected its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingField(&'static str),
    UnknownField {
        line: usize,
        field: String,
    },
    UnsupportedSection {
        line: usize,
        section: String,
    },
    Malformed {
        line: usize,
    },
    InvalidColor {
        line: usize,
        key: String,
        reason: String,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(field) => write!(f, "missing required field {field:?}"),
            Self::UnknownField { line, field } => {
                write!(f, "line {line}: unknown field {field:?}")
            }
            Self::UnsupportedSection { line, section } => {
                write!(f, "line {line}: unsupported section {section:?}")
            }
            Self::Malformed { line } => write!(f, "line {line}: not `key = \"value\"`"),
            Self::InvalidColor { line, key, reason } => {
                write!(f, "line {line}: colour {key:?}: {reason}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses the schema documented at the top of this module.
///
/// # Errors
///
/// Returns [`ParseError`] on the first line that does not fit the schema,
/// or when `name` or `slug` is never set.
pub fn parse(input: &str) -> Result<Palette, ParseError> {
    let mut name = None;
    let mut slug = None;
    let mut colors = BTreeMap::new();
    let mut in_colors = false;

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[colors]" {
            in_colors = true;
            continue;
        }
        if line.starts_with('[') {
            return Err(ParseError::UnsupportedSection {
                line: line_no,
                section: line.to_string(),
            });
        }

        let Some((key, value)) = line.split_once('=') else {
            return Err(ParseError::Malformed { line: line_no });
        };
        let key = key.trim();
        let value = parse_quoted(value.trim(), line_no)?;

        if in_colors {
            let color = Color::from_hex(&value).map_err(|source| ParseError::InvalidColor {
                line: line_no,
                key: key.to_string(),
                reason: source.to_string(),
            })?;
            colors.insert(key.to_string(), color);
        } else {
            match key {
                "name" => name = Some(value),
                "slug" => slug = Some(value),
                other => {
                    return Err(ParseError::UnknownField {
                        line: line_no,
                        field: other.to_string(),
                    });
                }
            }
        }
    }

    Ok(Palette {
        name: name.ok_or(ParseError::MissingField("name"))?,
        slug: slug.ok_or(ParseError::MissingField("slug"))?,
        colors,
    })
}

fn parse_quoted(value: &str, line: usize) -> Result<String, ParseError> {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .map(str::to_string)
        .ok_or(ParseError::Malformed { line })
}

/// An application-facing output shape the engine can fan a resolved palette
/// out to. More targets land as the applications that need them are added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// CSS custom properties, one `--name: #hex;` declaration per colour.
    Css,
    /// Shell-exportable `SCORCHED_NAME=#hex` assignments, one per line.
    Env,
}

/// Renders a resolved palette for the given target. Colours are emitted in
/// name order, so the same palette always renders to the same string.
#[must_use]
pub fn render(palette: &Palette, target: Target) -> String {
    match target {
        Target::Css => render_css(palette),
        Target::Env => render_env(palette),
    }
}

fn render_css(palette: &Palette) -> String {
    let mut out = String::from(":root {\n");
    for (name, color) in &palette.colors {
        writeln!(out, "  --{name}: {};", color.hex).expect("writing to a String cannot fail");
    }
    out.push_str("}\n");
    out
}

fn render_env(palette: &Palette) -> String {
    let mut out = String::new();
    for (name, color) in &palette.colors {
        let var = name.to_uppercase().replace('-', "_");
        writeln!(out, "SCORCHED_{var}={}", color.hex).expect("writing to a String cannot fail");
    }
    out
}

/// Reads a palette file and prints it rendered for `target`, the `palette`
/// subcommand: `scorched palette <path> <css|env>`.
#[must_use]
pub fn run(path: Option<&str>, target: Option<&str>) -> ExitCode {
    let (Some(path), Some(target)) = (path, target) else {
        eprintln!("usage: scorched palette <path> <css|env>");
        return ExitCode::FAILURE;
    };

    let target = match target {
        "css" => Target::Css,
        "env" => Target::Env,
        other => {
            eprintln!("scorched: unknown palette target '{other}'");
            return ExitCode::FAILURE;
        }
    };

    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            eprintln!("scorched: failed to read {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    match parse(&contents) {
        Ok(palette) => {
            print!("{}", render(&palette, target));
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("scorched: failed to parse {path}: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Color, ParseError, Target, parse, render};

    const SCHEME: &str = "\
# a comment, and a blank line follow

name = \"Test Scheme\"
slug = \"test-scheme\"

[colors]
background = \"#11151cff\"
accent = \"#5aa9e6\"
";

    #[test]
    fn hex_with_explicit_alpha_resolves_rgb_and_alpha() {
        let color = Color::from_hex("#5aa9e680").unwrap();
        assert_eq!(color.hex, "#5aa9e680");
        assert_eq!(color.rgb, (0x5a, 0xa9, 0xe6));
        assert!((color.alpha - (f32::from(0x80u8) / 255.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn hex_without_alpha_defaults_to_opaque() {
        let color = Color::from_hex("#5aa9e6").unwrap();
        assert_eq!(color.hex, "#5aa9e6ff");
        assert!((color.alpha - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn hex_rejects_the_wrong_shape() {
        assert!(Color::from_hex("5aa9e6").is_err());
        assert!(Color::from_hex("#5aa9e").is_err());
        assert!(Color::from_hex("#5aa9e6zz").is_err());
    }

    #[test]
    fn parses_a_known_scheme_into_a_known_palette() {
        let palette = parse(SCHEME).unwrap();
        assert_eq!(palette.name, "Test Scheme");
        assert_eq!(palette.slug, "test-scheme");
        assert_eq!(palette.colors.len(), 2);
        assert_eq!(palette.colors["background"].hex, "#11151cff");
        assert_eq!(palette.colors["accent"].hex, "#5aa9e6ff");
    }

    #[test]
    fn a_missing_required_field_is_rejected() {
        let err = parse("slug = \"only-slug\"\n").unwrap_err();
        assert_eq!(err, ParseError::MissingField("name"));
    }

    #[test]
    fn an_unknown_top_level_field_is_rejected() {
        let err = parse("name = \"X\"\nslug = \"x\"\nnope = \"1\"\n").unwrap_err();
        assert_eq!(
            err,
            ParseError::UnknownField {
                line: 3,
                field: "nope".to_string()
            }
        );
    }

    #[test]
    fn an_unsupported_section_is_rejected() {
        let err = parse("name = \"X\"\nslug = \"x\"\n[fonts]\n").unwrap_err();
        assert_eq!(
            err,
            ParseError::UnsupportedSection {
                line: 3,
                section: "[fonts]".to_string()
            }
        );
    }

    #[test]
    fn an_invalid_colour_names_its_line_and_key() {
        let err = parse("name = \"X\"\nslug = \"x\"\n[colors]\nbg = \"nope\"\n").unwrap_err();
        assert_eq!(
            err,
            ParseError::InvalidColor {
                line: 4,
                key: "bg".to_string(),
                reason: "\"nope\" does not start with '#'".to_string()
            }
        );
    }

    #[test]
    fn a_known_palette_renders_known_css() {
        let palette = parse(SCHEME).unwrap();
        assert_eq!(
            render(&palette, Target::Css),
            "\
:root {
  --accent: #5aa9e6ff;
  --background: #11151cff;
}
"
        );
    }

    #[test]
    fn a_known_palette_renders_known_env() {
        let palette = parse(SCHEME).unwrap();
        assert_eq!(
            render(&palette, Target::Env),
            "\
SCORCHED_ACCENT=#5aa9e6ff
SCORCHED_BACKGROUND=#11151cff
"
        );
    }

    #[test]
    fn the_curated_dark_scheme_parses_and_renders_deterministically() {
        let palette = parse(include_str!("../palettes/scorched-dark.toml")).unwrap();
        assert_eq!(palette.slug, "scorched-dark");
        assert!(palette.colors.contains_key("background"));
        assert!(palette.colors.contains_key("foreground"));
        assert_eq!(
            render(&palette, Target::Css),
            render(
                &parse(include_str!("../palettes/scorched-dark.toml")).unwrap(),
                Target::Css
            )
        );
    }

    #[test]
    fn the_curated_light_scheme_parses_and_renders_deterministically() {
        let palette = parse(include_str!("../palettes/scorched-light.toml")).unwrap();
        assert_eq!(palette.slug, "scorched-light");
        assert!(palette.colors.contains_key("background"));
        assert!(palette.colors.contains_key("foreground"));
        assert_eq!(
            render(&palette, Target::Env),
            render(
                &parse(include_str!("../palettes/scorched-light.toml")).unwrap(),
                Target::Env
            )
        );
    }
}
