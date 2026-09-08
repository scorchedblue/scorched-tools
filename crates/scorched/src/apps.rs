//! `.desktop` entry parsing for the `apps` subcommand.
//!
//! Ported from `scorched-desktop/quickshell/apps.sh`. Quickshell 0.3.1 ships a
//! `DesktopEntries` service that finds nothing at all on this system, so the
//! launcher reads `.desktop` files directly instead of using it.
//!
//! Only the `[Desktop Entry]` section is read. Desktop files also carry
//! `[Desktop Action ...]` sections with their own `Name` and `Exec` keys, and
//! those must not leak into the parsed result -- otherwise a launcher ends up
//! listing "New Window" as though it were an application.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const FIELD_CODES: &str = "fFuUdDnNickvm";

/// One launchable application, in the shape the launcher's JSON consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub name: String,
    pub exec: String,
    pub comment: String,
    pub terminal: bool,
    pub id: String,
}

/// The fields read out of a `[Desktop Entry]` section, before visibility
/// filtering and `Exec` field-code stripping are applied.
#[derive(Debug, Default)]
struct RawEntry {
    name: String,
    exec: String,
    comment: String,
    terminal: bool,
    nodisplay: bool,
    hidden: bool,
    tryexec: String,
    entry_type: String,
}

/// Scans the given directories for visible `.desktop` entries.
///
/// `path_var` is the `PATH` used to resolve a `TryExec` key that names a bare
/// command rather than an absolute path; callers pass `$PATH` in production
/// and a controlled value in tests.
#[must_use]
pub fn scan(dirs: &[PathBuf], path_var: &str) -> Vec<DesktopEntry> {
    // A later directory's entry for the same id replaces an earlier one --
    // XDG precedence, so a user's own entry overrides the system one rather
    // than appearing twice. `default_data_dirs` lists directories in that
    // order, so a plain `HashMap::insert` overwrite is enough.
    let mut by_id: HashMap<String, DesktopEntry> = HashMap::new();
    for dir in dirs {
        for file in desktop_files_in(dir) {
            let Ok(contents) = fs::read_to_string(&file) else {
                continue;
            };
            let raw = parse_desktop_entry_section(&contents);
            if !is_visible(&raw, path_var) {
                continue;
            }
            let id = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            by_id.insert(
                id.clone(),
                DesktopEntry {
                    name: raw.name,
                    exec: strip_field_codes(&raw.exec),
                    comment: raw.comment,
                    terminal: raw.terminal,
                    id,
                },
            );
        }
    }

    let mut entries: Vec<DesktopEntry> = by_id.into_values().collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    entries
}

/// The directories `scan` reads from in production: each `XDG_DATA_DIRS`
/// entry's `applications` subdirectory, then the user's own, last -- so it
/// wins ties.
#[must_use]
pub fn default_data_dirs() -> Vec<PathBuf> {
    let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&xdg_data_dirs)
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("applications"))
        .collect();

    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".local/share"))
        });
    if let Some(home) = data_home {
        dirs.push(home.join("applications"));
    }
    dirs
}

/// Serialises entries to the JSON array the launcher's QML parses.
#[must_use]
pub fn to_json(entries: &[DesktopEntry]) -> String {
    let mut out = String::from("[");
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write!(
            out,
            "{{\"name\":{},\"exec\":{},\"comment\":{},\"terminal\":{},\"id\":{}}}",
            json_string(&entry.name),
            json_string(&entry.exec),
            json_string(&entry.comment),
            entry.terminal,
            json_string(&entry.id),
        )
        .expect("writing to a String cannot fail");
    }
    out.push(']');
    out
}

/// Lists `.desktop` files directly in `dir` and one level of subdirectories,
/// following symlinks -- flatpak exports each application as one, and
/// skipping them silently drops every flatpak on the system.
fn desktop_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            push_if_desktop_file(&mut out, path);
        } else if path.is_dir() {
            let Ok(sub_entries) = fs::read_dir(&path) else {
                continue;
            };
            for sub_entry in sub_entries.flatten() {
                push_if_desktop_file(&mut out, sub_entry.path());
            }
        }
    }
    out
}

fn push_if_desktop_file(out: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_file() && path.extension().is_some_and(|ext| ext == "desktop") {
        out.push(path);
    }
}

/// Reads only the `[Desktop Entry]` section of a `.desktop` file.
///
/// Only the first section with that exact header counts; anything else
/// starting with `[` -- a `[Desktop Action ...]` section -- turns key
/// collection back off. A key's first occurrence wins for `Name`, `Comment`
/// and `Exec`; localised variants such as `Name[fr]` are a different key
/// entirely and are ignored, matching the source's plain-key lookup.
fn parse_desktop_entry_section(contents: &str) -> RawEntry {
    let mut raw = RawEntry::default();
    let mut in_section = false;

    for line in contents.lines() {
        if line.starts_with('[') {
            in_section = line == "[Desktop Entry]";
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(eq_idx) = line.find('=') else {
            continue;
        };
        let key = &line[..eq_idx];
        let val = &line[eq_idx + 1..];
        match key {
            "Name" if raw.name.is_empty() => raw.name = val.to_string(),
            "Comment" if raw.comment.is_empty() => raw.comment = val.to_string(),
            "Exec" if raw.exec.is_empty() => raw.exec = val.to_string(),
            "Terminal" => raw.terminal = val.eq_ignore_ascii_case("true"),
            "NoDisplay" => raw.nodisplay = val.eq_ignore_ascii_case("true"),
            "Hidden" => raw.hidden = val.eq_ignore_ascii_case("true"),
            "TryExec" => raw.tryexec = val.to_string(),
            "Type" => raw.entry_type = val.to_string(),
            _ => {}
        }
    }
    raw
}

/// Whether a parsed entry belongs in the launcher: it must name something
/// launchable, must not ask to be hidden, must be a plain application (or
/// not say otherwise), and its `TryExec`, if any, must resolve to something
/// present.
fn is_visible(raw: &RawEntry, path_var: &str) -> bool {
    if raw.name.is_empty() || raw.exec.is_empty() {
        return false;
    }
    if raw.nodisplay || raw.hidden {
        return false;
    }
    if !raw.entry_type.is_empty() && raw.entry_type != "Application" {
        return false;
    }
    if !raw.tryexec.is_empty() && !tryexec_present(&raw.tryexec, path_var) {
        return false;
    }
    true
}

/// Resolves a `TryExec` value the way a shell would: an absolute path must
/// exist and be executable, a bare name must resolve within `path_var`.
fn tryexec_present(tryexec: &str, path_var: &str) -> bool {
    let candidate = Path::new(tryexec);
    if candidate.is_absolute() {
        return is_executable_file(candidate);
    }
    std::env::split_paths(path_var).any(|dir| is_executable_file(&dir.join(tryexec)))
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Removes field codes (`%f`, `%U`, ...) -- placeholders for files and URLs a
/// real launch would pass in -- and the trailing whitespace left behind.
/// `%%`, an escaped literal percent, is left untouched, matching the source.
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' && chars.peek().is_some_and(|next| FIELD_CODES.contains(*next)) {
            chars.next();
            continue;
        }
        out.push(c);
    }
    out.trim_end_matches([' ', '\t']).to_string()
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).expect("writing to a String cannot fail");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{DesktopEntry, is_executable_file, scan, to_json};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A directory under `target/` (already git-ignored) unique to this test,
    /// cleaned up on drop. Avoids a `tempfile` dependency for a handful of
    /// filesystem-backed tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "scorched-apps-test-{label}-{}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("creating a test temp dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn applications_dir(&self) -> PathBuf {
            let dir = self.0.join("applications");
            fs::create_dir_all(&dir).expect("creating applications dir");
            dir
        }

        fn write_desktop_file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.applications_dir().join(name);
            fs::write(&path, contents).expect("writing a fixture desktop file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scan_one(label: &str, contents: &str) -> Vec<DesktopEntry> {
        let dir = TempDir::new(label);
        dir.write_desktop_file("entry.desktop", contents);
        scan(&[dir.applications_dir()], "")
    }

    #[test]
    fn parses_a_basic_entry() {
        let entries = scan_one(
            "basic",
            "[Desktop Entry]\nName=Foot\nComment=A terminal\nExec=foot\nType=Application\n",
        );
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.name, "Foot");
        assert_eq!(entry.exec, "foot");
        assert_eq!(entry.comment, "A terminal");
        assert!(!entry.terminal);
        assert_eq!(entry.id, "entry");
    }

    #[test]
    fn terminal_true_is_read() {
        let entries = scan_one(
            "terminal",
            "[Desktop Entry]\nName=Vim\nExec=vim\nTerminal=true\n",
        );
        assert!(entries[0].terminal);
    }

    #[test]
    fn no_display_entries_are_excluded() {
        let entries = scan_one(
            "nodisplay",
            "[Desktop Entry]\nName=Hidden Helper\nExec=helper\nNoDisplay=true\n",
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn hidden_entries_are_excluded() {
        let entries = scan_one(
            "hidden",
            "[Desktop Entry]\nName=Deleted App\nExec=deleted\nHidden=true\n",
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn tryexec_pointing_at_something_absent_is_excluded() {
        let entries = scan_one(
            "tryexec-absent",
            "[Desktop Entry]\nName=Ghost\nExec=ghost\nTryExec=/definitely/not/on/this/machine\n",
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn tryexec_pointing_at_an_executable_absolute_path_is_included() {
        let dir = TempDir::new("tryexec-present-abs");
        let binary = dir.path().join("real-binary");
        fs::write(&binary, "#!/bin/sh\n").expect("writing a fake binary");
        let mut perms = fs::metadata(&binary)
            .expect("stat fixture binary")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&binary, perms).expect("chmod fixture binary");
        assert!(is_executable_file(&binary));

        dir.write_desktop_file(
            "entry.desktop",
            &format!(
                "[Desktop Entry]\nName=Real\nExec=real\nTryExec={}\n",
                binary.display()
            ),
        );
        let entries = scan(&[dir.applications_dir()], "");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn tryexec_bare_name_resolves_against_path() {
        let dir = TempDir::new("tryexec-path");
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).expect("creating fake bin dir");
        let binary = bin_dir.join("mytool");
        fs::write(&binary, "#!/bin/sh\n").expect("writing a fake binary");
        let mut perms = fs::metadata(&binary)
            .expect("stat fixture binary")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        fs::set_permissions(&binary, perms).expect("chmod fixture binary");

        dir.write_desktop_file(
            "entry.desktop",
            "[Desktop Entry]\nName=My Tool\nExec=mytool\nTryExec=mytool\n",
        );
        let path_var = bin_dir.display().to_string();
        let entries = scan(&[dir.applications_dir()], &path_var);
        assert_eq!(entries.len(), 1);

        let entries_without_path = scan(&[dir.applications_dir()], "");
        assert!(entries_without_path.is_empty());
    }

    #[test]
    fn desktop_action_sections_do_not_leak_into_the_entry() {
        let entries = scan_one(
            "action",
            "[Desktop Entry]\nName=Foot\nExec=foot\n\n[Desktop Action new-window]\nName=New Window\nExec=foot --new\n",
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Foot");
        assert_eq!(entries[0].exec, "foot");
    }

    #[test]
    fn entries_missing_name_or_exec_are_excluded() {
        assert!(scan_one("no-name", "[Desktop Entry]\nExec=foo\n").is_empty());
        assert!(scan_one("no-exec", "[Desktop Entry]\nName=Foo\n").is_empty());
    }

    #[test]
    fn type_link_is_excluded_but_missing_type_is_included() {
        assert!(
            scan_one(
                "type-link",
                "[Desktop Entry]\nName=Foo\nExec=foo\nType=Link\n"
            )
            .is_empty()
        );
        assert_eq!(
            scan_one("type-missing", "[Desktop Entry]\nName=Foo\nExec=foo\n").len(),
            1
        );
    }

    #[test]
    fn field_codes_are_stripped_from_exec() {
        let entries = scan_one(
            "field-codes",
            "[Desktop Entry]\nName=Foo\nExec=foo %U %f --flag\n",
        );
        assert_eq!(entries[0].exec, "foo   --flag");
    }

    #[test]
    fn localized_name_does_not_override_the_plain_key() {
        let entries = scan_one(
            "localized",
            "[Desktop Entry]\nName=Foo\nName[fr]=Bonjour\nExec=foo\n",
        );
        assert_eq!(entries[0].name, "Foo");
    }

    #[test]
    fn a_later_directory_wins_the_same_id() {
        let system = TempDir::new("dedup-system");
        let user = TempDir::new("dedup-user");
        system.write_desktop_file(
            "app.desktop",
            "[Desktop Entry]\nName=App\nExec=app-system\nComment=system\n",
        );
        user.write_desktop_file(
            "app.desktop",
            "[Desktop Entry]\nName=App\nExec=app-user\nComment=user\n",
        );
        let entries = scan(&[system.applications_dir(), user.applications_dir()], "");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].comment, "user");
    }

    #[test]
    fn one_level_of_nested_subdirectories_is_scanned() {
        let dir = TempDir::new("nested");
        let nested = dir.applications_dir().join("kde4");
        fs::create_dir_all(&nested).expect("creating nested applications dir");
        fs::write(
            nested.join("nested.desktop"),
            "[Desktop Entry]\nName=Nested\nExec=nested\n",
        )
        .expect("writing nested fixture");
        let entries = scan(&[dir.applications_dir()], "");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Nested");
    }

    #[test]
    fn json_output_matches_the_shape_the_qml_expects() {
        let entries = vec![DesktopEntry {
            name: "Fire \"Fox\"".to_string(),
            exec: "firefox".to_string(),
            comment: "Browse the web".to_string(),
            terminal: false,
            id: "firefox".to_string(),
        }];
        assert_eq!(
            to_json(&entries),
            r#"[{"name":"Fire \"Fox\"","exec":"firefox","comment":"Browse the web","terminal":false,"id":"firefox"}]"#
        );
        assert_eq!(to_json(&[]), "[]");
    }
}
