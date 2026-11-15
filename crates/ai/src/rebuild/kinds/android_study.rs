//! The Android study walk: during the study of an APK target, install the original on a device or
//! emulator, start it, and walk through its first screen with the planner in
//! `craft_anything_engines::android_walk`, one step at a time through `adb_walkthrough_record`'s
//! `append`. The walk is saved in capture/android-walkthrough/, where the screen check replays it
//! on the rebuild.
//!
//! Everything here watches from outside, as a person using the app would: the package name comes
//! from the device's app list before and after the install in Clean room (Deep mode reads the
//! manifest), and only the walk's own captures are read. A walk the person recorded (one without
//! `by: study`) is never replaced. Every reason not to walk is said in the feed and in the notes,
//! and never fails the study.

use super::android::{ORIGINAL_WALK, is_package};
use super::*;
use crate::rebuild::output::apk_package;
use craft_anything_engines::android_walk::{self, MAX_TAPS, Next, Plan, Why};
use std::collections::BTreeSet;

/// Where the study records before the walk replaces capture/android-walkthrough/.
pub const NEW_WALK: &str = "android-walkthrough-new";
/// What the study writes as the walk's `by`, so a later study knows the walk is its own.
pub const BY_STUDY: &str = "study";
/// How long the app is given after it starts before its first screen is recorded.
const LAUNCH_SETTLE: Duration = Duration::from_secs(6);
/// The most time a walk takes, looked at between its steps.
const MAX_WALK_TIME: Duration = Duration::from_secs(180);
/// Largest walkthrough.json or controls file read back.
const MAX_SAVED: u64 = 8 * 1024 * 1024;
/// Largest APK read for its package name in Deep mode.
const MAX_APK: u64 = 2 * 1024 * 1024 * 1024;

/// What the walk needs from the study; [`StudyCx`] in a run, a stand-in in tests.
pub(super) trait Study {
    /// Run an engine tool; its `result`.
    fn call(&self, tool: &str, params: Value) -> Result<Value, ToolError>;
    /// Say a line in the feed.
    fn say(&mut self, text: &str) -> Result<(), ToolError>;
    /// Refuses once the person paused or stopped the run.
    fn stop(&self) -> Result<(), ToolError>;
    /// Wait, looking at the stop in short steps.
    fn wait(&self, d: Duration) -> Result<(), ToolError>;
}

impl Study for StudyCx<'_> {
    fn call(&self, tool: &str, params: Value) -> Result<Value, ToolError> {
        StudyCx::call(self, tool, params).map(|o| o.to_json().get("result").cloned().unwrap_or(Value::Null))
    }
    fn say(&mut self, text: &str) -> Result<(), ToolError> {
        StudyCx::say(self, text)
    }
    fn stop(&self) -> Result<(), ToolError> {
        StudyCx::stop(self).check()
    }
    fn wait(&self, d: Duration) -> Result<(), ToolError> {
        let until = Instant::now() + d;
        while Instant::now() < until {
            Study::stop(self)?;
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }
}

/// What the walk is about.
pub(super) struct Setup {
    pub capture: PathBuf,
    pub target: PathBuf,
    pub deep: bool,
    pub runtime_study: bool,
}

/// [`super::TargetKind::study_running`] for Android apps.
pub(super) fn study_running(cx: &mut StudyCx) -> Result<Option<String>, ToolError> {
    let setup = Setup { capture: cx.ws.capture_dir(), target: cx.target.to_path_buf(), deep: cx.deep, runtime_study: cx.runtime_study };
    walk_original(cx, &setup)
}

/// The notes' section for what the walk did.
fn section(text: &str) -> String {
    format!("\n## Walking it on a device\n\n{text}\n")
}

/// Said in the feed and noted: the study goes on.
fn skip(io: &mut dyn Study, text: &str) -> Result<Option<String>, ToolError> {
    io.say(text)?;
    Ok(Some(section(text)))
}

/// A tool's error, except a stop, which ends the study as a stop does everywhere.
fn soft<T>(r: Result<T, ToolError>) -> Result<Result<T, ToolError>, ToolError> {
    match r {
        Err(e) if e.code == ErrorCode::Cancelled => Err(e),
        other => Ok(other),
    }
}

/// Who recorded the walk already in capture/android-walkthrough/.
#[derive(Debug, PartialEq, Eq)]
enum Owner {
    /// There is none.
    Nobody,
    /// An earlier study (`by: study`): this one replaces it.
    Study,
    /// The person (or a file that can't be read, which is never replaced either).
    Person,
}

fn read_json(path: &Path) -> Option<Value> {
    let len = std::fs::metadata(path).ok()?.len();
    if len > MAX_SAVED {
        return None;
    }
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn owner(walk: &Path) -> Owner {
    let file = walk.join("walkthrough.json");
    if !walk.exists() {
        return Owner::Nobody;
    }
    match read_json(&file) {
        Some(doc) if doc.get("by").and_then(Value::as_str) == Some(BY_STUDY) => Owner::Study,
        _ if !file.exists() && std::fs::read_dir(walk).is_ok_and(|mut d| d.next().is_none()) => Owner::Nobody,
        _ => Owner::Person,
    }
}

/// The device the study walks on, from `adb_devices_find`'s `check_pick` (asked with `for_check`).
#[derive(Debug, PartialEq, Eq)]
enum Device {
    Use(String),
    Start(String),
    Skip(String),
}

/// The checks' rule, except that a phone is used only when it is the default or the only device:
/// with several connected and no default, the checks' rule may fall back to the first one, which
/// would put the original on the person's phone.
fn study_device(devices: &Value) -> Device {
    let pick = devices.get("check_pick").unwrap_or(&Value::Null);
    let text = |k: &str| pick.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    match (text("serial"), text("start"), text("how")) {
        (Some(_), _, Some(how)) if how == "first_online" => Device::Skip(
            "Several devices are connected, none of them an emulator and none set as the default, so the original wasn't put on one (a phone is used only when it is the default or the only device). Set one with adb_default_device_set to walk it there."
                .into(),
        ),
        (Some(serial), _, _) => Device::Use(serial),
        (None, Some(avd), _) => Device::Start(avd),
        _ => Device::Skip(format!(
            "No Android device or emulator, so the original wasn't walked: {}",
            text("message").unwrap_or_else(|| "no phone or emulator is connected and ready.".into())
        )),
    }
}

/// The one package the install added, from the device's app list before and after it; `None`
/// when it added none (it was there already) or several (something else was installed meanwhile).
pub(super) fn new_package(before: &BTreeSet<String>, after: &BTreeSet<String>) -> Option<String> {
    let mut added = after.difference(before);
    match (added.next(), added.next()) {
        (Some(one), None) => Some(one.clone()),
        _ => None,
    }
}

fn apps(io: &dyn Study, serial: &str) -> Result<Result<BTreeSet<String>, ToolError>, ToolError> {
    Ok(soft(io.call("adb_apps_list", json!({"serial": serial, "scope": "third_party"})))?
        .map(|r| r.get("packages").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()))
}

/// Walk the original when it is a single APK and Settings allow running it, on the device the
/// checks would use (see [`study_device`]); returns the notes' section.
pub(super) fn walk_original(io: &mut dyn Study, s: &Setup) -> Result<Option<String>, ToolError> {
    let name = s.target.to_string_lossy().to_ascii_lowercase();
    if !is_package(&name) {
        return Ok(None);
    }
    let (walk, new) = (s.capture.join(ORIGINAL_WALK), s.capture.join(NEW_WALK));
    // What a stopped study left: never a walk anything reads.
    if new.exists()
        && let Err(e) = std::fs::remove_dir_all(&new)
    {
        return skip(io, &format!("A walk left by an earlier study in capture/{NEW_WALK}/ couldn't be removed ({e}), so the original wasn't walked."));
    }
    if owner(&walk) == Owner::Person {
        return skip(io, &format!("Using the walk you recorded in capture/{ORIGINAL_WALK}/; the study leaves it as it is."));
    }
    if !name.ends_with(".apk") || !s.target.is_file() {
        return skip(
            io,
            "The original is a split set or bundle (.apks, .xapk or .aab), which can't be installed for a walk yet, so the original wasn't walked. Record one yourself with adb_walkthrough_record into capture/android-walkthrough/ to have the screens compared.",
        );
    }
    if !s.runtime_study {
        return skip(io, "Run to study is off in Settings, so the original wasn't walked; the screen check has nothing to compare.");
    }
    let devices = match soft(io.call("adb_devices_find", json!({"for_check": true})))? {
        Ok(d) => d,
        Err(e) => return skip(io, &format!("No Android device or emulator could be found ({}), so the original wasn't walked.", e.message)),
    };
    let serial = match study_device(&devices) {
        Device::Use(serial) => serial,
        Device::Skip(why) => return skip(io, &why),
        Device::Start(avd) => {
            io.say(&format!("Starting the virtual device {avd} without a window to walk the original…"))?;
            match soft(io.call("adb_emulator_start", json!({"name": avd, "no_window": true})))? {
                Ok(r) => match r.get("serial").and_then(Value::as_str) {
                    Some(serial) => serial.to_string(),
                    None => return skip(io, &format!("The virtual device {avd} started but named no serial, so the original wasn't walked.")),
                },
                Err(e) => return skip(io, &format!("The virtual device {avd} didn't start ({}), so the original wasn't walked.", e.message)),
            }
        }
    };
    // Deep mode may read the package from the manifest; Clean room tells it from the device.
    let before = if s.deep {
        None
    } else {
        match apps(io, &serial)? {
            Ok(list) => Some(list),
            Err(e) => return skip(io, &format!("The apps on {serial} couldn't be listed ({}), so the original wasn't walked.", e.message)),
        }
    };
    let known = if s.deep {
        let read = std::fs::metadata(&s.target).ok().filter(|m| m.len() <= MAX_APK).and_then(|_| std::fs::read(&s.target).ok());
        match read.as_deref().and_then(apk_package) {
            Some(p) => Some(p),
            None => return skip(io, "The original's manifest names no package that could be read, so it wasn't walked."),
        }
    } else {
        None
    };
    io.say(&format!("Installing the original on {serial} to walk through it…"))?;
    // Clean room never installs over an app that is already there: it couldn't be told apart.
    let install = json!({"serial": serial, "apk_path": s.target.display().to_string(), "replace": s.deep});
    if let Err(e) = soft(io.call("adb_app_install", install))? {
        return skip(io, &format!("The original couldn't be installed on {serial} ({}), so it wasn't walked.", e.message));
    }
    let package = match (known, before) {
        (Some(p), _) => p,
        (None, Some(before)) => {
            let after = match apps(io, &serial)? {
                Ok(list) => list,
                Err(e) => {
                    return skip(io, &format!("The apps on {serial} couldn't be listed after the install ({}), so the original wasn't walked.", e.message));
                }
            };
            match new_package(&before, &after) {
                Some(p) => p,
                None => {
                    return skip(
                        io,
                        &format!(
                            "The original was already installed on {serial} (or something else was installed at the same time), so it couldn't be told apart from the device's apps and wasn't walked."
                        ),
                    );
                }
            }
        }
        (None, None) => return skip(io, "The original's package couldn't be told, so it wasn't walked."),
    };
    let result = walk_launched(io, s, &serial, &package, &walk, &new);
    // Leave it installed but not on screen, so the install check starts the rebuild on its own.
    // A stop skips this: the call refuses at once.
    let _ = io.call("adb_app_force_stop", json!({"serial": serial, "package": package}));
    result
}

/// Why the walk ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Planner(Why),
    TimeLimit,
    /// The device refused a step, or a step couldn't be recorded.
    Failed(String),
    /// The last screen's controls couldn't be read.
    Unreadable,
}

fn ending(end: &End) -> String {
    match end {
        End::Planner(Why::AllTried) => "every control on its first screen that a walk may tap was tapped once".into(),
        End::Planner(Why::TapLimit) => format!("it reached the walk's limit of {MAX_TAPS} taps"),
        End::Planner(Why::Lost) => "Back didn't bring its first screen back, so the walk stopped there".into(),
        End::Planner(Why::LeftApp) => "a tap opened another app and Back didn't return to this one, so the walk stopped there".into(),
        End::Planner(Why::AsksForText) => format!(
            "its first screen asks for text, so the walk stopped there. To go further, record your own walk with adb_walkthrough_record into capture/{ORIGINAL_WALK}/ (without `by: study`, so it is kept)"
        ),
        End::TimeLimit => format!("it reached the walk's time limit of {} minutes", MAX_WALK_TIME.as_secs() / 60),
        End::Failed(why) => format!("a step didn't work ({why}), so the walk stopped there"),
        End::Unreadable => "a screen's controls couldn't be read, so the walk stopped there".into(),
    }
}

/// The controls file a capture names, read from the walk's own folder.
fn capture_tree(dir: &Path, capture: &Value) -> Option<Value> {
    let name = capture.get("controls")?.as_str()?;
    let plain =
        !name.is_empty() && name.len() <= 64 && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !plain {
        return None;
    }
    read_json(&dir.join(name))
}

/// The walk recorded so far: its file.
fn recorded(dir: &Path) -> Option<Value> {
    read_json(&dir.join("walkthrough.json"))
}

fn captures(doc: &Value) -> &[Value] {
    doc.get("captures").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// Start the installed original, walk it into `new`, and replace `walk` with it.
fn walk_launched(io: &mut dyn Study, s: &Setup, serial: &str, package: &str, walk: &Path, new: &Path) -> Result<Option<String>, ToolError> {
    io.say(&format!("Starting {package} on {serial}…"))?;
    if let Err(e) = soft(io.call("adb_app_launch", json!({"serial": serial, "package": package})))? {
        return skip(io, &format!("{package} didn't start on {serial} ({}), so it wasn't walked.", e.message));
    }
    io.wait(LAUNCH_SETTLE)?;
    io.say("Walking through its first screen, one tap at a time…")?;
    // The record tool creates the walk's folder inside capture/, which the study made.
    let first = json!({"serial": serial, "output_directory": new.display().to_string(), "steps": [{"action": "wait", "ms": 500}], "name": "the study's walk", "by": BY_STUDY});
    if let Err(e) = soft(io.call("adb_walkthrough_record", first))? {
        let _ = std::fs::remove_dir_all(new);
        return skip(io, &format!("Recording its first screen on {serial} didn't work ({}), so no walk was kept.", e.message));
    }
    let first_tree = recorded(new).and_then(|doc| captures(&doc).first().and_then(|c| capture_tree(new, c)));
    let Some(first_tree) = first_tree.filter(|t| !android_walk::controls(t).is_empty()) else {
        let _ = std::fs::remove_dir_all(new);
        return skip(io, &format!("The controls of {package}'s first screen couldn't be read, so no walk was kept."));
    };
    if android_walk::package(&first_tree) != Some(package) {
        let shown = android_walk::package(&first_tree).unwrap_or("nothing readable").to_string();
        let _ = std::fs::remove_dir_all(new);
        return skip(io, &format!("{package} wasn't on screen after it started (the screen showed {shown}), so no walk was kept."));
    }

    let started = Instant::now();
    let mut plan = Plan::default();
    let mut after_back = false;
    let end = loop {
        io.stop()?;
        if started.elapsed() > MAX_WALK_TIME {
            break End::TimeLimit;
        }
        let Some(doc) = recorded(new) else { break End::Unreadable };
        if doc.get("complete").and_then(Value::as_bool) != Some(true) {
            let why = doc.pointer("/stopped/reason").and_then(Value::as_str).unwrap_or("the device stopped it").to_string();
            break End::Failed(why);
        }
        let Some(current) = captures(&doc).last().and_then(|c| capture_tree(new, c)) else { break End::Unreadable };
        let step = match android_walk::next(&plan, &first_tree, &current, package, after_back) {
            Next::Tap { x, y, key } => {
                plan.tried.insert(key);
                plan.taps = plan.taps.saturating_add(1);
                after_back = false;
                json!({"action": "tap", "x": x, "y": y})
            }
            Next::Back => {
                after_back = true;
                json!({"action": "key", "key": "back"})
            }
            Next::Done(why) => break End::Planner(why),
        };
        let more = json!({"serial": serial, "output_directory": new.display().to_string(), "steps": [step], "append": true});
        if let Err(e) = soft(io.call("adb_walkthrough_record", more))? {
            break End::Failed(e.message);
        }
    };

    let screens = recorded(new).map_or(0, |doc| captures(&doc).len());
    // Replace the earlier study's walk (a person's was never reached here).
    if walk.exists()
        && let Err(e) = std::fs::remove_dir_all(walk)
    {
        return skip(io, &format!("The earlier study's walk in capture/{ORIGINAL_WALK}/ couldn't be replaced ({e}); the new one is in capture/{NEW_WALK}/."));
    }
    if let Err(e) = std::fs::rename(new, walk) {
        return skip(io, &format!("The walk couldn't be moved into capture/{ORIGINAL_WALK}/ ({e}); it is in capture/{NEW_WALK}/."));
    }
    let mode =
        if s.deep { "Deep mode read its package from the manifest" } else { "its package was told from the device's app list before and after the install" };
    let text = format!(
        "The original was installed on {serial} ({mode}: {package}), started, and walked through from its first screen: {} tap(s), {screens} screen(s) saved in capture/{ORIGINAL_WALK}/ (step-NN.png and step-NN.json, with walkthrough.json listing the steps). It ended because {}. After each build the screen check goes through the same steps on the rebuild and compares each screen's controls and where they sit. The walk types nothing.",
        plan.taps,
        ending(&end)
    );
    io.say(&format!("Walked the original: {} tap(s), {screens} screen(s) saved.", plan.taps))?;
    Ok(Some(section(&text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const APP: &str = "com.me.notes";

    fn button(label: &str, top: i64) -> Value {
        json!({"class": "android.widget.Button", "package": APP, "label": label, "resource_id": "", "clickable": true, "enabled": true,
            "bounds": {"left": 0, "top": top, "right": 400, "bottom": top + 100}})
    }

    /// A stand-in device with a two-screen app: "Stay" keeps the first screen, "Open" opens a
    /// detail screen that Back closes. It records walks as the record tool writes them.
    struct Phone {
        calls: RefCell<Vec<(String, Value)>>,
        said: Vec<String>,
        on_detail: RefCell<bool>,
        before: Vec<&'static str>,
        after: Vec<&'static str>,
        /// The first screen shows another app.
        elsewhere: bool,
        refuse: Option<&'static str>,
    }

    fn phone() -> Phone {
        Phone {
            calls: RefCell::default(),
            said: Vec::new(),
            on_detail: RefCell::new(false),
            before: vec!["com.other"],
            after: vec!["com.other", APP],
            elsewhere: false,
            refuse: None,
        }
    }

    impl Phone {
        fn screen(&self) -> Value {
            if self.elsewhere {
                return json!({"nodes": [{"class": "android.widget.Button", "package": "com.android.launcher", "label": "Home", "clickable": true}]});
            }
            if *self.on_detail.borrow() { json!({"nodes": [button("Close", 0)]}) } else { json!({"nodes": [button("Stay", 0), button("Open", 200)]}) }
        }

        fn record(&self, p: &Value) -> Result<Value, ToolError> {
            let dir = PathBuf::from(p["output_directory"].as_str().unwrap());
            let save = |doc: &Value| std::fs::write(dir.join("walkthrough.json"), serde_json::to_vec(doc).unwrap()).unwrap();
            let capture = |i: usize, step: Value| {
                let file = format!("step-{i:02}.json");
                std::fs::write(dir.join(&file), serde_json::to_vec(&self.screen()).unwrap()).unwrap();
                json!({"index": i, "step": step, "controls": file})
            };
            if p["append"].as_bool() != Some(true) {
                std::fs::create_dir(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
                let doc = json!({"format": "craft-anything-android-walkthrough", "version": 1, "serial": p["serial"], "by": p["by"], "complete": true,
                    "steps": p["steps"], "captures": [capture(0, Value::Null)]});
                save(&doc);
                return Ok(json!({}));
            }
            let mut doc: Value = serde_json::from_slice(&std::fs::read(dir.join("walkthrough.json")).unwrap()).unwrap();
            let step = p["steps"][0].clone();
            match (step["action"].as_str(), step["y"].as_u64()) {
                (Some("tap"), Some(y)) if y > 200 => *self.on_detail.borrow_mut() = true,
                (Some("key"), _) => *self.on_detail.borrow_mut() = false,
                _ => {}
            }
            let n = doc["captures"].as_array().unwrap().len();
            let c = capture(n, step.clone());
            doc["steps"].as_array_mut().unwrap().push(step);
            doc["captures"].as_array_mut().unwrap().push(c);
            save(&doc);
            Ok(json!({}))
        }

        fn tools(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|(t, _)| t.clone()).collect()
        }
    }

    impl Study for Phone {
        fn call(&self, tool: &str, params: Value) -> Result<Value, ToolError> {
            self.calls.borrow_mut().push((tool.to_string(), params.clone()));
            if self.refuse == Some(tool) {
                return Err(ToolError::new(ErrorCode::ProgramFailed, "the stand-in said no"));
            }
            match tool {
                "adb_devices_find" => Ok(json!({"check_pick": {"serial": "emulator-5554", "how": "first_emulator"}})),
                "adb_apps_list" => {
                    let installed = self.calls.borrow().iter().any(|(t, _)| t == "adb_app_install");
                    Ok(json!({"packages": if installed { &self.after } else { &self.before }}))
                }
                "adb_walkthrough_record" => self.record(&params),
                _ => Ok(json!({})),
            }
        }
        fn say(&mut self, text: &str) -> Result<(), ToolError> {
            self.said.push(text.to_string());
            Ok(())
        }
        fn stop(&self) -> Result<(), ToolError> {
            Ok(())
        }
        fn wait(&self, _d: Duration) -> Result<(), ToolError> {
            Ok(())
        }
    }

    fn setup(d: &tempfile::TempDir, file: &str) -> Setup {
        let target = d.path().join(file);
        std::fs::write(&target, b"PK").unwrap();
        let capture = d.path().join("capture");
        std::fs::create_dir_all(&capture).unwrap();
        Setup { capture, target, deep: false, runtime_study: true }
    }

    #[test]
    fn the_study_walks_the_first_screen_from_outside_and_keeps_it_as_its_own() {
        let d = tempfile::tempdir().unwrap();
        let s = setup(&d, "notes.apk");
        // A study stopped earlier left a half walk; an earlier study's walk is replaced.
        std::fs::create_dir_all(s.capture.join(NEW_WALK)).unwrap();
        std::fs::create_dir_all(s.capture.join(ORIGINAL_WALK)).unwrap();
        std::fs::write(s.capture.join(ORIGINAL_WALK).join("walkthrough.json"), br#"{"by": "study", "old": true}"#).unwrap();
        let mut p = phone();
        let section = walk_original(&mut p, &s).unwrap().unwrap();
        assert!(section.contains(APP), "{section}");
        assert!(section.contains("2 tap(s), 4 screen(s)") && section.contains("every control on its first screen"), "{section}");
        let doc = read_json(&s.capture.join(ORIGINAL_WALK).join("walkthrough.json")).unwrap();
        assert_eq!(doc["by"], "study");
        assert!(doc.get("old").is_none(), "the earlier study's walk was replaced");
        let actions: Vec<String> = doc["steps"].as_array().unwrap().iter().map(|s| s["action"].as_str().unwrap().to_string()).collect();
        assert_eq!(actions, ["wait", "tap", "tap", "key"], "Stay, then Open, then Back to the first screen");
        assert!(!s.capture.join(NEW_WALK).exists());
        // Clean room: the package came from the device, the install never replaced an app, and the
        // original was stopped at the end.
        let tools = p.tools();
        assert_eq!(tools.first().map(String::as_str), Some("adb_devices_find"));
        assert_eq!(tools.last().map(String::as_str), Some("adb_app_force_stop"));
        let install = p.calls.borrow().iter().find(|(t, _)| t == "adb_app_install").map(|(_, v)| v.clone()).unwrap();
        assert_eq!(install["replace"], false);
        assert!(p.said.iter().any(|l| l.starts_with("Walked the original: 2 tap(s)")), "{:?}", p.said);
    }

    #[test]
    fn a_walk_the_person_recorded_is_never_replaced() {
        let d = tempfile::tempdir().unwrap();
        let s = setup(&d, "notes.apk");
        let theirs = s.capture.join(ORIGINAL_WALK).join("walkthrough.json");
        std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
        for doc in [&br#"{"format": "craft-anything-android-walkthrough", "steps": []}"#[..], b"not json"] {
            std::fs::write(&theirs, doc).unwrap();
            let mut p = phone();
            let section = walk_original(&mut p, &s).unwrap().unwrap();
            assert!(section.contains("Using the walk you recorded"), "{section}");
            assert!(p.tools().is_empty(), "no device was touched");
            assert_eq!(std::fs::read(&theirs).unwrap(), doc);
        }
    }

    #[test]
    fn every_reason_not_to_walk_is_said_and_none_fails_the_study() {
        let d = tempfile::tempdir().unwrap();
        let said = |s: &Setup, p: &mut Phone| {
            let section = walk_original(p, s).unwrap().unwrap();
            assert_eq!(p.said.last().map(|l| section.contains(l.as_str())), Some(true), "the same words in the feed and the notes");
            assert!(!s.capture.join(ORIGINAL_WALK).exists() && !s.capture.join(NEW_WALK).exists(), "no walk kept: {section}");
            section
        };
        // A split set or a bundle.
        for f in ["notes.apks", "notes.xapk", "notes.aab"] {
            let d = tempfile::tempdir().unwrap();
            assert!(said(&setup(&d, f), &mut phone()).contains("can't be installed for a walk yet"), "{f}");
        }
        // Not an Android package at all: nothing to say.
        assert_eq!(walk_original(&mut phone(), &setup(&d, "notes.exe")).unwrap(), None);
        // Run to study off.
        let mut s = setup(&d, "notes.apk");
        s.runtime_study = false;
        let mut p = phone();
        assert!(said(&s, &mut p).contains("Run to study is off"));
        assert!(p.tools().is_empty());
        s.runtime_study = true;
        // No adb, or no device.
        let mut p = Phone { refuse: Some("adb_devices_find"), ..phone() };
        assert!(said(&s, &mut p).contains("No Android device or emulator could be found"));
        // Already installed: Clean room can't tell its package, and nothing is started.
        let mut p = Phone { after: vec!["com.other"], ..phone() };
        assert!(said(&s, &mut p).contains("already installed"));
        assert!(!p.tools().iter().any(|t| t == "adb_app_launch"));
        // The install or the start refused.
        let mut p = Phone { refuse: Some("adb_app_install"), ..phone() };
        assert!(said(&s, &mut p).contains("couldn't be installed"));
        let mut p = Phone { refuse: Some("adb_app_launch"), ..phone() };
        assert!(said(&s, &mut p).contains("didn't start"));
        assert_eq!(p.tools().last().map(String::as_str), Some("adb_app_force_stop"), "stopped even so");
        // Its first screen is another app's: no walk kept.
        let mut p = Phone { elsewhere: true, ..phone() };
        assert!(said(&s, &mut p).contains("wasn't on screen after it started (the screen showed com.android.launcher)"));
        // Deep mode reads the manifest; a file with none is said.
        s.deep = true;
        let mut p = phone();
        assert!(said(&s, &mut p).contains("manifest names no package"));
        assert!(!p.tools().iter().any(|t| t == "adb_apps_list" || t == "adb_app_install"));
    }

    #[test]
    fn a_failed_step_ends_the_walk_and_keeps_what_was_recorded() {
        let d = tempfile::tempdir().unwrap();
        let s = setup(&d, "notes.apk");
        struct Once(Phone, RefCell<usize>);
        impl Study for Once {
            fn call(&self, tool: &str, params: Value) -> Result<Value, ToolError> {
                if tool == "adb_walkthrough_record" && params["append"] == true {
                    *self.1.borrow_mut() += 1;
                    if *self.1.borrow() == 2 {
                        return Err(ToolError::new(ErrorCode::ProgramFailed, "device gone"));
                    }
                }
                self.0.call(tool, params)
            }
            fn say(&mut self, text: &str) -> Result<(), ToolError> {
                self.0.say(text)
            }
            fn stop(&self) -> Result<(), ToolError> {
                Ok(())
            }
            fn wait(&self, _d: Duration) -> Result<(), ToolError> {
                Ok(())
            }
        }
        let mut p = Once(phone(), RefCell::new(0));
        let section = walk_original(&mut p, &s).unwrap().unwrap();
        assert!(section.contains("a step didn't work (device gone)") && section.contains("2 screen(s)"), "{section}");
        assert!(s.capture.join(ORIGINAL_WALK).join("walkthrough.json").is_file());
    }

    #[test]
    fn the_device_and_the_package_come_from_outside() {
        let pick = |v: Value| study_device(&json!({"check_pick": v}));
        assert_eq!(pick(json!({"serial": "emulator-5554", "how": "first_emulator"})), Device::Use("emulator-5554".into()));
        assert_eq!(pick(json!({"serial": "R58M", "how": "default"})), Device::Use("R58M".into()));
        assert_eq!(pick(json!({"serial": "R58M", "how": "only_one"})), Device::Use("R58M".into()), "the only device, even a phone");
        assert!(matches!(pick(json!({"serial": "R58M", "how": "first_online"})), Device::Skip(w) if w.contains("default or the only device")));
        assert_eq!(pick(json!({"start": "Medium_Phone", "how": "started"})), Device::Start("Medium_Phone".into()));
        assert!(matches!(pick(json!({"none": {"reason": "no_avd"}, "message": "No phone."})), Device::Skip(w) if w.ends_with("No phone.")));
        assert!(matches!(study_device(&Value::Null), Device::Skip(_)));
        let set = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(new_package(&set(&["a"]), &set(&["a", "b"])), Some("b".into()));
        assert_eq!(new_package(&set(&["a"]), &set(&["a"])), None, "already installed");
        assert_eq!(new_package(&set(&["a"]), &set(&["a", "b", "c"])), None, "two at once can't be told apart");
        assert_eq!(new_package(&set(&["a", "b"]), &set(&["b", "c"])), Some("c".into()));
    }
}
