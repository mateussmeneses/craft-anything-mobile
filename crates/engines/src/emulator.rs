//! The Android emulator: which program starts a virtual device, with which arguments, and how a
//! running one names itself.
//!
//! Nothing here starts anything: the engine's tools do, through `craft_anything_platform::process`.
//! A virtual device's name is checked against a strict pattern and passed as its own argv item,
//! never through a shell.

use crate::android_sdk::Sdk;
use craft_anything_evidence::ToolError;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

/// Longest virtual device name accepted.
pub const MAX_NAME: usize = 128;

/// How to start a virtual device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartOptions {
    /// Run without a window (the screen is still there for screenshots).
    pub no_window: bool,
    /// Boot from scratch instead of from the saved quick-boot state.
    pub cold_boot: bool,
}

/// A virtual device's name as the emulator accepts it: letters, digits, `.`, `_` and `-`.
pub fn validate_avd_name(s: &str) -> Result<&str, ToolError> {
    let ok = !s.is_empty() && s.len() <= MAX_NAME && !s.starts_with('-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(s)
    } else {
        Err(ToolError::invalid(format!("{s:?} is not a virtual device name."))
            .with_remediation("Use a name adb_emulators_list shows: letters, digits, '.', '_' and '-' only."))
    }
}

/// The emulator program of an SDK, when the SDK has one on disk.
pub fn program(sdk: &Sdk) -> Option<PathBuf> {
    let dir = &sdk.emulator.as_ref()?.path;
    let exe = dir.join(if cfg!(windows) { "emulator.exe" } else { "emulator" });
    exe.is_file().then_some(exe)
}

/// The arguments that start `name`. `name` must have passed [`validate_avd_name`].
pub fn start_args(name: &str, options: StartOptions) -> Vec<String> {
    let mut args = vec!["-avd".to_string(), name.to_string()];
    if options.no_window {
        args.push("-no-window".into());
    }
    if options.cold_boot {
        args.push("-no-snapshot-load".into());
    }
    args
}

/// Whether an adb serial is an emulator's (`emulator-5554`).
pub fn is_emulator_serial(serial: &str) -> bool {
    serial.strip_prefix("emulator-").is_some_and(|port| !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()))
}

/// The virtual device's name from what `adb emu avd name` printed: the name on one line, then
/// `OK`. `None` when the console answered anything else (an error, nothing).
pub fn parse_avd_name(text: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty()).take(8);
    let name = lines.next()?;
    let ok = lines.next()? == "OK";
    (ok && validate_avd_name(name).is_ok()).then(|| name.to_string())
}

/// Whether `getprop sys.boot_completed` says the device has finished starting.
pub fn boot_completed(getprop_output: &str) -> bool {
    getprop_output.trim() == "1"
}

/// How long a started emulator may stay `offline` in adb before the wait gives up on it. A healthy
/// boot lists the device as offline for seconds; one resumed from a broken quick-boot snapshot
/// stayed offline for ten minutes (a cold boot of the same device took 26 s).
pub const OFFLINE_LIMIT: Duration = Duration::from_secs(90);

/// The least a cold-boot retry is given to boot, however little of the caller's wait was left.
pub const COLD_RETRY_MIN: Duration = Duration::from_secs(120);

/// Where a device being waited for stands, as far as adb says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootPhase {
    /// adb has never listed it.
    NeverListed,
    /// Listed, but not online (`offline` and the like): adb can't talk to it.
    Offline,
    /// Online, and `sys.boot_completed` isn't `1` yet (or it couldn't be asked).
    Booting,
    /// Online and finished booting.
    Booted,
}

/// The phase of one look at the device: `None` when adb doesn't list it, otherwise whether it is
/// online and what `sys.boot_completed` said.
pub fn boot_phase(listed: Option<(bool, Option<bool>)>) -> BootPhase {
    match listed {
        None => BootPhase::NeverListed,
        Some((false, _)) => BootPhase::Offline,
        Some((true, Some(true))) => BootPhase::Booted,
        Some((true, _)) => BootPhase::Booting,
    }
}

/// What the wait does after a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Continue,
    /// It finished booting.
    Booted,
    /// The wait ran out; `phase` is where the device was.
    TimedOut(BootPhase),
    /// It stayed offline for [`OFFLINE_LIMIT`]: more waiting won't help.
    StuckOffline,
}

/// Decides, from looks at a starting device, when to stop waiting. Time is given, never read, so a
/// test needs no device and no clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct BootWatch {
    offline_since: Option<Duration>,
}

impl BootWatch {
    /// The verdict after a look at `at` (time since the start) when the device was in `phase`; the
    /// whole wait is `wait`.
    pub fn look(&mut self, at: Duration, phase: BootPhase, wait: Duration) -> Verdict {
        if phase == BootPhase::Offline {
            let since = *self.offline_since.get_or_insert(at);
            if at.saturating_sub(since) >= OFFLINE_LIMIT {
                return Verdict::StuckOffline;
            }
        } else {
            self.offline_since = None;
        }
        match phase {
            BootPhase::Booted => Verdict::Booted,
            _ if at >= wait => Verdict::TimedOut(phase),
            _ => Verdict::Continue,
        }
    }
}

/// The time a cold-boot retry gets after `elapsed` of a `wait`: what is left, but not less than
/// [`COLD_RETRY_MIN`].
pub fn cold_retry_budget(wait: Duration, elapsed: Duration) -> Duration {
    wait.saturating_sub(elapsed).max(COLD_RETRY_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_sdk::{self, RootSource};

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn the_phase_tells_never_listed_from_offline_from_booting() {
        assert_eq!(boot_phase(None), BootPhase::NeverListed);
        assert_eq!(boot_phase(Some((false, None))), BootPhase::Offline);
        assert_eq!(boot_phase(Some((false, Some(true)))), BootPhase::Offline, "an offline device can't be booted");
        assert_eq!(boot_phase(Some((true, None))), BootPhase::Booting);
        assert_eq!(boot_phase(Some((true, Some(false)))), BootPhase::Booting);
        assert_eq!(boot_phase(Some((true, Some(true)))), BootPhase::Booted);
    }

    #[test]
    fn a_device_that_stays_offline_is_given_up_on_long_before_the_wait_ends() {
        let mut w = BootWatch::default();
        assert_eq!(w.look(secs(2), BootPhase::Offline, secs(600)), Verdict::Continue);
        assert_eq!(w.look(secs(2) + OFFLINE_LIMIT - secs(1), BootPhase::Offline, secs(600)), Verdict::Continue);
        assert_eq!(w.look(secs(2) + OFFLINE_LIMIT, BootPhase::Offline, secs(600)), Verdict::StuckOffline);
    }

    #[test]
    fn offline_time_starts_over_when_the_device_comes_online_or_goes_missing() {
        let mut w = BootWatch::default();
        assert_eq!(w.look(secs(0), BootPhase::Offline, secs(600)), Verdict::Continue);
        assert_eq!(w.look(secs(80), BootPhase::Booting, secs(600)), Verdict::Continue);
        assert_eq!(w.look(secs(100), BootPhase::Offline, secs(600)), Verdict::Continue, "20 s of offline, not 100");
        assert_eq!(w.look(secs(150), BootPhase::NeverListed, secs(600)), Verdict::Continue);
        assert_eq!(w.look(secs(200), BootPhase::Offline, secs(600)), Verdict::Continue);
    }

    #[test]
    fn the_wait_ends_booted_or_timed_out_with_the_phase_it_reached() {
        let mut w = BootWatch::default();
        assert_eq!(w.look(secs(5), BootPhase::Booted, secs(240)), Verdict::Booted);
        assert_eq!(BootWatch::default().look(secs(240), BootPhase::NeverListed, secs(240)), Verdict::TimedOut(BootPhase::NeverListed));
        assert_eq!(BootWatch::default().look(secs(240), BootPhase::Booting, secs(240)), Verdict::TimedOut(BootPhase::Booting));
        assert_eq!(BootWatch::default().look(secs(239), BootPhase::Booting, secs(240)), Verdict::Continue);
        // A short wait ends before the offline limit does.
        assert_eq!(BootWatch::default().look(secs(30), BootPhase::Offline, secs(30)), Verdict::TimedOut(BootPhase::Offline));
    }

    #[test]
    fn a_cold_retry_gets_what_is_left_but_never_too_little() {
        assert_eq!(cold_retry_budget(secs(600), secs(100)), secs(500));
        assert_eq!(cold_retry_budget(secs(240), secs(100)), secs(140));
        assert_eq!(cold_retry_budget(secs(240), secs(200)), COLD_RETRY_MIN);
        assert_eq!(cold_retry_budget(secs(60), secs(90)), COLD_RETRY_MIN);
    }

    #[test]
    fn names_that_could_change_a_command_line_are_refused() {
        for good in ["Medium_Phone", "Pixel-9.API_36", "a"] {
            assert!(validate_avd_name(good).is_ok(), "{good}");
        }
        let long = "a".repeat(MAX_NAME + 1);
        for bad in ["", "-avd", "-no-window", "a b", "a;b", "a\"b", "a/b", "a\\b", "é", "a\n", long.as_str()] {
            assert!(validate_avd_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn start_arguments_name_the_device_and_only_the_options_asked_for() {
        assert_eq!(start_args("P", StartOptions::default()), ["-avd", "P"]);
        assert_eq!(start_args("P", StartOptions { no_window: true, cold_boot: true }), ["-avd", "P", "-no-window", "-no-snapshot-load"]);
    }

    #[test]
    fn emulator_serials_are_told_from_phones() {
        assert!(is_emulator_serial("emulator-5554"));
        for other in ["emulator-", "emulator-55x4", "emulator-555555", "R58M12345", "192.168.0.5:5555", ""] {
            assert!(!is_emulator_serial(other), "{other}");
        }
    }

    #[test]
    fn the_console_answer_gives_a_name_only_when_it_says_ok() {
        assert_eq!(parse_avd_name("Medium_Phone\r\nOK\r\n").as_deref(), Some("Medium_Phone"));
        assert_eq!(parse_avd_name("\nP\nOK").as_deref(), Some("P"));
        for bad in ["", "OK", "KO: unknown command", "P\nKO", "a b\nOK", "P"] {
            assert_eq!(parse_avd_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn boot_is_complete_only_on_a_one() {
        assert!(boot_completed("1\r\n"));
        for no in ["", "0", "11", "error: device offline"] {
            assert!(!boot_completed(no), "{no:?}");
        }
    }

    #[test]
    fn the_program_is_found_only_when_the_file_is_there() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("emulator")).unwrap();
        assert_eq!(program(&android_sdk::read(t.path(), RootSource::Given)), None, "a folder without the program");
        let exe = t.path().join("emulator").join(if cfg!(windows) { "emulator.exe" } else { "emulator" });
        std::fs::write(&exe, b"stand-in").unwrap();
        assert_eq!(program(&android_sdk::read(t.path(), RootSource::Given)), Some(exe));
        let none = tempfile::tempdir().unwrap();
        assert_eq!(program(&android_sdk::read(none.path(), RootSource::Given)), None);
    }
}
