//! Acting on a device's screen as a person would (`adb_input_send`): taps, held presses, swipes,
//! typed text, keys and waits, sent in order with `input`.
//!
//! Every step is checked before the first is sent, so a bad step changes nothing. It changes the
//! device, so it asks first; it watches and acts from outside, as a person does, so it runs in
//! Clean room. A step the device refuses stops the call there, and the error says how many steps
//! had already run.

use super::adb_tools::{adb_for, dev_ev, ensure_online, serial_of};
use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::android_input::{self, Command, Step};
use craft_anything_evidence::{Coverage, ErrorCode, Limitation, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub const INPUT_TOOL: &str = "adb_input_send";

/// How long one step may take on the device (a long swipe included).
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        INPUT_TOOL,
        "Tap, type and swipe",
        "Act on a device's screen as a person would, with `input`: a list of up to 50 steps run in order, each one of tap {x, y}, long_press {x, y, duration_ms}, swipe {x1, y1, x2, y2, duration_ms}, text {text} (printable ASCII, typed into the focused field), key {key} (back, home, enter, tab, delete, escape, menu, app_switch, search, up, down, left, right, center, page_up, page_down, move_home, move_end, volume_up, volume_down, wakeup) or wait {ms}. Coordinates are screen pixels, as adb_ui_show's bounds give them. Every step is checked before any is sent. Changes the device.",
        "Tap, type or swipe on the phone's screen.",
        Toolset::Adb,
        Effects::MUTATES_SESSION | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK | Effects::MUTATES_DEVICE,
        Requires::Engine("adb"),
        schema_of::<InputParams>,
        adb_input_send,
    )]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InputParams {
    /// Device serial from adb_devices_find (e.g. emulator-5554).
    serial: String,
    /// The steps, in order.
    steps: Vec<Step>,
}

/// What a step looks like in the record: typed text is counted, never repeated, since it may be a
/// password.
fn shown(step: &Step) -> Value {
    match step {
        Step::Text { text } => json!({"action": "text", "characters": text.chars().count()}),
        other => serde_json::to_value(other).unwrap_or(Value::Null),
    }
}

/// Whether what `input` printed says it failed. It prints nothing when a step works.
fn refused(said: &str) -> Option<String> {
    let line = said.lines().map(str::trim).find(|l| !l.is_empty())?;
    let failed = ["Error", "Exception", "error:", "Usage:", "Unknown"].iter().any(|w| line.contains(w));
    failed.then(|| line.chars().take(300).collect())
}

fn adb_input_send(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: InputParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let commands = android_input::commands(&p.steps)?;
    let adb = adb_for(ctx)?.timeout(STEP_TIMEOUT).idle_timeout(STEP_TIMEOUT);
    ensure_online(&adb, serial)?;
    let total = commands.len();
    for (i, command) in commands.iter().enumerate() {
        ctx.cancel.check().map_err(|e| e.with_details(json!({"steps_run": i})))?;
        ctx.report(Some(i as f32 / total as f32), format!("Step {} of {total}", i.saturating_add(1)));
        match command {
            Command::Pause(ms) => std::thread::sleep(Duration::from_millis(u64::from(*ms))),
            Command::Shell(line) => {
                let out = adb.shell(serial, line).map_err(|e| e.with_details(json!({"steps_run": i})))?;
                let said = format!("{}{}", out.stdout_text(), out.stderr_text());
                if let Some(why) = refused(&said).or_else(|| (!out.success).then(|| format!("exit code {:?}", out.code))) {
                    return Err(ToolError::new(ErrorCode::ProgramFailed, format!("Step {} was refused by the device: {why}", i.saturating_add(1)))
                        .with_remediation(format!("{i} step(s) before it ran and changed the device; check the screen (adb_ui_show) before sending the rest."))
                        .with_details(json!({"steps_run": i})));
                }
            }
        }
    }
    let steps: Vec<Value> = p.steps.iter().map(shown).collect();
    let e = dev_ev(ctx, INPUT_TOOL, serial, json!({"serial": serial, "steps": steps}))
        .result(json!({"steps_run": total, "steps": steps}))
        .summary(format!("Sent {total} step(s) to {serial}."))
        .limitation(Limitation::new("device_changed", "This acted on the device's screen."))
        .limitation(Limitation::new(
            "effect_not_checked",
            "The device took every step, but what each one did on screen was not checked; read the screen with adb_ui_show or adb_screenshot_take.",
        ))
        .coverage(Coverage::complete(total as u64))
        .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native_common::test_support::call_err;
    use crate::{CallOptions, Caller, Consent, Engine};

    fn engine_with_stand_in_adb() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb.exe");
        std::fs::write(&adb, b"stand-in").unwrap();
        let mut s = crate::testing::session();
        s.settings.engines.tools.adb = Some(adb);
        (Engine::new(s), dir)
    }

    fn consented(e: &Engine, params: Value) -> ToolError {
        e.call(INPUT_TOOL, params, CallOptions::new(Caller::Mcp).with_consent(Consent::Granted)).unwrap_err()
    }

    #[test]
    fn it_asks_before_changing_the_device() {
        let (e, _d) = engine_with_stand_in_adb();
        let err = call_err(&e, INPUT_TOOL, json!({"serial": "emulator-5554", "steps": [{"action": "tap", "x": 1, "y": 1}]}));
        assert_eq!(err.code, ErrorCode::ConsentRequired);
    }

    #[test]
    fn bad_steps_are_refused_before_the_device_is_asked() {
        let (e, _d) = engine_with_stand_in_adb();
        for steps in [
            json!([]),
            json!([{"action": "tap", "x": 1, "y": 1}, {"action": "key", "key": "power"}]),
            json!([{"action": "text", "text": "olá"}]),
            json!([{"action": "shell", "line": "reboot"}]),
            json!([{"action": "wait", "ms": 999999}]),
        ] {
            assert_eq!(consented(&e, json!({"serial": "emulator-5554", "steps": steps.clone()})).code, ErrorCode::BadInput, "{steps}");
        }
        assert_eq!(consented(&e, json!({"serial": "a b", "steps": [{"action": "tap", "x": 1, "y": 1}]})).code, ErrorCode::BadInput);
    }

    #[test]
    fn it_needs_adb() {
        let e = Engine::new(crate::testing::session());
        assert_eq!(consented(&e, json!({"serial": "emulator-5554", "steps": [{"action": "tap", "x": 1, "y": 1}]})).code, ErrorCode::EngineMissing);
    }

    #[test]
    fn typed_text_is_never_repeated_in_the_record() {
        let v = shown(&Step::Text { text: "hunter2 secret".into() });
        assert_eq!(v, json!({"action": "text", "characters": 14}));
        assert_eq!(shown(&Step::Tap { x: 5, y: 6 }), json!({"action": "tap", "x": 5, "y": 6}));
    }

    #[test]
    fn a_refusal_is_read_from_what_input_printed() {
        assert_eq!(refused(""), None);
        assert_eq!(refused("\n  \n"), None);
        assert!(refused("Error: Unknown command: tapx").is_some());
        assert!(refused("java.lang.SecurityException: Injecting to another application requires INJECT_EVENTS permission").is_some());
        assert!(refused("Usage: input [<source>] <command> [<arg>...]").is_some());
    }
}
