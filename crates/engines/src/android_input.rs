//! Input for an Android device: taps, typed text, keys and swipes, as `input` shell commands.
//!
//! Each step becomes one fixed command line. Numbers are bounded, keys come from a fixed table,
//! and typed text is limited to printable ASCII, quoted with [`crate::adb::quote_device`] for the
//! device shell, with spaces written the way `input text` reads them (`%s`). Nothing a caller
//! sends can add a second command.

use crate::adb::quote_device;
use craft_anything_evidence::ToolError;
use serde::{Deserialize, Serialize};

/// Largest coordinate accepted (screens are a few thousand pixels; this leaves room for any).
pub const MAX_COORD: u32 = 20_000;
/// Longest swipe or hold, in milliseconds.
pub const MAX_DURATION_MS: u32 = 10_000;
/// Longest wait step, in milliseconds.
pub const MAX_WAIT_MS: u32 = 10_000;
/// Longest text typed in one step.
pub const MAX_TEXT: usize = 500;
/// Most steps in one call.
pub const MAX_STEPS: usize = 50;

/// Keys a step may press, by the name callers use, with Android's key code name.
pub const KEYS: &[(&str, &str)] = &[
    ("back", "KEYCODE_BACK"),
    ("home", "KEYCODE_HOME"),
    ("enter", "KEYCODE_ENTER"),
    ("tab", "KEYCODE_TAB"),
    ("delete", "KEYCODE_DEL"),
    ("forward_delete", "KEYCODE_FORWARD_DEL"),
    ("escape", "KEYCODE_ESCAPE"),
    ("menu", "KEYCODE_MENU"),
    ("app_switch", "KEYCODE_APP_SWITCH"),
    ("search", "KEYCODE_SEARCH"),
    ("up", "KEYCODE_DPAD_UP"),
    ("down", "KEYCODE_DPAD_DOWN"),
    ("left", "KEYCODE_DPAD_LEFT"),
    ("right", "KEYCODE_DPAD_RIGHT"),
    ("center", "KEYCODE_DPAD_CENTER"),
    ("page_up", "KEYCODE_PAGE_UP"),
    ("page_down", "KEYCODE_PAGE_DOWN"),
    ("move_home", "KEYCODE_MOVE_HOME"),
    ("move_end", "KEYCODE_MOVE_END"),
    ("volume_up", "KEYCODE_VOLUME_UP"),
    ("volume_down", "KEYCODE_VOLUME_DOWN"),
    ("wakeup", "KEYCODE_WAKEUP"),
];

/// One thing to do on the device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Tap a point, in screen pixels (`bounds` from adb_ui_show).
    Tap { x: u32, y: u32 },
    /// Press and hold a point.
    LongPress {
        x: u32,
        y: u32,
        /// How long to hold, in milliseconds (default 800).
        #[serde(default)]
        duration_ms: Option<u32>,
    },
    /// Drag from one point to another.
    Swipe {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        /// How long the drag takes, in milliseconds (default 300).
        #[serde(default)]
        duration_ms: Option<u32>,
    },
    /// Type text into the focused field: printable ASCII only.
    Text { text: String },
    /// Press a key by name: back, home, enter, tab, delete, escape, menu, app_switch, up, down…
    Key { key: String },
    /// Wait before the next step, in milliseconds.
    Wait { ms: u32 },
}

/// What a step turns into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A line for the device shell.
    Shell(String),
    /// A pause on this side; nothing is sent.
    Pause(u32),
}

fn coord(v: u32, what: &str) -> Result<u32, ToolError> {
    if v <= MAX_COORD { Ok(v) } else { Err(ToolError::invalid(format!("{what} {v} is off any screen (at most {MAX_COORD})."))) }
}

fn duration(v: Option<u32>, default: u32) -> Result<u32, ToolError> {
    let d = v.unwrap_or(default);
    if (1..=MAX_DURATION_MS).contains(&d) { Ok(d) } else { Err(ToolError::invalid(format!("duration_ms must be between 1 and {MAX_DURATION_MS}."))) }
}

/// The key code for a key name.
pub fn key_code(name: &str) -> Option<&'static str> {
    KEYS.iter().find(|(k, _)| *k == name).map(|(_, code)| *code)
}

/// The command for one step, or why it is refused.
pub fn command(step: &Step) -> Result<Command, ToolError> {
    Ok(match step {
        Step::Tap { x, y } => Command::Shell(format!("input tap {} {}", coord(*x, "x")?, coord(*y, "y")?)),
        Step::LongPress { x, y, duration_ms } => {
            let (x, y) = (coord(*x, "x")?, coord(*y, "y")?);
            Command::Shell(format!("input swipe {x} {y} {x} {y} {}", duration(*duration_ms, 800)?))
        }
        Step::Swipe { x1, y1, x2, y2, duration_ms } => Command::Shell(format!(
            "input swipe {} {} {} {} {}",
            coord(*x1, "x1")?,
            coord(*y1, "y1")?,
            coord(*x2, "x2")?,
            coord(*y2, "y2")?,
            duration(*duration_ms, 300)?
        )),
        Step::Text { text } => Command::Shell(format!("input text {}", typed(text)?)),
        Step::Key { key } => {
            let code = key_code(key).ok_or_else(|| {
                let names: Vec<&str> = KEYS.iter().map(|(k, _)| *k).collect();
                ToolError::invalid(format!("{key:?} is not a key this tool presses. Keys: {}.", names.join(", ")))
            })?;
            Command::Shell(format!("input keyevent {code}"))
        }
        Step::Wait { ms } => {
            if *ms > MAX_WAIT_MS {
                return Err(ToolError::invalid(format!("A wait can be at most {MAX_WAIT_MS} ms.")));
            }
            Command::Pause(*ms)
        }
    })
}

/// Text as `input text` takes it, quoted for the device shell. `input text` reads `%s` as a
/// space, so a space is written that way, and text that itself holds `%s` is refused rather than
/// typed differently from what was asked.
fn typed(text: &str) -> Result<String, ToolError> {
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(ToolError::invalid(format!("text must be 1 to {MAX_TEXT} characters.")));
    }
    if !text.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return Err(ToolError::invalid("text can hold only printable ASCII (letters, digits, punctuation and spaces).")
            .with_remediation("Type other characters with the device's keyboard, or split the text around them."));
    }
    if text.contains("%s") {
        return Err(ToolError::invalid("text can't contain \"%s\": the device would type a space there."));
    }
    Ok(quote_device(&text.replace(' ', "%s")))
}

/// Every step's command, checked before any is sent: a bad step refuses the whole call.
pub fn commands(steps: &[Step]) -> Result<Vec<Command>, ToolError> {
    if steps.is_empty() || steps.len() > MAX_STEPS {
        return Err(ToolError::invalid(format!("Send 1 to {MAX_STEPS} steps.")));
    }
    steps.iter().enumerate().map(|(i, s)| command(s).map_err(|e| ToolError::invalid(format!("Step {}: {}", i.saturating_add(1), e.message)))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(step: Step) -> String {
        match command(&step).unwrap() {
            Command::Shell(s) => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn steps_become_fixed_input_commands() {
        assert_eq!(shell(Step::Tap { x: 540, y: 640 }), "input tap 540 640");
        assert_eq!(shell(Step::LongPress { x: 1, y: 2, duration_ms: None }), "input swipe 1 2 1 2 800");
        assert_eq!(shell(Step::Swipe { x1: 500, y1: 1800, x2: 500, y2: 600, duration_ms: Some(250) }), "input swipe 500 1800 500 600 250");
        assert_eq!(shell(Step::Key { key: "back".into() }), "input keyevent KEYCODE_BACK");
        assert_eq!(command(&Step::Wait { ms: 500 }).unwrap(), Command::Pause(500));
    }

    #[test]
    fn typed_text_is_quoted_and_spaces_are_written_for_input() {
        assert_eq!(shell(Step::Text { text: "hello world".into() }), "input text 'hello%sworld'");
        assert_eq!(shell(Step::Text { text: "it's; rm -rf / && $(x) `y`".into() }), r#"input text 'it'\''s;%srm%s-rf%s/%s&&%s$(x)%s`y`'"#);
        assert_eq!(shell(Step::Text { text: "100%".into() }), "input text '100%'");
    }

    #[test]
    fn text_that_would_be_typed_differently_is_refused() {
        for bad in ["", "é", "line\nbreak", "tab\there", "50%sale", "\u{7f}"] {
            assert!(command(&Step::Text { text: bad.into() }).is_err(), "{bad:?}");
        }
        assert!(command(&Step::Text { text: "a".repeat(MAX_TEXT + 1) }).is_err());
        assert!(command(&Step::Text { text: "a".repeat(MAX_TEXT) }).is_ok());
    }

    #[test]
    fn numbers_and_keys_out_of_range_are_refused() {
        assert!(command(&Step::Tap { x: MAX_COORD + 1, y: 0 }).is_err());
        assert!(command(&Step::Swipe { x1: 0, y1: 0, x2: 0, y2: 0, duration_ms: Some(0) }).is_err());
        assert!(command(&Step::Swipe { x1: 0, y1: 0, x2: 0, y2: 0, duration_ms: Some(MAX_DURATION_MS + 1) }).is_err());
        assert!(command(&Step::Wait { ms: MAX_WAIT_MS + 1 }).is_err());
        for bad in ["power", "KEYCODE_BACK", "Back", "back ", "26", ""] {
            assert!(command(&Step::Key { key: bad.into() }).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn one_bad_step_refuses_the_whole_list_and_names_it() {
        let steps = vec![Step::Tap { x: 1, y: 1 }, Step::Key { key: "nope".into() }];
        let err = commands(&steps).unwrap_err();
        assert!(err.message.starts_with("Step 2:"), "{}", err.message);
        assert!(commands(&[]).is_err());
        assert!(commands(&vec![Step::Wait { ms: 1 }; MAX_STEPS + 1]).is_err());
        assert_eq!(commands(&vec![Step::Wait { ms: 1 }; MAX_STEPS]).unwrap().len(), MAX_STEPS);
    }

    #[test]
    fn steps_are_read_from_json_strictly() {
        let s: Step = serde_json::from_str(r#"{"action":"tap","x":3,"y":4}"#).unwrap();
        assert_eq!(s, Step::Tap { x: 3, y: 4 });
        for bad in [r#"{"action":"tap","x":-1,"y":4}"#, r#"{"action":"tap","x":1,"y":4,"z":1}"#, r#"{"action":"shell","line":"reboot"}"#, r#"{"x":1,"y":2}"#] {
            assert!(serde_json::from_str::<Step>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn every_key_name_is_distinct_and_maps_to_a_key_code() {
        let mut names: Vec<&str> = KEYS.iter().map(|(k, _)| *k).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), KEYS.len());
        assert!(KEYS.iter().all(|(_, c)| c.starts_with("KEYCODE_") && c.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')));
    }
}
