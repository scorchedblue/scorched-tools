//! First-boot Homebrew provisioning for the `brew-setup` subcommand.
//!
//! Ported from `files/usr/libexec/scorched-brew-setup` in
//! `scorchedblue/scorchedblue`. `/home` is a symlink to `var/home`, so
//! `/home/linuxbrew` lives in `/var` -- deployment state, not the image's
//! immutable `/usr`. An image therefore cannot *contain* an installed brew;
//! it can only ship the payload and this mechanism to unpack it.
//!
//! Deriving the owner rather than hardcoding 1000:1000: a fixed UID is
//! correct on a single-user machine and silently wrong otherwise. Falls back
//! to 1000 only when no human account exists yet, which is the case on first
//! boot.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PAYLOAD: &str = "/usr/share/homebrew.tar.zst";
const TARGET: &str = "/home/linuxbrew";

/// The first UID >= 1000 and < 65534 in `/etc/passwd` content, falling back
/// to 1000, and the matching GID -- falling back to the UID itself when no
/// passwd line has that UID. Mirrors the original script's two `awk` passes
/// exactly, including that the GID pass runs unconditionally against the
/// (possibly-fallback) UID rather than only against a UID actually found.
#[must_use]
fn derive_owner(passwd: &str) -> (u32, u32) {
    fn field(line: &str, index: usize) -> Option<u32> {
        line.split(':').nth(index)?.parse().ok()
    }

    let uid = passwd
        .lines()
        .filter_map(|line| field(line, 2))
        .find(|&uid| (1000..65534).contains(&uid))
        .unwrap_or(1000);

    let gid = passwd
        .lines()
        .find_map(|line| {
            if field(line, 2)? != uid {
                return None;
            }
            field(line, 3)
        })
        .unwrap_or(uid);

    (uid, gid)
}

/// What to do, decided before touching `tar`, `cp` or `chown`.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    NoPayload,
    AlreadyProvisioned,
    Provision { uid: u32, gid: u32 },
}

/// Preflight checks shared with the original script's early exits, kept
/// separate from the actual provisioning so they can be tested against a
/// fixture directory instead of the real, root-owned `/`.
fn plan(payload: &Path, target: &Path, passwd: &str) -> Plan {
    if !payload.is_file() {
        return Plan::NoPayload;
    }
    if target.join(".linuxbrew").is_dir() {
        return Plan::AlreadyProvisioned;
    }
    let (uid, gid) = derive_owner(passwd);
    Plan::Provision { uid, gid }
}

/// Removes its directory on drop, mirroring the original script's
/// `trap 'rm -rf "${tmp}"' EXIT`: cleanup runs whether provisioning
/// succeeded or a step below failed.
struct TmpDir(PathBuf);

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A working directory nothing else can have created, owned or entered.
///
/// This runs as root and extracts an archive whose contents are then copied
/// into `/home/linuxbrew` with `cp -n` and handed to the desktop user with
/// `chown -R`. A directory an unprivileged process could pre-create is
/// therefore a directory that process can seed: `cp -n` will not clobber what
/// is already there, so anything it planted survives the copy and is then given
/// away. The unit sets no `PrivateTmp=`, so `/tmp` really is shared.
///
/// Two properties close that, and the original shell script had both from
/// `mktemp -d`. The port to Rust replaced it with a PID-derived name and
/// `create_dir_all`, which has neither -- `create_dir_all` succeeds on a
/// directory that already exists (#30).
///
///   * **Unguessable.** 16 hex characters from `/dev/urandom`, not the PID.
///   * **Exclusive.** `DirBuilder` without `recursive` fails with
///     `AlreadyExists` rather than adopting, and `.mode(0o700)` is applied by
///     `mkdir(2)` itself, so there is no window between creation and
///     permissions.
///
/// Reading `/dev/urandom` rather than taking a dependency: this crate has none
/// by design, and `tempfile` would mean a vetting pass and `cargo-deny` for
/// sixteen bytes.
fn make_work_dir(parent: &Path) -> std::io::Result<PathBuf> {
    let mut bytes = [0u8; 8];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut suffix = String::with_capacity(16);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(suffix, "{byte:02x}");
    }
    let path = parent.join(format!("scorched-brew-setup-{suffix}"));
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
}

/// Runs `tar --zstd -xf payload -C tmp`, `cp -R -n .../linuxbrew target/` and
/// `chown -R uid:gid target`, in that order, stopping at the first failure --
/// the same behaviour `set -euo pipefail` gave the original script.
fn provision(payload: &Path, target: &Path, uid: u32, gid: u32) -> ExitCode {
    let Ok(work) = make_work_dir(&std::env::temp_dir()) else {
        eprintln!("scorched: failed to create a working directory");
        return ExitCode::FAILURE;
    };
    let tmp = TmpDir(work);

    let steps: [Command; 3] = [
        {
            let mut c = Command::new("tar");
            c.args(["--zstd", "-xf"]).arg(payload).arg("-C").arg(&tmp.0);
            c
        },
        {
            if fs::create_dir_all(target).is_err() {
                eprintln!("scorched: failed to create {}", target.display());
                return ExitCode::FAILURE;
            }
            let mut c = Command::new("cp");
            c.args(["-R", "-n"])
                .arg(tmp.0.join("home/linuxbrew/.linuxbrew"))
                .arg(target);
            c
        },
        {
            let mut c = Command::new("chown");
            c.arg("-R").arg(format!("{uid}:{gid}")).arg(target);
            c
        },
    ];

    for mut step in steps {
        match step.status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let code = status.code().unwrap_or(1).rem_euclid(256);
                return ExitCode::from(u8::try_from(code).unwrap_or(1));
            }
            Err(err) => {
                eprintln!(
                    "scorched: failed to run {}: {err}",
                    step.get_program().display()
                );
                return ExitCode::FAILURE;
            }
        }
    }

    println!("brew provisioned to {} for {uid}:{gid}", target.display());
    ExitCode::SUCCESS
}

#[must_use]
pub fn run() -> ExitCode {
    let payload = Path::new(PAYLOAD);
    let target = Path::new(TARGET);
    let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();

    match plan(payload, target, &passwd) {
        Plan::NoPayload => {
            eprintln!("no brew payload at {}", payload.display());
            ExitCode::FAILURE
        }
        Plan::AlreadyProvisioned => {
            println!("brew already present");
            ExitCode::SUCCESS
        }
        Plan::Provision { uid, gid } => provision(payload, target, uid, gid),
    }
}

#[cfg(test)]
mod tests {
    use super::{Plan, derive_owner, make_work_dir, plan};
    use std::fs;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn the_work_directory_is_private_to_root() {
        // 0700 comes from mkdir(2) itself, not a later chmod: there must be no
        // window in which the directory exists and is readable. This runs as
        // root and later `chown -R`s what it extracts to the desktop user, so a
        // directory another process can enter is one it can seed.
        let parent = TempDir::new("workdir-mode");
        let dir = make_work_dir(&parent.0).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "work directory must be 0700, got {mode:o}");
        assert!(dir.is_dir());
    }

    #[test]
    fn the_work_directory_name_is_not_guessable() {
        // It was `scorched-brew-setup-<pid>`, which anything can predict and
        // pre-create -- and `create_dir_all` adopted it rather than failing
        // (#30). Two calls must not collide, which a PID-derived name would.
        let parent = TempDir::new("workdir-unique");
        let first = make_work_dir(&parent.0).unwrap();
        let second = make_work_dir(&parent.0).unwrap();
        assert_ne!(first, second);
        for dir in [&first, &second] {
            let name = dir.file_name().unwrap().to_str().unwrap();
            let suffix = name.strip_prefix("scorched-brew-setup-").unwrap();
            assert_eq!(suffix.len(), 16, "expected 16 hex chars, got {suffix:?}");
            assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn an_existing_directory_is_refused_rather_than_adopted() {
        // The property `create_dir_all` lacked. Asserted against the same
        // DirBuilder call make_work_dir uses, because "it would have failed" is
        // the whole fix.
        let parent = TempDir::new("workdir-exclusive");
        let squatted = parent.0.join("squatted");
        fs::create_dir(&squatted).unwrap();
        let err = fs::DirBuilder::new()
            .mode(0o700)
            .create(&squatted)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "scorched-brew-setup-test-{label}-{}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("creating a test temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
bin:x:1:1:bin:/bin:/sbin/nologin
nobody:x:65534:65534:nobody:/:/sbin/nologin
napalm:x:1000:1000:Napalm:/home/napalm:/bin/bash
service:x:1001:1001:service:/home/service:/sbin/nologin
";

    #[test]
    fn picks_the_first_uid_at_or_above_1000() {
        assert_eq!(derive_owner(PASSWD), (1000, 1000));
    }

    #[test]
    fn skips_system_and_nobody_uids() {
        let passwd = "sys:x:999:999:sys:/:/sbin/nologin\nnobody:x:65534:65534::/:/sbin/nologin\n";
        assert_eq!(derive_owner(passwd), (1000, 1000));
    }

    #[test]
    fn falls_back_to_1000_1000_when_no_human_account_exists() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n";
        assert_eq!(derive_owner(passwd), (1000, 1000));
    }

    #[test]
    fn a_mismatched_gid_column_is_still_picked_up() {
        let passwd = "svc:x:1000:2000:svc:/home/svc:/sbin/nologin\n";
        assert_eq!(derive_owner(passwd), (1000, 2000));
    }

    #[test]
    fn plan_reports_no_payload_when_the_archive_is_missing() {
        let dir = TempDir::new("no-payload");
        let payload = dir.path().join("homebrew.tar.zst");
        let target = dir.path().join("linuxbrew");
        assert_eq!(plan(&payload, &target, PASSWD), Plan::NoPayload);
    }

    #[test]
    fn plan_reports_already_provisioned_when_linuxbrew_exists() {
        let dir = TempDir::new("already-there");
        let payload = dir.path().join("homebrew.tar.zst");
        fs::write(&payload, b"fake payload").unwrap();
        let target = dir.path().join("linuxbrew");
        fs::create_dir_all(target.join(".linuxbrew")).unwrap();

        assert_eq!(plan(&payload, &target, PASSWD), Plan::AlreadyProvisioned);
    }

    #[test]
    fn plan_provisions_with_the_derived_owner_when_clear_to_proceed() {
        let dir = TempDir::new("clear-to-proceed");
        let payload = dir.path().join("homebrew.tar.zst");
        fs::write(&payload, b"fake payload").unwrap();
        let target = dir.path().join("linuxbrew");

        assert_eq!(
            plan(&payload, &target, PASSWD),
            Plan::Provision {
                uid: 1000,
                gid: 1000
            }
        );
    }
}
