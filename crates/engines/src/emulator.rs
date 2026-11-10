//! The Android emulator: which program starts a virtual device, with which arguments, and how a
//! running one names itself.
//!
//! Nothing here starts anything: the engine's tools do, through `craft_anything_platform::process`.
//! A virtual device's name is checked against a strict pattern and passed as its own argv item,
//! never through a shell.

use crate::android_sdk::Sdk;
use craft_anything_evidence::ToolError;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android_sdk::{self, RootSource};

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
