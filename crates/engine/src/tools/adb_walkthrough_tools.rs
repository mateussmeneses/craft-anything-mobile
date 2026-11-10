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
pub const REPLAY_TOOL: &str = "adb_walkthrough_replay";
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
    vec![
        spec(
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
        ),
        spec(
            REPLAY_TOOL,
            "Replay a walkthrough",
            "Run a walkthrough saved by adb_walkthrough_record again, on the same app, a rebuild of it or another device, saving new captures the same way, and say for each capture what changed from the recording: the app on screen, and the controls a person can act on that were added or removed (by class, id and label; at most 20 named each way), and whether the picture is byte for byte the same. walkthrough is the saved walkthrough.json (or its folder); texts it kept out are given in secrets, in the order they're typed, and are kept out of the new file too. output_directory must be absolute and must not exist yet. Changes the device.",
            "Do a recorded walk through an app again and see which screens came out different.",
            Toolset::Adb,
            Effects::MUTATES_SESSION
                | Effects::LAUNCHES_PROCESS
                | Effects::ACCESSES_NETWORK
                | Effects::MUTATES_DEVICE
                | Effects::WRITES_FILESYSTEM
                | Effects::CARRIES_TARGET_CONTENT,
            Requires::Engine("adb"),
            schema_of::<ReplayParams>,
            adb_walkthrough_replay,
        ),
    ]
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

fn manifest(name: Option<&str>, serial: &str, settle_ms: u32, walk: &Walk) -> Value {
    json!({
        "format": FORMAT,
        "version": VERSION,
        "name": name,
        "serial": serial,
        "settle_ms": settle_ms,
        "steps": walk.steps,
        "captures": walk.captures,
        "complete": walk.stopped.is_none(),
        "stopped": walk.stopped,
    })
}

/// What a walk on the device came to.
struct Walk {
    /// Every step sent (waits too), as the file keeps them.
    steps: Vec<Value>,
    captures: Vec<Value>,
    stopped: Option<Value>,
    secrets: usize,
}

/// Send `steps` one by one, capturing the screen before the first and after each but a wait.
/// A text step whose `keep_out` is true is kept out of the record whatever the screen shows.
#[allow(clippy::too_many_arguments)]
fn walk(
    ctx: &mut Ctx<'_>,
    adb: &Adb,
    serial: &str,
    dir: &Path,
    steps: &[Step],
    commands: &[Command],
    settle: Duration,
    keep_out: &[bool],
) -> Result<Walk, ToolError> {
    let mut tree: Option<UiTree> = None;
    let mut captures = vec![capture(adb, serial, dir, 0, Value::Null, &mut tree)?];
    let mut stopped: Option<Value> = None;
    let mut secrets = 0usize;
    let mut kept = Vec::new();
    let total = commands.len();
    for (i, (step, command)) in steps.iter().zip(commands).enumerate() {
        if ctx.cancel.is_cancelled() {
            stopped = Some(json!({"before_step": i.saturating_add(1), "reason": "stopped by the person"}));
            break;
        }
        ctx.report(Some(i as f32 / total as f32), format!("Step {} of {total}", i.saturating_add(1)));
        // Text typed into a field that looks like a password is kept out, marked or not.
        let secret = matches!(step, Step::Text { .. })
            && (matches!(step, Step::Text { secret: true, .. })
                || keep_out.get(i).copied().unwrap_or(false)
                || tree.as_ref().and_then(focused).is_some_and(looks_secret));
        secrets = secrets.saturating_add(usize::from(secret));
        kept.push(recorded(step, secret));
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
        captures.push(capture(adb, serial, dir, captures.len(), recorded(step, secret), &mut tree)?);
    }
    Ok(Walk { steps: kept, captures, stopped, secrets })
}

fn check_settle(settle_ms: u32) -> Result<Duration, ToolError> {
    if settle_ms > android_input::MAX_WAIT_MS {
        return Err(ToolError::invalid(format!("settle_ms can be at most {}.", android_input::MAX_WAIT_MS)));
    }
    Ok(Duration::from_millis(u64::from(settle_ms)))
}

fn write_manifest(dir: &Path, doc: &Value) -> Result<std::path::PathBuf, ToolError> {
    let file = dir.join("walkthrough.json");
    let text = serde_json::to_vec_pretty(doc).map_err(|e| ToolError::new(ErrorCode::Internal, e.to_string()))?;
    write_atomic(&file, &text)?;
    Ok(file)
}

fn adb_walkthrough_record(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: WalkParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let commands = android_input::commands(&p.steps)?;
    let settle_ms = p.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);
    let settle = check_settle(settle_ms)?;
    if p.name.as_ref().is_some_and(|n| n.len() > MAX_NAME || n.chars().any(char::is_control)) {
        return Err(ToolError::invalid(format!("name must be one line of at most {MAX_NAME} bytes.")));
    }
    let adb = adb_for(ctx)?.timeout(STEP_TIMEOUT.max(DUMP_TIMEOUT)).idle_timeout(STEP_TIMEOUT.max(DUMP_TIMEOUT));
    ensure_online(&adb, serial)?;
    let dir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&dir);

    let w = walk(ctx, &adb, serial, &dir, &p.steps, &commands, settle, &[])?;
    let file = write_manifest(&dir, &manifest(p.name.as_deref(), serial, settle_ms, &w))?;
    made.keep();
    let Walk { captures, stopped, secrets, .. } = w;

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
        None => format!("Recorded {} capture(s) over {} step(s) to {}.", captures.len(), p.steps.len(), dir.display()),
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReplayParams {
    /// Device serial from adb_devices_find (e.g. emulator-5554).
    serial: String,
    /// Absolute path of a walkthrough.json written by adb_walkthrough_record (or its folder).
    walkthrough: String,
    /// Absolute host folder that must not exist yet; the new captures go there.
    output_directory: String,
    /// The texts the walk kept out (secret ones), in the order they're typed.
    #[serde(default)]
    secrets: Vec<String>,
    /// How long the screen is given after each step, in milliseconds (default: the recording's).
    #[serde(default)]
    settle_ms: Option<u32>,
}

/// Largest walkthrough.json or controls file read back.
const MAX_SAVED: u64 = 8 * 1024 * 1024;
/// Largest saved picture read back to compare.
const MAX_PICTURE: u64 = 64 * 1024 * 1024;
/// Controls named per capture as added or removed.
const MAX_LISTED: usize = 20;

fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, ToolError> {
    let len = std::fs::metadata(path).map_err(|e| ToolError::new(ErrorCode::NotFound, format!("Can't read {}: {e}", path.display())))?.len();
    if len > cap {
        return Err(ToolError::invalid(format!("{} is larger than {cap} bytes.", path.display())));
    }
    std::fs::read(path).map_err(|e| ToolError::new(ErrorCode::NotFound, format!("Can't read {}: {e}", path.display())))
}

/// The steps of a saved walk, with the kept-out texts filled from `secrets` in order, and for each
/// whether its text was kept out. Also whether waits are known (files that list every step).
fn saved_steps(doc: &Value, secrets: &[String]) -> Result<(Vec<Step>, Vec<bool>, bool), ToolError> {
    let listed = doc.get("steps").and_then(Value::as_array).filter(|s| !s.is_empty());
    let source: Vec<&Value> = match listed {
        Some(s) => s.iter().collect(),
        None => {
            doc.get("captures").and_then(Value::as_array).map(|c| c.iter().filter_map(|c| c.get("step")).filter(|s| !s.is_null()).collect()).unwrap_or_default()
        }
    };
    let needed = source.iter().filter(|s| s.get("action").and_then(Value::as_str) == Some("text") && !s.get("text").is_some_and(Value::is_string)).count();
    if needed != secrets.len() {
        return Err(ToolError::invalid(format!(
            "The walk kept out {needed} text(s); give exactly that many in secrets, in the order they're typed ({} given).",
            secrets.len()
        )));
    }
    let mut left = secrets.iter();
    let mut steps = Vec::with_capacity(source.len());
    let mut kept_out = Vec::with_capacity(source.len());
    for (i, s) in source.iter().enumerate() {
        if s.get("action").and_then(Value::as_str) == Some("text") {
            let (text, out) = match s.get("text").and_then(Value::as_str) {
                Some(t) => (t.to_string(), s.get("secret").and_then(Value::as_bool).unwrap_or(false)),
                None => (left.next().cloned().unwrap_or_default(), true),
            };
            steps.push(Step::Text { text, secret: out });
            kept_out.push(out);
        } else {
            let step: Step =
                serde_json::from_value((*s).clone()).map_err(|e| ToolError::invalid(format!("Saved step {} can't be read: {e}", i.saturating_add(1))))?;
            steps.push(step);
            kept_out.push(false);
        }
    }
    Ok((steps, kept_out, listed.is_some()))
}

/// The controls a person can act on in a saved controls file, each as class, id and label.
fn control_names(tree: &Value) -> std::collections::BTreeSet<String> {
    let flag = |n: &Value, k: &str| n.get(k).and_then(Value::as_bool).unwrap_or(false);
    let text = |n: &Value, k: &str| n.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    tree.get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter(|n| ["clickable", "long_clickable", "checkable", "scrollable", "focusable"].iter().any(|k| flag(n, k)))
                .map(|n| {
                    let class = text(n, "class");
                    let short = class.rsplit('.').next().unwrap_or_default().to_string();
                    format!("{short} {} \"{}\"", text(n, "resource_id"), text(n, "label"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A file a saved capture names, only if it is a plain name in the walk's own folder.
fn saved_file(dir: &Path, capture: &Value, key: &str) -> Option<std::path::PathBuf> {
    let name = capture.get(key)?.as_str()?;
    let plain =
        !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') && !name.starts_with('.');
    plain.then(|| dir.join(name))
}

/// What changed between a saved capture and the new one.
fn compare_capture(old_dir: &Path, old: Option<&Value>, new_dir: &Path, new: &Value) -> Value {
    let index = new.get("index").cloned().unwrap_or(Value::Null);
    let Some(old) = old else {
        return json!({"index": index, "original": false, "changed": true});
    };
    let tree = |dir: &Path, c: &Value| {
        saved_file(dir, c, "controls").and_then(|p| read_capped(&p, MAX_SAVED).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    };
    let picture = |dir: &Path, c: &Value| saved_file(dir, c, "screenshot").and_then(|p| read_capped(&p, MAX_PICTURE).ok());
    let same_app = match (old.get("package").and_then(Value::as_str), new.get("package").and_then(Value::as_str)) {
        (Some(a), Some(b)) => Some(a == b),
        _ => None,
    };
    let (added, removed, controls_known) = match (tree(old_dir, old), tree(new_dir, new)) {
        (Some(a), Some(b)) => {
            let (a, b) = (control_names(&a), control_names(&b));
            (b.difference(&a).cloned().collect::<Vec<_>>(), a.difference(&b).cloned().collect::<Vec<_>>(), true)
        }
        _ => (vec![], vec![], false),
    };
    let (old_png, new_png) = (picture(old_dir, old), picture(new_dir, new));
    let picture_identical = match (&old_png, &new_png) {
        (Some(a), Some(b)) => Some(a == b),
        _ => None,
    };
    let picture = match (&old_png, &new_png) {
        (Some(a), Some(b)) => compare_pictures(a, b),
        _ => Value::Null,
    };
    let picture_changed = picture.get("difference_fraction").and_then(Value::as_f64).map(|f| f > PICTURE_CHANGE);
    let changed = same_app == Some(false) || !added.is_empty() || !removed.is_empty() || !controls_known;
    json!({
        "index": index,
        "original": true,
        "changed": changed,
        "same_app": same_app,
        "controls_compared": controls_known,
        "controls_added": added.len(),
        "controls_removed": removed.len(),
        "added": added.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "removed": removed.iter().take(MAX_LISTED).collect::<Vec<_>>(),
        "picture_identical": picture_identical,
        "picture_changed": picture_changed,
        "picture": picture,
    })
}

/// How far two channel values may differ and still count as the same pixel.
const PIXEL_TOLERANCE: u8 = 24;
/// The share of pixels that must differ for a picture to count as changed.
const PICTURE_CHANGE: f64 = 0.01;
/// The share of a picture's height at the top left out (the status bar, with its clock).
const STATUS_BAR_SHARE: u32 = 6;

/// The two pictures compared pixel by pixel, the top `STATUS_BAR_SHARE` percent left out.
fn compare_pictures(a: &[u8], b: &[u8]) -> Value {
    use super::web_common::{Image, decode_png, pixel_diff};
    let below_bar = |img: Image| {
        let skip = img.height.saturating_mul(STATUS_BAR_SHARE) / 100;
        let start = (skip as usize).saturating_mul(img.width as usize).saturating_mul(4);
        Image { width: img.width, height: img.height.saturating_sub(skip), rgba: img.rgba.get(start..).unwrap_or_default().to_vec() }
    };
    match (decode_png(a), decode_png(b)) {
        (Ok(a), Ok(b)) => {
            let mut v = pixel_diff(&below_bar(a), &below_bar(b), PIXEL_TOLERANCE);
            if let Value::Object(m) = &mut v {
                m.insert("top_left_out_percent".into(), json!(STATUS_BAR_SHARE));
            }
            v
        }
        (Err(e), _) | (_, Err(e)) => json!({"comparable": false, "reason": e.message}),
    }
}

fn adb_walkthrough_replay(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ReplayParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let given = Path::new(&p.walkthrough);
    if !given.is_absolute() {
        return Err(ToolError::invalid("walkthrough must be an absolute path."));
    }
    let file = if given.is_dir() { given.join("walkthrough.json") } else { given.to_path_buf() };
    let doc: Value = serde_json::from_slice(&read_capped(&file, MAX_SAVED)?).map_err(|e| ToolError::invalid(format!("{} isn't JSON: {e}", file.display())))?;
    if doc.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(ToolError::invalid(format!("{} isn't a walkthrough adb_walkthrough_record wrote.", file.display())));
    }
    match doc.get("version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(VERSION) => {}
        v => return Err(ToolError::invalid(format!("This reads walkthrough version {VERSION}; the file says {v:?}."))),
    }
    let (steps, kept_out, waits_known) = saved_steps(&doc, &p.secrets)?;
    let commands = android_input::commands(&steps)?;
    let settle_ms = p.settle_ms.or_else(|| doc.get("settle_ms").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok())).unwrap_or(DEFAULT_SETTLE_MS);
    let settle = check_settle(settle_ms)?;
    let old_dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    let adb = adb_for(ctx)?.timeout(STEP_TIMEOUT.max(DUMP_TIMEOUT)).idle_timeout(STEP_TIMEOUT.max(DUMP_TIMEOUT));
    ensure_online(&adb, serial)?;
    let dir = create_absent_dir(&p.output_directory)?;
    let made = UnlessWritten::new(&dir);

    let w = walk(ctx, &adb, serial, &dir, &steps, &commands, settle, &kept_out)?;
    let old_captures = doc.get("captures").and_then(Value::as_array).cloned().unwrap_or_default();
    let changes: Vec<Value> = w.captures.iter().enumerate().map(|(k, c)| compare_capture(&old_dir, old_captures.get(k), &dir, c)).collect();
    let mut out = manifest(doc.get("name").and_then(Value::as_str), serial, settle_ms, &w);
    if let Value::Object(m) = &mut out {
        m.insert("replay_of".into(), json!(file));
        m.insert("changes".into(), json!(changes));
    }
    let new_file = write_manifest(&dir, &out)?;
    made.keep();

    let changed = changes.iter().filter(|c| c["changed"].as_bool() == Some(true)).count();
    let mut lims = vec![Limitation::new("device_changed", "This acted on the device's screen.")];
    if !waits_known {
        lims.push(Limitation::new("waits_not_known", "The recording doesn't list its waits (an older file), so they were left out of the replay."));
    }
    if let Some(s) = &w.stopped {
        lims.push(Limitation::new("stopped_early", format!("The replay ended early: {}", s["reason"].as_str().unwrap_or("unknown"))));
    }
    if old_captures.len() != w.captures.len() {
        lims.push(Limitation::new("capture_count_differs", format!("The recording has {} capture(s), the replay {}.", old_captures.len(), w.captures.len())));
    }
    lims.push(Limitation::new(
        "how_compared",
        "A capture counts as changed when its app or the controls a person can act on differ. Pictures are compared pixel by pixel apart from the top 6% (the status bar): a picture counts as changed when over 1% of its pixels differ by more than 24 in a channel, so animations or a changed clock elsewhere can still count."
    ));
    let pictures = changes.iter().filter(|c| c["picture_changed"].as_bool() == Some(true)).count();
    let summary = format!(
        "Replayed {} step(s): {changed} of {} capture(s) differ from the recording, {pictures} picture(s) look different. Saved to {}.",
        steps.len(),
        changes.len(),
        dir.display()
    );
    let mut coverage = Coverage::complete(w.captures.len() as u64);
    if w.stopped.is_some() {
        coverage.status = CoverageStatus::Partial;
    }
    let e = dev_ev(
        ctx,
        REPLAY_TOOL,
        serial,
        json!({"serial": serial, "walkthrough": file, "output_directory": dir, "secrets": p.secrets.len(), "settle_ms": settle_ms}),
    )
    .result(json!({"file": new_file, "replay_of": file, "complete": w.stopped.is_none(), "stopped": w.stopped, "changed": changed, "pictures_changed": pictures, "changes": changes}))
    .summary(summary)
    .limitations(lims)
    .location(Location::new("path", new_file.to_string_lossy()))
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
        let w = |stopped| Walk { steps: vec![], captures: vec![json!({"index": 0})], stopped, secrets: 0 };
        let done = manifest(Some("sign in"), "emulator-5554", 1000, &w(None));
        assert_eq!((done["format"].as_str(), done["version"].as_u64(), done["complete"].as_bool()), (Some(FORMAT), Some(1), Some(true)));
        let cut = manifest(None, "emulator-5554", 1000, &w(Some(json!({"at_step": 2, "reason": "Error: x"}))));
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
    fn saved_steps_come_back_with_kept_out_texts_filled_in_order() {
        let doc = json!({"steps": [
            {"action": "tap", "x": 1, "y": 2},
            {"action": "text", "text": null, "secret": true, "characters": 4},
            {"action": "wait", "ms": 10},
            {"action": "text", "text": "hi", "secret": false, "characters": 2},
            {"action": "text", "text": null, "secret": true, "characters": 1},
        ]});
        let (steps, out, waits) = saved_steps(&doc, &["abcd".into(), "z".into()]).unwrap();
        assert!(waits);
        assert_eq!(out, vec![false, true, false, false, true]);
        assert_eq!(steps[1], Step::Text { text: "abcd".into(), secret: true });
        assert_eq!(steps[4], Step::Text { text: "z".into(), secret: true });
        assert_eq!(steps[3], Step::Text { text: "hi".into(), secret: false });
        for wrong in [vec![], vec!["a".to_string()], vec!["a".into(), "b".into(), "c".into()]] {
            assert_eq!(saved_steps(&doc, &wrong).unwrap_err().code, ErrorCode::BadInput);
        }
        // An older file without the step list: steps from the captures, waits unknown.
        let old = json!({"captures": [{"step": null}, {"step": {"action": "key", "key": "back"}}]});
        let (steps, _, waits) = saved_steps(&old, &[]).unwrap();
        assert_eq!((steps, waits), (vec![Step::Key { key: "back".into() }], false));
        assert!(saved_steps(&json!({"steps": [{"action": "reboot"}]}), &[]).is_err());
    }

    #[test]
    fn a_capture_is_compared_by_app_and_controls() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let tree = |labels: &[&str]| json!({"nodes": labels.iter().map(|l| json!({"class": "android.widget.Button", "resource_id": "", "label": l, "clickable": true})).chain([json!({"class": "android.widget.TextView", "label": "", "text": "x"})]).collect::<Vec<_>>()});
        std::fs::write(a.path().join("step-01.json"), serde_json::to_vec(&tree(&["OK", "Cancel"])).unwrap()).unwrap();
        std::fs::write(b.path().join("step-01.json"), serde_json::to_vec(&tree(&["OK", "Help"])).unwrap()).unwrap();
        std::fs::write(a.path().join("step-01.png"), b"png").unwrap();
        std::fs::write(b.path().join("step-01.png"), b"png").unwrap();
        let cap = |pkg: &str| json!({"index": 1, "controls": "step-01.json", "screenshot": "step-01.png", "package": pkg});
        let c = compare_capture(a.path(), Some(&cap("app")), b.path(), &cap("app"));
        assert_eq!((c["changed"].as_bool(), c["same_app"].as_bool(), c["picture_identical"].as_bool()), (Some(true), Some(true), Some(true)));
        assert_eq!(c["added"], json!(["Button  \"Help\""]));
        assert_eq!(c["removed"], json!(["Button  \"Cancel\""]));
        let same = compare_capture(a.path(), Some(&cap("app")), a.path(), &cap("app"));
        assert_eq!(same["changed"].as_bool(), Some(false));
        assert_eq!(compare_capture(a.path(), Some(&cap("app")), a.path(), &cap("other"))["changed"].as_bool(), Some(true));
        assert_eq!(compare_capture(a.path(), None, b.path(), &cap("app"))["original"].as_bool(), Some(false));
        // A name pointing outside the walk's folder is not followed.
        let outside = json!({"index": 1, "controls": "../step-01.json", "package": "app"});
        assert_eq!(compare_capture(a.path(), Some(&outside), a.path(), &outside)["controls_compared"].as_bool(), Some(false));
    }

    #[test]
    fn pictures_are_compared_by_pixel_leaving_out_the_status_bar() {
        use crate::tools::web_common::{Image, encode_png};
        let png = |paint: &dyn Fn(u32, u32) -> u8| {
            let (w, h) = (20u32, 100u32);
            let mut rgba = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let v = paint(x, y);
                    rgba.extend([v, v, v, 255]);
                }
            }
            encode_png(&Image { width: w, height: h, rgba }).unwrap()
        };
        let base = png(&|_, _| 100);
        // A different clock in the status bar (top rows) and a slight shade change: the same.
        let clock = png(&|x, y| if y < 5 && x < 10 { 255 } else { 110 });
        let same = compare_pictures(&base, &clock);
        assert_eq!((same["comparable"].as_bool(), same["identical"].as_bool()), (Some(true), Some(true)), "{same}");
        // A new panel in the lower half: changed.
        let panel = png(&|_, y| if y > 60 { 0 } else { 100 });
        let diff = compare_pictures(&base, &panel);
        assert!(diff["difference_fraction"].as_f64().unwrap() > PICTURE_CHANGE, "{diff}");
        assert_eq!(compare_pictures(&base, b"not a png")["comparable"].as_bool(), Some(false));
    }

    #[test]
    fn replay_refuses_a_file_that_is_not_a_walkthrough_before_touching_the_device() {
        let (e, d) = engine_with_stand_in_adb();
        let opts = || CallOptions::new(Caller::Mcp).with_consent(Consent::Granted);
        let out = d.path().join("again");
        let file = d.path().join("walkthrough.json");
        let call = |doc: Value, extra: Value| {
            std::fs::write(&file, serde_json::to_vec(&doc).unwrap()).unwrap();
            let mut v = json!({"serial": "emulator-5554", "walkthrough": file, "output_directory": out});
            if let (Value::Object(m), Value::Object(x)) = (&mut v, extra) {
                m.extend(x);
            }
            e.call(REPLAY_TOOL, v, opts()).unwrap_err().code
        };
        let good = json!({"format": FORMAT, "version": 1, "steps": [{"action": "text", "text": null, "secret": true, "characters": 3}]});
        assert_eq!(call(json!({"format": "other", "version": 1}), json!({})), ErrorCode::BadInput);
        assert_eq!(call(json!({"format": FORMAT, "version": 2, "steps": []}), json!({})), ErrorCode::BadInput);
        assert_eq!(call(good.clone(), json!({})), ErrorCode::BadInput, "a kept-out text needs its secret");
        assert_eq!(call(good, json!({"walkthrough": "relative/walkthrough.json", "secrets": ["abc"]})), ErrorCode::BadInput);
        assert!(!out.exists());
        let no_consent = e.call(REPLAY_TOOL, json!({"serial": "emulator-5554", "walkthrough": file, "output_directory": out}), CallOptions::new(Caller::Mcp));
        assert_eq!(no_consent.unwrap_err().code, ErrorCode::ConsentRequired);
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
