//! Recording a walkthrough of an Android app (`adb_walkthrough_record`): a list of steps sent to
//! the device, with a picture of the screen and its controls saved before the first step and after
//! each one, and a `walkthrough.json` that says what was done and what came of it, so the same
//! walk can be compared later (against a rebuild, or another device).
//!
//! The folder holds, for capture `NN`: `step-NN.png` (the screen) and `step-NN.json` (its controls,
//! as `adb_ui_show` reads them, typed text left out). `walkthrough.json` is versioned
//! ([`FORMAT`], [`VERSION`]).
//!
//! Typed text is kept in the file so the walk can be repeated, except a step marked `secret` and
//! any text typed while the focused field looked like a password (marked as one, or named or hinted
//! as a password, PIN or code): those are kept out, with only their length.

use super::adb_tools::{adb_for, dev_ev, ensure_online, screencap, serial_of};
use super::adb_ui_tools::{DUMP_TIMEOUT, read_screen};
use super::artifact_tools::{UnlessWritten, create_absent_dir};
use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::adb::Adb;
use craft_anything_engines::android_input::{self, Command, Step};
use craft_anything_engines::android_ui::{UiNode, UiTree};
use craft_anything_evidence::{Coverage, CoverageStatus, ErrorCode, Limitation, Location, ToolError};
use craft_anything_platform::fs::write_atomic;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

pub const WALKTHROUGH_TOOL: &str = "adb_walkthrough_record";
/// What `walkthrough.json` says it is.
pub const FORMAT: &str = "craft-anything-android-walkthrough";
/// Its version; a change to its shape bumps this and adds a migration step.
pub const VERSION: u32 = 1;

/// How long the screen is given to settle after a step unless told otherwise.
const DEFAULT_SETTLE_MS: u32 = 1000;
/// How long one step may take on the device.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest walkthrough name.
const MAX_NAME: usize = 120;
/// Words that mark a field as one for a secret, in its id, hint or description.
const SECRET_WORDS: &[&str] = &["pass", "pwd", "pin", "secret", "otp", "cvv", "cvc", "security code", "token", "senha", "contraseña"];

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        WALKTHROUGH_TOOL,
        "Record a walkthrough",
        "Send a list of steps to a device (the same steps as adb_input_send: tap, long_press, swipe, text, key, wait) and save, before the first step and after each one but a wait, a picture of the screen (step-NN.png) and its controls (step-NN.json, as adb_ui_show reads them), with walkthrough.json saying each step and what came of it. output_directory must be absolute and must not exist yet. settle_ms (default 1000, at most 10000) is how long the screen is given after each step. Typed text is kept in walkthrough.json so the walk can be repeated, except a text step marked secret and any text typed into a field that looks like a password. A step the device refuses ends the walk there; what was recorded so far is kept and says where it stopped. Changes the device.",
        "Go through an app step by step and save a picture of every screen on the way.",
        Toolset::Adb,
        Effects::MUTATES_SESSION
            | Effects::LAUNCHES_PROCESS
            | Effects::ACCESSES_NETWORK
            | Effects::MUTATES_DEVICE
            | Effects::WRITES_FILESYSTEM
            | Effects::CARRIES_TARGET_CONTENT,
        Requires::Engine("adb"),
        schema_of::<WalkParams>,
        adb_walkthrough_record,
    )]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WalkParams {
    /// Device serial from adb_devices_find (e.g. emulator-5554).
    serial: String,
    /// Absolute host folder that must not exist yet; it is created fresh.
    output_directory: String,
    /// The steps, in order.
    steps: Vec<Step>,
    /// A name for the walk ("sign in", "add to cart").
    #[serde(default)]
    name: Option<String>,
    /// How long the screen is given to settle after each step, in milliseconds (default 1000).
    #[serde(default)]
    settle_ms: Option<u32>,
}

/// Whether a field looks like one for a secret: marked as a password, or named, hinted or
/// described as one. Errs towards yes.
fn looks_secret(n: &UiNode) -> bool {
    n.password
        || [&n.resource_id, &n.hint, &n.content_desc].iter().any(|v| {
            let v = v.to_lowercase();
            SECRET_WORDS.iter().any(|w| v.contains(w))
        })
}

/// The field that has the keyboard on a screen, if one does.
fn focused(tree: &UiTree) -> Option<&UiNode> {
    tree.nodes.iter().find(|n| n.focused)
}

/// A step as the file keeps it. `secret` is whether its text is kept out.
fn recorded(step: &Step, secret: bool) -> Value {
    match step {
        Step::Text { text, .. } => json!({
            "action": "text",
            "text": if secret { None } else { Some(text) },
            "secret": secret,
            "characters": text.chars().count(),
        }),
        other => serde_json::to_value(other).unwrap_or(Value::Null),
    }
}

/// What one capture saved, and anything it couldn't.
fn capture(adb: &Adb, serial: &str, dir: &Path, index: usize, step: Value, tree_out: &mut Option<UiTree>) -> Result<Value, ToolError> {
    let stem = format!("step-{index:02}");
    let mut problems = Vec::new();
    let picture = match screencap(adb, serial) {
        Ok(png) => {
            let file = format!("{stem}.png");
            write_atomic(&dir.join(&file), &png)?;
            Some(file)
        }
        Err(e) => {
            problems.push(format!("picture: {}", e.message));
            None
        }
    };
    let (controls, package, summary) = match read_screen(adb, serial) {
        Ok(tree) => {
            let file = format!("{stem}.json");
            let text = serde_json::to_vec_pretty(&tree).map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
            write_atomic(&dir.join(&file), &text)?;
            let package = tree.nodes.iter().map(|n| n.package.clone()).find(|p| !p.is_empty());
            let summary = json!({
                "controls": tree.nodes.len(),
                "interactive": tree.nodes.iter().filter(|n| n.interactive()).count(),
                "focused": focused(&tree).map(|n| if n.label.is_empty() { n.resource_id.clone() } else { n.label.clone() }),
            });
            *tree_out = Some(tree);
            (Some(file), package, summary)
        }
        Err(e) => {
            problems.push(format!("controls: {}", e.message));
            *tree_out = None;
            (None, None, Value::Null)
        }
    };
    Ok(json!({
        "index": index,
        "step": step,
        "screenshot": picture,
        "controls": controls,
        "package": package,
        "screen": summary,
        "problems": problems,
    }))
}

fn manifest(name: Option<&str>, serial: &str, settle_ms: u32, captures: &[Value], stopped: Option<&Value>) -> Value {
    json!({
        "format": FORMAT,
        "version": VERSION,
        "name": name,
        "serial": serial,
        "settle_ms": settle_ms,
        "captures": captures,
        "complete": stopped.is_none(),
        "stopped": stopped,
    })
}

fn adb_walkthrough_record(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: WalkParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let commands = android_input::commands(&p.steps)?;
    let settle_ms = p.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);
    if settle_ms > android_input::MAX_WAIT_MS {
        return Err(ToolError::invalid(format!("settle_ms can be at most {}.", android_input::MAX_WAIT_MS)));
    }
    if p.name.as_ref().is_some_and(|n| n.len() > MAX_NAME || n.chars().any(char::is_control)) {
        return Err(ToolError::invalid(format!("name must be one line of at most {MAX_NAME} bytes.")));
    }
    let adb = adb_for(ctx)?.timeout(STEP_TIMEOUT.max(DUMP_TIMEOUT)).idle_timeout(STEP_TIMEOUT.max(DUMP_TIMEOUT));
    ensure_online(&adb, serial)?;
    let dir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&dir);
    let settle = Duration::from_millis(u64::from(settle_ms));

    let mut tree: Option<UiTree> = None;
    let mut captures = vec![capture(&adb, serial, &dir, 0, Value::Null, &mut tree)?];
    let mut stopped: Option<Value> = None;
    let mut secrets = 0usize;
    let total = commands.len();
    for (i, (step, command)) in p.steps.iter().zip(&commands).enumerate() {
        if ctx.cancel.is_cancelled() {
            stopped = Some(json!({"before_step": i.saturating_add(1), "reason": "stopped by the person"}));
            break;
        }
        ctx.report(Some(i as f32 / total as f32), format!("Step {} of {total}", i.saturating_add(1)));
        // Text typed into a field that looks like a password is kept out, marked or not.
        let secret =
            matches!(step, Step::Text { secret: true, .. }) || (matches!(step, Step::Text { .. }) && tree.as_ref().and_then(focused).is_some_and(looks_secret));
        secrets = secrets.saturating_add(usize::from(secret));
        match command {
            Command::Pause(ms) => {
                std::thread::sleep(Duration::from_millis(u64::from(*ms)));
                continue;
            }
            Command::Shell(line) => {
                let refused = match adb.shell(serial, line) {
                    Ok(out) => {
                        let said = format!("{}{}", out.stdout_text(), out.stderr_text());
                        let line = said.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
                        let bad = ["Error", "Exception", "error:", "Usage:", "Unknown"].iter().any(|w| line.contains(w));
                        (bad || !out.success).then(|| if line.is_empty() { format!("exit code {:?}", out.code) } else { line.chars().take(300).collect() })
                    }
                    Err(e) => Some(e.message),
                };
                if let Some(why) = refused {
                    stopped = Some(json!({"at_step": i.saturating_add(1), "reason": why}));
                    break;
                }
            }
        }
        std::thread::sleep(settle);
        captures.push(capture(&adb, serial, &dir, captures.len(), recorded(step, secret), &mut tree)?);
    }

    let file = dir.join("walkthrough.json");
    let doc = manifest(p.name.as_deref(), serial, settle_ms, &captures, stopped.as_ref());
    let text = serde_json::to_vec_pretty(&doc).map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    write_atomic(&file, &text)?;
    made.keep();

    let problems: usize = captures.iter().map(|c| c["problems"].as_array().map_or(0, Vec::len)).sum();
    let mut lims = vec![Limitation::new("device_changed", "This acted on the device's screen.")];
    if let Some(s) = &stopped {
        lims.push(Limitation::new("stopped_early", format!("The walk ended early: {}", s["reason"].as_str().unwrap_or("unknown"))));
    }
    if problems > 0 {
        lims.push(Limitation::new("captures_missing", format!("{problems} picture(s) or control list(s) could not be taken; each capture says which.")));
    }
    if secrets > 0 {
        lims.push(Limitation::new(
            "secret_text_not_kept",
            format!("{secrets} typed text(s) looked secret and were not saved; repeating the walk needs them again."),
        ));
    }
    let summary = match &stopped {
        None => format!("Recorded {} capture(s) over {total} step(s) to {}.", captures.len(), dir.display()),
        Some(_) => format!("Recorded {} capture(s) before the walk ended early; saved to {}.", captures.len(), dir.display()),
    };
    let mut coverage = Coverage::complete(captures.len() as u64);
    if stopped.is_some() {
        coverage.status = CoverageStatus::Partial;
    }
    let steps_for_record: Vec<Value> = captures.iter().map(|c| c["step"].clone()).collect();
    let e = dev_ev(
        ctx,
        WALKTHROUGH_TOOL,
        serial,
        json!({"serial": serial, "output_directory": dir, "name": p.name, "settle_ms": settle_ms, "steps": p.steps.len()}),
    )
    .result(json!({"file": file, "captures": captures.len(), "complete": stopped.is_none(), "stopped": stopped, "steps": steps_for_record}))
    .summary(summary)
    .limitations(lims)
    .location(Location::new("path", file.to_string_lossy()))
    .coverage(coverage)
    .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native_common::test_support::call_err;
    use crate::{CallOptions, Caller, Consent, Engine};
    use craft_anything_engines::android_ui;

    fn screen_with_focus(attrs: &str) -> UiTree {
        android_ui::parse(&format!("<hierarchy><node class=\"android.widget.EditText\" focused=\"true\" focusable=\"true\" {attrs} /></hierarchy>")).unwrap()
    }

    #[test]
    fn a_field_that_looks_secret_is_told_apart() {
        for attrs in [
            "password=\"true\"",
            "resource-id=\"app:id/password\"",
            "hint=\"PIN\"",
            "content-desc=\"Security code\"",
            "resource-id=\"app:id/otp_input\"",
            "hint=\"Senha\"",
        ] {
            let t = screen_with_focus(attrs);
            assert!(focused(&t).is_some_and(looks_secret), "{attrs}");
        }
        for attrs in ["resource-id=\"app:id/email\"", "hint=\"Search\"", ""] {
            let t = screen_with_focus(attrs);
            assert!(!focused(&t).is_some_and(looks_secret), "{attrs}");
        }
    }

    #[test]
    fn secret_text_is_kept_out_of_the_file_and_other_text_kept_in() {
        let kept = recorded(&Step::Text { text: "wi fi".into(), secret: false }, false);
        assert_eq!(kept, json!({"action": "text", "text": "wi fi", "secret": false, "characters": 5}));
        let out = recorded(&Step::Text { text: "Hunter2".into(), secret: false }, true);
        assert_eq!(out, json!({"action": "text", "text": null, "secret": true, "characters": 7}));
        assert_eq!(recorded(&Step::Key { key: "back".into() }, false), json!({"action": "key", "key": "back"}));
    }

    #[test]
    fn the_file_says_what_it_is_and_whether_it_finished() {
        let done = manifest(Some("sign in"), "emulator-5554", 1000, &[json!({"index": 0})], None);
        assert_eq!((done["format"].as_str(), done["version"].as_u64(), done["complete"].as_bool()), (Some(FORMAT), Some(1), Some(true)));
        let cut = manifest(None, "emulator-5554", 1000, &[], Some(&json!({"at_step": 2, "reason": "Error: x"})));
        assert_eq!((cut["complete"].as_bool(), cut["stopped"]["at_step"].as_u64()), (Some(false), Some(2)));
    }

    fn engine_with_stand_in_adb() -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb.exe");
        std::fs::write(&adb, b"stand-in").unwrap();
        let mut s = crate::testing::session();
        s.settings.engines.tools.adb = Some(adb);
        (Engine::new(s), dir)
    }

    #[test]
    fn it_asks_first_and_refuses_bad_input_before_touching_anything() {
        let (e, d) = engine_with_stand_in_adb();
        let out = d.path().join("walk");
        let base = json!({"serial": "emulator-5554", "output_directory": out, "steps": [{"action": "key", "key": "back"}]});
        assert_eq!(call_err(&e, WALKTHROUGH_TOOL, base.clone()).code, ErrorCode::ConsentRequired);
        let ok = |patch: Value| {
            let mut v = base.clone();
            if let (Value::Object(m), Value::Object(p)) = (&mut v, patch) {
                m.extend(p);
            }
            e.call(WALKTHROUGH_TOOL, v, CallOptions::new(Caller::Mcp).with_consent(Consent::Granted)).unwrap_err().code
        };
        assert_eq!(ok(json!({"steps": []})), ErrorCode::BadInput);
        assert_eq!(ok(json!({"steps": [{"action": "key", "key": "power"}]})), ErrorCode::BadInput);
        assert_eq!(ok(json!({"settle_ms": 999_999})), ErrorCode::BadInput);
        assert_eq!(ok(json!({"name": "two\nlines"})), ErrorCode::BadInput);
        assert_eq!(ok(json!({"serial": "a b"})), ErrorCode::BadInput);
        assert!(!out.exists(), "nothing is created before the call can run");
    }

    #[test]
    fn it_needs_adb() {
        let e = Engine::new(crate::testing::session());
        let d = tempfile::tempdir().unwrap();
        let err = e
            .call(
                WALKTHROUGH_TOOL,
                json!({"serial": "emulator-5554", "output_directory": d.path().join("w"), "steps": [{"action": "key", "key": "back"}]}),
                CallOptions::new(Caller::Mcp).with_consent(Consent::Granted),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::EngineMissing);
    }
}
