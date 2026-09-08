//! Idempotent monitor power control for the `screen-power` subcommand.
//!
//! Ported from `scorched-desktop/quickshell/screen-power.sh`. The reasoning
//! below is preserved from that script because it is exactly the kind of
//! thing that gets "simplified" back into a bug:
//!
//! Hyprland 0.56 gives no way to *command* a DPMS state.
//!
//!   hyprctl dispatch dpms off             -> parse error; the pre-Lua syntax is gone
//!   hyprctl dispatch 'hl.dsp.dpms("on")'         -> toggles, argument ignored
//!   hyprctl dispatch 'hl.dsp.dpms({state="on"})' -> toggles, argument ignored
//!   hyprctl dispatch 'hl.dsp.dpms({on=true})'    -> toggles, argument ignored
//!
//! All three were checked by calling the same form twice and watching the
//! state alternate. A bare toggle on an idle timer is dangerous: miss one
//! edge and the screen stays dark after the user comes back.
//!
//! So: read the state first, and toggle only when it differs from the one
//! asked for. That builds a reliable "set" out of an unreliable "toggle", and
//! makes repeated calls harmless -- which is what an idle daemon needs, since
//! it may fire resume more than once.
//!
//! The toggle applies to every monitor; the dispatcher ignores a monitor
//! argument too. On a multi-head setup this is all-or-nothing.

use std::process::{Command, ExitCode};
use std::time::Duration;

/// How long to wait after an unconfirmed toggle before checking again. Not a
/// retry loop: one wait, one recheck, then give up rather than strobe the
/// display.
const CONFIRM_DELAY: Duration = Duration::from_millis(400);

/// Whether `hyprctl monitors -j` output says any monitor is on.
///
/// `dpmsStatus` is true when a display is on; this mirrors
/// `jq -e 'any(.[]; .dpmsStatus)'` closely enough for hyprctl's own output,
/// which never nests that key inside a string value.
#[must_use]
pub fn any_monitor_on(monitors_json: &str) -> bool {
    const KEY: &str = "\"dpmsStatus\"";
    monitors_json.match_indices(KEY).any(|(idx, _)| {
        let after = monitors_json[idx + KEY.len()..].trim_start();
        let after = after.strip_prefix(':').unwrap_or(after).trim_start();
        after.starts_with("true")
    })
}

/// Ensures the display power state matches `want`, toggling only when it
/// currently differs, and confirming once after an unconfirmed toggle.
///
/// `current`, `toggle` and `wait` are injected so the decision logic can be
/// tested without a compositor: `current` reports the on/off state, `toggle`
/// flips it, `wait` pauses for the compositor to settle.
pub fn ensure(
    want: bool,
    mut current: impl FnMut() -> bool,
    mut toggle: impl FnMut(),
    settle: impl FnOnce(),
) {
    if current() == want {
        return;
    }
    toggle();
    settle();
    if current() != want {
        toggle();
    }
}

fn hyprctl_current() -> bool {
    let Ok(output) = Command::new("hyprctl").args(["monitors", "-j"]).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let Ok(json) = String::from_utf8(output.stdout) else {
        return false;
    };
    any_monitor_on(&json)
}

fn hyprctl_toggle() {
    let _ = Command::new("hyprctl")
        .args(["dispatch", "hl.dsp.dpms({})"])
        .output();
}

/// Runs the `screen-power` subcommand: `on`, `off`, `toggle`, `status`, or no
/// argument at all, which behaves as `status`.
#[must_use]
pub fn run(action: Option<&str>) -> ExitCode {
    match action.unwrap_or("status") {
        "status" => {
            println!("{}", if hyprctl_current() { "on" } else { "off" });
            ExitCode::SUCCESS
        }
        "toggle" => {
            hyprctl_toggle();
            ExitCode::SUCCESS
        }
        "on" => {
            ensure(true, hyprctl_current, hyprctl_toggle, || {
                std::thread::sleep(CONFIRM_DELAY);
            });
            ExitCode::SUCCESS
        }
        "off" => {
            ensure(false, hyprctl_current, hyprctl_toggle, || {
                std::thread::sleep(CONFIRM_DELAY);
            });
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: scorched screen-power on|off|toggle|status");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{any_monitor_on, ensure};
    use std::cell::Cell;

    #[test]
    fn one_monitor_on_is_detected() {
        let json = r#"[{"name":"eDP-1","dpmsStatus":true}]"#;
        assert!(any_monitor_on(json));
    }

    #[test]
    fn all_monitors_off_is_detected() {
        let json = r#"[{"name":"eDP-1","dpmsStatus":false}]"#;
        assert!(!any_monitor_on(json));
    }

    #[test]
    fn one_of_several_monitors_on_counts_as_on() {
        let json = r#"[{"name":"eDP-1","dpmsStatus":false},{"name":"DP-1","dpmsStatus":true}]"#;
        assert!(any_monitor_on(json));
    }

    #[test]
    fn an_empty_monitor_list_is_off() {
        assert!(!any_monitor_on("[]"));
    }

    #[test]
    fn already_matching_state_never_toggles() {
        let toggles = Cell::new(0);
        ensure(true, || true, || toggles.set(toggles.get() + 1), || {});
        assert_eq!(toggles.get(), 0);
    }

    #[test]
    fn a_confirmed_toggle_stops_after_one_attempt() {
        let toggles = Cell::new(0);
        let state = Cell::new(false);
        ensure(
            true,
            || state.get(),
            || {
                toggles.set(toggles.get() + 1);
                state.set(true);
            },
            || {},
        );
        assert_eq!(toggles.get(), 1);
    }

    #[test]
    fn an_unconfirmed_toggle_is_retried_once() {
        let toggles = Cell::new(0);
        // The compositor never reports the change taking effect, so `ensure`
        // must try exactly twice, then give up rather than loop forever.
        ensure(true, || false, || toggles.set(toggles.get() + 1), || {});
        assert_eq!(toggles.get(), 2);
    }
}
