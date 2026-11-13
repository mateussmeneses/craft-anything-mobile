//! Android apps: recognised from an Android package target (an APK, a split set or a bundle), an
//! Android stack, or a request that asks for one. Built with Gradle (or the stack's own build) into
//! a debug APK, and checked by building.

use super::*;
use crate::rebuild::output::{apk_package, debug_apk};
use craft_anything_engine::workspace::Claim;

pub struct AndroidKind;

impl AndroidKind {
    pub const ID: &'static str = "android";
    /// The stack used when the request names none.
    pub const DEFAULT_STACK: &'static str = "Kotlin + Jetpack Compose";
    /// The stacks that build Android apps (named in `stacks.toml`).
    pub const STACKS: &'static [&'static str] =
        &["Kotlin + Jetpack Compose", "Kotlin + Android Views", "Flutter", "React Native", ".NET MAUI", "Kotlin Multiplatform"];
}

/// Endings of files that are an Android app.
const PACKAGE_ENDINGS: &[&str] = &[".apk", ".apks", ".xapk", ".aab"];

/// What the brief says a build must be, for every Android stack.
pub const ANDROID_BRIEF: &str = "The result is an Android app: build a debug APK with the stack's own build and keep it where that build puts it. Give it a package name (applicationId) of your own, never the original's, so both can be installed side by side. Ask only for the permissions the features you rebuild need. It must start on an emulator or phone without a development server, a network login or anything only you have.";

/// What the brief adds when the target is an Android package and Deep mode lets the builder read it.
pub const STUDY_PACKAGE: &str = "The original is an Android package. Before writing code, study it with Craft Anything's tools: android_bundle_show (for a split set or bundle: which part is which), android_package_show (package, SDK levels, permissions, components), android_frameworks_find (what it was made with), android_components_list (its screens and what other apps can start) and android_resources_read (its strings, per language). Rebuild what it does in your own code; don't copy its code, resources or name.";

fn is_package(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    PACKAGE_ENDINGS.iter().any(|e| p.ends_with(e))
}

impl TargetKind for AndroidKind {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn words(&self) -> KindWords {
        KindWords {
            button: "Run on Android",
            icon: "smartphone",
            ready: "Your Android app is ready",
            change_hint: "Ask for a change, e.g. \"a bigger search bar\" or \"add a settings screen\"",
        }
    }

    fn detect(&self, cx: &DetectCx) -> Option<u32> {
        if Self::STACKS.contains(&cx.stack) {
            // Picked as the stack: an Android app, unless a game engine said otherwise first.
            Some(150)
        } else if matches!(cx.target, TargetRef::Local { path } if is_package(&path.to_string_lossy())) {
            Some(150)
        } else if cx.says(&[
            " android app",
            " android application",
            " apk ",
            " app android",
            " aplicativo android",
            " aplicativo para android",
            " app para android",
            " android-app",
            " application android",
            " android 应用",
            " 安卓应用",
            " 安卓",
        ]) {
            // Below a game (60): "an Android game" is a game.
            Some(50)
        } else {
            None
        }
    }

    fn default_stack(&self, _cx: &DetectCx) -> &'static str {
        Self::DEFAULT_STACK
    }

    fn goal(&self) -> &'static str {
        "an Android app with the same core features"
    }

    fn brief_sections(&self, cx: &BriefCx) -> Vec<String> {
        let mut s = Vec::new();
        if cx.ws.manifest.own_project() {
            return s;
        }
        s.push(ANDROID_BRIEF.to_string());
        if cx.ws.capture_dir().join(ORIGINAL_WALK).join("walkthrough.json").is_file() {
            s.push(format!("A walk through the original was recorded in capture/{ORIGINAL_WALK}/: walkthrough.json lists its steps (taps, typing, keys), and step-NN.png and step-NN.json are each screen and its controls. After each build Craft Anything goes through the same steps on yours, from its first screen, and compares each screen's controls (by kind and label) with the original's, so lay your screens out so the same taps reach the same places."));
        }
        if cx.deep && matches!(&cx.ws.manifest.target, TargetRef::Local { path } if is_package(&path.to_string_lossy())) {
            s.push(STUDY_PACKAGE.to_string());
        }
        s
    }

    /// Its screens are on a device, not on this computer, so the builder doesn't photograph them.
    fn self_screenshots(&self) -> bool {
        false
    }

    fn checks(&self, _ws: &Workspace) -> Vec<Box<dyn Check>> {
        vec![Box::new(BuildCheck), Box::new(InstallCheck), Box::new(ScreenCheck)]
    }
}

/// How long the app is given after it starts before the screen is read.
const SETTLE: Duration = Duration::from_secs(6);

/// The device a check uses, from `adb_devices_find`'s `check_pick` (asked with `for_check`): the
/// engine's rule for checks, which never stops a build to ask.
#[derive(Debug, PartialEq, Eq)]
enum Pick {
    /// This device is online.
    Use(String),
    /// Nothing is online: this virtual device can be started.
    Start(String),
    /// No device can be used, and why.
    None(String),
}

fn check_pick(devices: &Value) -> Pick {
    let pick = devices.get("check_pick").unwrap_or(&Value::Null);
    let text = |k: &str| pick.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(serial) = text("serial") {
        Pick::Use(serial)
    } else if let Some(avd) = text("start") {
        Pick::Start(avd)
    } else {
        Pick::None(text("message").unwrap_or_else(|| "no phone or emulator is connected and ready.".to_string()))
    }
}

/// What a failed Play says about the build, from the error's typed `step`: the claims it settles,
/// or `None` when the device (not the build) is why nothing ran.
fn play_failure(e: &ToolError) -> Option<Vec<Claim>> {
    match e.details.as_ref().and_then(|d| d.get("step")).and_then(Value::as_str) {
        Some("install") => Some(vec![Claim::new("The APK installs", false, e.message.clone())]),
        Some("launch") => Some(vec![Claim::new("The APK installs", true, ""), Claim::new("The app starts", false, e.message.clone())]),
        _ => None,
    }
}

/// The lines of a crash log that are about `package`'s crash, at most 40.
fn crash_lines(log: &Value, package: &str) -> Vec<String> {
    let lines: Vec<&str> = log
        .get("lines")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(|x| x.as_str().or_else(|| x.get("text").and_then(Value::as_str))).collect())
        .unwrap_or_default();
    // The last crash of this package: its "FATAL EXCEPTION" line, else the line naming its process.
    let process = lines.iter().rposition(|l| l.contains("Process: ") && l.contains(package));
    let start = process.map(|p| lines.iter().take(p + 1).rposition(|l| l.contains("FATAL EXCEPTION")).unwrap_or(p));
    start.map(|s| lines.iter().skip(s).take(40).map(|l| l.to_string()).collect()).unwrap_or_default()
}

/// Where a walkthrough recorded on the original is kept for the screen check (capture/).
pub const ORIGINAL_WALK: &str = "android-walkthrough";

/// The share of captures that came out like the original's, in percent.
fn likeness(changes: &[Value]) -> f64 {
    let compared: Vec<&Value> = changes.iter().filter(|c| c.get("original").and_then(Value::as_bool) == Some(true)).collect();
    if compared.is_empty() {
        return 0.0;
    }
    let alike = compared.iter().filter(|c| c.get("changed").and_then(Value::as_bool) == Some(false)).count();
    (alike as f64) * 100.0 / (compared.len() as f64)
}

/// One sentence per capture that differs: what to fix.
fn differences(changes: &[Value]) -> Vec<String> {
    changes
        .iter()
        .filter(|c| c.get("changed").and_then(Value::as_bool) == Some(true))
        .take(20)
        .map(|c| {
            let i = c.get("index").and_then(Value::as_u64).unwrap_or(0);
            let list = |k: &str| {
                c.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).take(8).collect::<Vec<_>>().join(", ")).unwrap_or_default()
            };
            if c.get("original").and_then(Value::as_bool) != Some(true) {
                format!("Screen {i}: the original's walk has no screen here (the rebuild went further or elsewhere).")
            } else if c.get("controls_compared").and_then(Value::as_bool) != Some(true) {
                format!("Screen {i}: its controls couldn't be read on one side.")
            } else {
                let (missing, extra) = (list("removed"), list("added"));
                let mut s = format!("Screen {i}:");
                if !missing.is_empty() {
                    s.push_str(&format!(" missing {missing}."));
                }
                if !extra.is_empty() {
                    s.push_str(&format!(" not in the original: {extra}."));
                }
                s
            }
        })
        .collect()
}

/// The rebuild goes through the same walk as the original and its screens are compared: the
/// controls a person can act on (by kind and label) on each screen, and the pictures. Runs only
/// when a walk recorded on the original is in capture/android-walkthrough/.
pub struct ScreenCheck;

impl ScreenCheck {
    pub const ID: &'static str = "android-screens";
}

impl Check for ScreenCheck {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn applies(&self, cx: &CheckCx) -> bool {
        cx.ws.capture_dir().join(ORIGINAL_WALK).join("walkthrough.json").is_file()
    }

    fn can_refine(&self, result: &CheckResult) -> bool {
        result.unknowns.is_empty()
    }

    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        let (id, round) = (self.id(), cx.round);
        let unknown = |note: String| {
            let mut r = CheckResult::new(id, round, false, note.clone());
            r.unknowns.push(note);
            r
        };
        let Some(apk) = debug_apk(&cx.ws.app_dir()) else {
            return Ok(unknown("The screens weren't compared: no debug APK was built.".into()));
        };
        let Some(package) = std::fs::read(&apk).ok().and_then(|b| apk_package(&b)) else {
            return Ok(unknown("The screens weren't compared: the built APK names no package.".into()));
        };
        // The same device the install check used; this check never starts one.
        let devices = cx.call("adb_devices_find", json!({"for_check": true})).map(|o| o.to_json().get("result").cloned().unwrap_or(Value::Null));
        let Some(Pick::Use(serial)) = devices.ok().as_ref().map(check_pick) else {
            return Ok(unknown("The screens weren't compared: no device is connected.".into()));
        };
        // Start the rebuild from its first screen, as the original's walk did.
        let _ = cx.call("adb_app_force_stop", json!({"serial": serial, "package": package}));
        if let Err(e) = cx.call("adb_app_launch", json!({"serial": serial, "package": package})) {
            return Ok(CheckResult::new(id, round, false, format!("{package} didn't start for the screen check: {}", e.message)));
        }
        let until = Instant::now() + SETTLE;
        while Instant::now() < until {
            cx.stop().check()?;
            std::thread::sleep(Duration::from_millis(250));
        }
        let out = cx.ws.checks_dir().join(format!("round-{round}")).join("screens");
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ToolError::io(parent.display(), e))?;
        }
        let walk = cx.ws.capture_dir().join(ORIGINAL_WALK);
        let params = json!({"serial": serial, "walkthrough": walk.display().to_string(), "output_directory": out.display().to_string(), "other_app": true});
        let replay = match cx.call("adb_walkthrough_replay", params) {
            Ok(o) => o.to_json().get("result").cloned().unwrap_or(Value::Null),
            // A walk that kept typed text out can't be repeated without it.
            Err(e) => return Ok(unknown(format!("The recorded walk couldn't be replayed on the rebuild: {}", e.message))),
        };
        let changes: Vec<Value> = replay.get("changes").and_then(Value::as_array).cloned().unwrap_or_default();
        let similarity = likeness(&changes);
        let diffs = differences(&changes);
        let note = if diffs.is_empty() {
            format!("The rebuild went through the original's walk with the same controls on all {} screen(s).", changes.len())
        } else {
            format!(
                "The rebuild went through the original's walk; {} of {} screen(s) differ. Fix these so each screen has the same controls as the original's (pictures are in checks/round-{round}/screens/ and capture/{ORIGINAL_WALK}/):\n{}",
                diffs.len(),
                changes.len(),
                diffs.join("\n")
            )
        };
        let mut r = CheckResult::new(id, round, true, note);
        r.similarity = Some(similarity);
        if replay.get("complete").and_then(Value::as_bool) == Some(false) {
            r.unknowns.push("The walk stopped early on the rebuild, so later screens weren't compared.".into());
        }
        Ok(r)
    }
}

/// The built APK installs on a device, starts, and is still on screen a few seconds later.
pub struct InstallCheck;

impl InstallCheck {
    pub const ID: &'static str = "android-install";
}

impl Check for InstallCheck {
    fn id(&self) -> &'static str {
        Self::ID
    }

    /// A device the person doesn't have can't be fixed by another build.
    fn can_refine(&self, result: &CheckResult) -> bool {
        result.unknowns.is_empty()
    }

    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        let id = self.id();
        let round = cx.round;
        let unknown = |note: String| {
            let mut r = CheckResult::new(id, round, false, note.clone());
            r.unknowns.push(note);
            r
        };
        let Some(apk) = debug_apk(&cx.ws.app_dir()) else {
            return Ok(CheckResult::new(
                id,
                round,
                false,
                "No debug APK was found in app/ after the build. Build it with the stack's own build so it lands under build/outputs/…/debug/.",
            ));
        };
        let bytes = std::fs::read(&apk).map_err(|e| ToolError::io(apk.display(), e))?;
        let Some(package) = apk_package(&bytes) else {
            return Ok(CheckResult::new(id, round, false, format!("{} has no readable AndroidManifest.xml naming a package.", apk.display())));
        };
        let devices = match cx.call("adb_devices_find", json!({"for_check": true})) {
            Ok(out) => out.to_json().get("result").cloned().unwrap_or(Value::Null),
            Err(e) => return Ok(unknown(format!("The build wasn't tried on a device: {}", e.message))),
        };
        // A check never asks which device: the engine's rule for checks names one, or the virtual
        // device to start.
        let device = match check_pick(&devices) {
            Pick::Use(device) | Pick::Start(device) => device,
            Pick::None(why) => return Ok(unknown(format!("The build wasn't tried on a device: {why}"))),
        };
        // Install and start it as Play does. A virtual device is started without a window and left
        // running, so the person can try the app on it (Play then gives it a window).
        let played = cx.call("project_play", json!({"project": cx.ws.root.display().to_string(), "on": "android", "device": device, "no_window": true}));
        let serial = match played {
            Ok(out) => out.to_json().pointer("/result/serial").and_then(Value::as_str).unwrap_or(&device).to_string(),
            Err(e) => {
                let Some(claims) = play_failure(&e) else {
                    return Ok(unknown(format!("The build wasn't tried on a device: {}", e.message)));
                };
                let on = e.details.as_ref().and_then(|d| d.get("serial")).and_then(Value::as_str).unwrap_or(&device).to_string();
                let note = if claims.len() > 1 {
                    format!("{package} installed on {on} but didn't start: {}", e.message)
                } else {
                    format!("The APK didn't install on {on}: {}", e.message)
                };
                let mut r = CheckResult::new(id, round, false, note);
                r.claims = claims;
                return Ok(r);
            }
        };
        let mut claims = vec![Claim::new("The APK installs", true, "")];
        // Wait in short steps so a pause or the round's limit still stops it.
        let until = Instant::now() + SETTLE;
        while Instant::now() < until {
            cx.stop().check()?;
            std::thread::sleep(Duration::from_millis(250));
        }
        let screen = cx.call("adb_ui_show", json!({"serial": serial, "limit": 1})).map(|o| o.to_json().get("result").cloned().unwrap_or(Value::Null));
        let on_screen: Vec<String> = match &screen {
            Ok(v) => v.get("packages").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()).unwrap_or_default(),
            Err(e) => return Ok(unknown(format!("{package} started, but the screen couldn't be read to see whether it stayed up: {}", e.message))),
        };
        let up = on_screen.contains(&package);
        claims.push(Claim::new(
            "The app is still on screen after it starts",
            up,
            format!(
                "After {} s the screen shows {}, not {package}.",
                SETTLE.as_secs(),
                if on_screen.is_empty() { "nothing readable".to_string() } else { on_screen.join(", ") }
            ),
        ));
        let note = if up {
            format!("{package} installed on {serial}, started and stayed on screen.")
        } else {
            // The crash log names why, when this mode may read it.
            let trace = cx
                .call("adb_log_read", json!({"serial": serial, "buffer": "crash", "count": 400}))
                .ok()
                .map(|o| crash_lines(&o.to_json().get("result").cloned().unwrap_or(Value::Null), &package))
                .unwrap_or_default();
            if trace.is_empty() {
                format!(
                    "{package} installed on {serial} and started, but it wasn't on screen {} s later (it crashed or closed). No crash log was read.",
                    SETTLE.as_secs()
                )
            } else {
                format!("{package} crashed after starting on {serial}:\n{}", trace.join("\n"))
            }
        };
        let mut r = CheckResult::new(id, round, up, note);
        r.claims = claims;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn detect(request: &str, target: &TargetRef, stack: &str) -> Option<u32> {
        AndroidKind.detect(&DetectCx::new(request, target, stack))
    }

    #[test]
    fn it_is_recognised_from_the_target_the_stack_and_the_words() {
        let none = TargetRef::None;
        for f in ["notes.apk", "Notes.APKS", "x.xapk", "app.aab"] {
            assert_eq!(detect("rebuild this", &TargetRef::Local { path: PathBuf::from(format!("/tmp/{f}")) }, ""), Some(150), "{f}");
        }
        assert_eq!(detect("rebuild this", &TargetRef::Local { path: PathBuf::from("/tmp/notes.exe") }, ""), None);
        assert_eq!(detect("a notes app", &none, "Flutter"), Some(150));
        assert_eq!(detect("make an android app for notes", &none, ""), Some(50));
        assert_eq!(detect("um aplicativo android de notas", &none, ""), Some(50));
        assert_eq!(detect("a notes app", &none, ""), None);
        assert_eq!(detect("a desktop app in kotlin", &none, ""), None);
    }

    #[test]
    fn the_device_and_crash_are_read_from_results() {
        let pick = |v: Value| check_pick(&json!({"devices": [], "check_pick": v}));
        assert_eq!(pick(json!({"serial": "emulator-5554", "avd": "Medium_Phone", "how": "first_emulator"})), Pick::Use("emulator-5554".into()));
        assert_eq!(pick(json!({"start": "Medium_Phone", "how": "started"})), Pick::Start("Medium_Phone".into()));
        assert_eq!(pick(json!({"none": {"reason": "no_avd"}, "message": "No phone."})), Pick::None("No phone.".into()));
        // A result without the pick is no device, never a guess from the list.
        assert!(matches!(check_pick(&json!({"devices": [{"serial": "x", "state": "device"}]})), Pick::None(_)));
        assert!(matches!(pick(Value::Null), Pick::None(_)));
        // A failed Play settles claims by its typed step; a device that didn't start settles none.
        let failed = |details: Value| play_failure(&ToolError::new(craft_anything_evidence::ErrorCode::ProgramFailed, "adb said no").with_details(details));
        let install = failed(json!({"step": "install", "install_failure": "INSTALL_FAILED_UPDATE_INCOMPATIBLE", "serial": "emulator-5554"})).unwrap();
        assert_eq!(install.len(), 1);
        let launch = failed(json!({"step": "launch", "serial": "emulator-5554"})).unwrap();
        assert_eq!(launch.len(), 2);
        assert!(failed(json!({"reason": "no_avd"})).is_none());
        assert!(play_failure(&ToolError::new(craft_anything_evidence::ErrorCode::TimedOut, "still booting")).is_none());
        let log = json!({"lines": ["noise", "E AndroidRuntime: FATAL EXCEPTION: main", "E AndroidRuntime: Process: com.me.notes, PID: 1", "E AndroidRuntime: java.lang.IllegalStateException: boom"]});
        let t = crash_lines(&log, "com.me.notes");
        assert_eq!(t.len(), 3);
        assert!(t[2].contains("IllegalStateException"));
        assert!(crash_lines(&json!({"lines": ["fine"]}), "com.me.notes").is_empty());
    }

    #[test]
    fn screen_differences_are_said_as_things_to_fix() {
        let changes = vec![
            json!({"index": 0, "original": true, "changed": false}),
            json!({"index": 1, "original": true, "changed": true, "controls_compared": true, "removed": ["Button \"Save\""], "added": ["Button \"OK\""]}),
            json!({"index": 2, "original": false, "changed": true}),
        ];
        assert!((likeness(&changes) - 50.0).abs() < 1e-9);
        let d = differences(&changes);
        assert_eq!(d.len(), 2);
        assert!(d[0].contains("missing Button \"Save\"") && d[0].contains("not in the original: Button \"OK\""), "{}", d[0]);
        assert!(d[1].contains("no screen here"));
        assert_eq!(likeness(&[]), 0.0);
    }

    #[test]
    fn every_android_stack_is_in_the_table() {
        for s in AndroidKind::STACKS {
            assert!(crate::rebuild::stacks::get(s).is_some(), "{s}");
        }
        assert!(AndroidKind::STACKS.contains(&AndroidKind::DEFAULT_STACK));
    }
}
