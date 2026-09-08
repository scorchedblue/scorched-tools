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

/// Runs `tar --zstd -xf payload -C tmp`, `cp -R -n .../linuxbrew target/` and
/// `chown -R uid:gid target`, in that order, stopping at the first failure --
/// the same behaviour `set -euo pipefail` gave the original script.
fn provision(payload: &Path, target: &Path, uid: u32, gid: u32) -> ExitCode {
    let tmp =
        TmpDir(std::env::temp_dir().join(format!("scorched-brew-setup-{}", std::process::id())));
    if fs::create_dir_all(&tmp.0).is_err() {
        eprintln!("scorched: failed to create a working directory");
        return ExitCode::FAILURE;
    }

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
    use super::{Plan, derive_owner, plan};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

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
