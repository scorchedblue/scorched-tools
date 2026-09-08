//! CPU performance mode for the `performance` subcommand.
//!
//! Ported from `files/usr/libexec/scorched-performance` in
//! `scorchedblue/scorchedblue`. `ScorchedBlue` targets desktops. There is no
//! battery to preserve, so the kernel's default of trading latency for power
//! is the wrong trade here every time.
//!
//! Two knobs, not one. On `intel_pstate` in active mode -- the common case on
//! a modern Intel desktop -- the scaling governor is only half the story: the
//! hardware also takes an energy/performance hint, and leaving that at
//! `balance_performance` holds clocks back even with the governor set to
//! `performance`. A machine "set to performance" with EPP still balanced is
//! the usual reason this change appears to do nothing.
//!
//! Deliberately tolerant. Not every machine has cpufreq at all (a VM often
//! does not), drivers expose different governor sets, and `amd-pstate` and
//! `acpi-cpufreq` name things differently. Anything absent is skipped rather
//! than failed on: the unit must not go red on hardware that simply has
//! fewer knobs.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

const CPU_ROOT: &str = "/sys/devices/system/cpu";
const PERFORMANCE: &str = "performance";

/// What happened during one pass over every `cpuN/cpufreq` directory under
/// the CPU root.
#[derive(Debug, Default, PartialEq, Eq)]
struct Report {
    cpus: usize,
    wrote_governor: usize,
    wrote_epp: usize,
    cpu0_governor: Option<String>,
    cpu0_epp: Option<String>,
}

/// Whether `name` is a `cpuN` directory name, mirroring the shell glob
/// `cpu[0-9]*`: the `cpu` prefix followed by at least one digit.
fn is_cpu_dir_name(name: &str) -> bool {
    name.strip_prefix("cpu")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_digit())
}

/// Whether `dir/available_file` lists `value` as a whole word, mirroring
/// `grep -qw`. A missing file is "no", not an error -- the same tolerance the
/// original script gives an absent knob.
fn offers(dir: &Path, available_file: &str, value: &str) -> bool {
    fs::read_to_string(dir.join(available_file))
        .is_ok_and(|contents| contents.split_whitespace().any(|word| word == value))
}

/// Writes `value` to `dir/target_file` when the driver offers it, returning
/// whether the write happened. Both the offer check and the write failing
/// are tolerated silently, exactly as the original script's
/// `echo ... 2>/dev/null` did.
fn set_if_offered(dir: &Path, target_file: &str, available_file: &str, value: &str) -> bool {
    dir.join(target_file).is_file()
        && offers(dir, available_file, value)
        && fs::write(dir.join(target_file), value).is_ok()
}

/// Applies the performance mode under `cpu_root`, returning what took.
/// `cpu_root` is injected so the decision logic can be tested against a
/// fixture directory tree instead of the real, root-owned `/sys`.
fn apply(cpu_root: &Path) -> Report {
    let mut report = Report::default();

    let Ok(entries) = fs::read_dir(cpu_root) else {
        return report;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !is_cpu_dir_name(&name) {
            continue;
        }
        let cpufreq_dir = entry.path().join("cpufreq");
        if !cpufreq_dir.is_dir() {
            continue;
        }
        report.cpus += 1;

        if set_if_offered(
            &cpufreq_dir,
            "scaling_governor",
            "scaling_available_governors",
            PERFORMANCE,
        ) {
            report.wrote_governor += 1;
        }
        if set_if_offered(
            &cpufreq_dir,
            "energy_performance_preference",
            "energy_performance_available_preferences",
            PERFORMANCE,
        ) {
            report.wrote_epp += 1;
        }
    }

    if report.cpus == 0 {
        return report;
    }

    // Turbo is normally already enabled; assert rather than assume, since a
    // firmware or a previous boot could have disabled it.
    let no_turbo = cpu_root.join("intel_pstate/no_turbo");
    if no_turbo.is_file() {
        let _ = fs::write(&no_turbo, "0");
    }

    let cpu0 = cpu_root.join("cpu0/cpufreq");
    report.cpu0_governor = fs::read_to_string(cpu0.join("scaling_governor"))
        .ok()
        .map(|s| s.trim().to_string());
    report.cpu0_epp = fs::read_to_string(cpu0.join("energy_performance_preference"))
        .ok()
        .map(|s| s.trim().to_string());

    report
}

/// The lines the original script echoed, in order. Report what actually
/// took, not what was attempted -- this is the whole reason to read
/// `systemctl status scorched-performance`.
fn report_lines(report: &Report) -> Vec<String> {
    if report.cpus == 0 {
        return vec!["no cpufreq interface present; nothing to do".to_string()];
    }
    vec![
        format!(
            "cpufreq: {} policies, governor set on {}, EPP set on {}",
            report.cpus, report.wrote_governor, report.wrote_epp
        ),
        format!(
            "cpu0 now: governor={} epp={}",
            report.cpu0_governor.as_deref().unwrap_or("?"),
            report.cpu0_epp.as_deref().unwrap_or("n/a")
        ),
    ]
}

#[must_use]
pub fn run() -> ExitCode {
    let report = apply(Path::new(CPU_ROOT));
    for line in report_lines(&report) {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{apply, report_lines};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A directory under `target/` (already git-ignored) unique to this
    /// test, cleaned up on drop. Avoids a `tempfile` dependency for a
    /// handful of filesystem-backed tests.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "scorched-performance-test-{label}-{}-{n}",
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

    fn write_cpu(root: &Path, n: u32, governor_values: &[&str], epp_values: &[&str]) {
        let dir = root.join(format!("cpu{n}/cpufreq"));
        fs::create_dir_all(&dir).expect("creating a fixture cpufreq dir");
        fs::write(dir.join("scaling_governor"), "powersave").unwrap();
        fs::write(
            dir.join("scaling_available_governors"),
            governor_values.join(" "),
        )
        .unwrap();
        fs::write(
            dir.join("energy_performance_preference"),
            "balance_performance",
        )
        .unwrap();
        fs::write(
            dir.join("energy_performance_available_preferences"),
            epp_values.join(" "),
        )
        .unwrap();
    }

    #[test]
    fn an_empty_cpu_root_reports_zero_cpus() {
        let root = TempDir::new("empty");
        let report = apply(root.path());
        assert_eq!(report.cpus, 0);
        assert_eq!(
            report_lines(&report),
            vec!["no cpufreq interface present; nothing to do"]
        );
    }

    #[test]
    fn sets_governor_and_epp_when_both_are_offered() {
        let root = TempDir::new("both-offered");
        write_cpu(
            root.path(),
            0,
            &["powersave", "performance"],
            &["balance_performance", "performance"],
        );

        let report = apply(root.path());

        assert_eq!(report.cpus, 1);
        assert_eq!(report.wrote_governor, 1);
        assert_eq!(report.wrote_epp, 1);
        assert_eq!(
            fs::read_to_string(root.path().join("cpu0/cpufreq/scaling_governor")).unwrap(),
            "performance"
        );
        assert_eq!(
            fs::read_to_string(
                root.path()
                    .join("cpu0/cpufreq/energy_performance_preference")
            )
            .unwrap(),
            "performance"
        );
        assert_eq!(report.cpu0_governor.as_deref(), Some("performance"));
        assert_eq!(report.cpu0_epp.as_deref(), Some("performance"));
    }

    #[test]
    fn skips_a_knob_the_driver_does_not_offer() {
        let root = TempDir::new("epp-unavailable");
        write_cpu(
            root.path(),
            0,
            &["powersave", "performance"],
            &["balance_performance"],
        );

        let report = apply(root.path());

        assert_eq!(report.wrote_governor, 1);
        assert_eq!(report.wrote_epp, 0);
        assert_eq!(
            fs::read_to_string(
                root.path()
                    .join("cpu0/cpufreq/energy_performance_preference")
            )
            .unwrap(),
            "balance_performance"
        );
    }

    #[test]
    fn counts_every_policy_directory_present() {
        let root = TempDir::new("multi-cpu");
        for n in 0..4 {
            write_cpu(
                root.path(),
                n,
                &["powersave", "performance"],
                &["performance"],
            );
        }
        let report = apply(root.path());
        assert_eq!(report.cpus, 4);
        assert_eq!(report.wrote_governor, 4);
        assert_eq!(report.wrote_epp, 4);
    }

    #[test]
    fn ignores_directories_that_are_not_cpu_policies() {
        let root = TempDir::new("stray-dirs");
        write_cpu(root.path(), 0, &["performance"], &["performance"]);
        fs::create_dir_all(root.path().join("cpufreq")).unwrap();
        fs::create_dir_all(root.path().join("cpuidle")).unwrap();

        let report = apply(root.path());

        assert_eq!(report.cpus, 1);
    }

    #[test]
    fn enables_turbo_when_the_knob_is_present() {
        let root = TempDir::new("turbo");
        write_cpu(root.path(), 0, &["performance"], &["performance"]);
        let pstate = root.path().join("intel_pstate");
        fs::create_dir_all(&pstate).unwrap();
        fs::write(pstate.join("no_turbo"), "1").unwrap();

        apply(root.path());

        assert_eq!(fs::read_to_string(pstate.join("no_turbo")).unwrap(), "0");
    }

    #[test]
    fn a_missing_scaling_governor_file_is_reported_as_unknown() {
        let root = TempDir::new("no-governor-file");
        let dir = root.path().join("cpu0/cpufreq");
        fs::create_dir_all(&dir).unwrap();

        let report = apply(root.path());

        assert_eq!(report.cpus, 1);
        assert_eq!(report.wrote_governor, 0);
        assert_eq!(report.cpu0_governor, None);
        assert_eq!(report_lines(&report)[1], "cpu0 now: governor=? epp=n/a");
    }
}
