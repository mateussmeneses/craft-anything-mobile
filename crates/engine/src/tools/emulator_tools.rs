//! Android virtual devices: which exist, which are running, starting one and stopping one.
//!
//! The emulator is a long-running program, so `adb_emulator_start` starts it with
//! [`craft_anything_platform::process::Launch`] (no console, output to a log file, nothing
//! inherited) and returns once the device has finished booting, the wait ran out, or the person
//! stopped the call; the emulator itself keeps running until `adb_emulator_stop` or the person
//! closes it. Everything asked of a running emulator goes through the bounded adb adapter.
//!
//! Nothing here reads a target: the tools start, stop and list devices, so they run in Clean room.

use super::android_sdk_tools::places;
use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::adb::{self, Adb};
use craft_anything_engines::android_sdk;
use craft_anything_engines::emulator::{self, StartOptions};
use craft_anything_evidence::{Authority, Confidence, Coverage, Environment, ErrorCode, EvidenceBuilder, Limitation, Location, Subject, ToolError};
use craft_anything_platform::process::{Cancel, Launch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const LIST_TOOL: &str = "adb_emulators_list";
pub const START_TOOL: &str = "adb_emulator_start";
pub const STOP_TOOL: &str = "adb_emulator_stop";

/// How long `adb_emulator_start` waits for the device to finish booting unless told otherwise.
const DEFAULT_WAIT_SECS: u32 = 240;
/// The longest wait it accepts.
const MAX_WAIT_SECS: u32 = 900;
/// How often a starting device is asked whether it has booted.
const POLL: Duration = Duration::from_secs(2);
/// How long one question to adb or the emulator console may take.
const QUICK: Duration = Duration::from_secs(15);
/// How long a stopped emulator is given to leave adb's list.
const STOP_WAIT: Duration = Duration::from_secs(30);
/// Most characters of the emulator's log put into an error.
const LOG_TAIL: usize = 1500;

pub fn specs() -> Vec<ToolSpec> {
    use Toolset::Adb;
    let read = Effects::MUTATES_SESSION | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK;
    vec![
        spec(
            LIST_TOOL,
            "Virtual devices",
            "List the Android virtual devices (AVDs) on this computer with their system image, ABI, screen and memory, and say which are running: for each emulator adb sees, its serial, the virtual device it runs and whether it has finished booting. Without adb the devices are still listed; which are running is then unknown.",
            "See which virtual Android phones this computer has and which are switched on.",
            Adb,
            read,
            Requires::Nothing,
            schema_of::<ListParams>,
            adb_emulators_list,
        ),
        spec(
            START_TOOL,
            "Start a virtual device",
            "Start an Android virtual device by name with the SDK's emulator and wait until it has finished booting (`sys.boot_completed`), up to wait_seconds (default 240, at most 900). Returns its adb serial. A device that is already running is not started again. The emulator keeps running after this returns; stop it with adb_emulator_stop. no_window runs it without a window; cold_boot ignores the saved quick-boot state.",
            "Switch on a virtual Android phone and wait until it is ready.",
            Adb,
            read | Effects::WRITES_FILESYSTEM,
            Requires::Engine("adb"),
            schema_of::<StartParams>,
            adb_emulator_start,
        ),
        spec(
            STOP_TOOL,
            "Stop a virtual device",
            "Shut down a running emulator by its adb serial (`adb emu kill`) and wait until adb no longer lists it. Only emulator serials (emulator-5554) are accepted; a phone is never touched. Changes the device (it is switched off).",
            "Switch off a virtual Android phone.",
            Adb,
            read | Effects::MUTATES_DEVICE,
            Requires::Engine("adb"),
            schema_of::<StopParams>,
            adb_emulator_stop,
        ),
    ]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListParams {
    /// An SDK folder to use instead of looking for one.
    #[serde(default)]
    sdk_root: Option<PathBuf>,
    /// The folder that holds the virtual devices' `.ini` files, instead of the usual one.
    #[serde(default)]
    avd_home: Option<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StartParams {
    /// The virtual device's name, as adb_emulators_list shows it.
    name: String,
    /// Run without a window.
    #[serde(default)]
    no_window: bool,
    /// Boot from scratch instead of from the saved quick-boot state.
    #[serde(default)]
    cold_boot: bool,
    /// How long to wait for the boot to finish, in seconds (default 240, at most 900).
    #[serde(default)]
    wait_seconds: Option<u32>,
    /// An SDK folder to use instead of looking for one.
    #[serde(default)]
    sdk_root: Option<PathBuf>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StopParams {
    /// The emulator's adb serial, e.g. emulator-5554.
    serial: String,
}

/// An emulator adb sees.
#[derive(Debug, Clone, Serialize)]
struct Running {
    serial: String,
    /// adb's state for it: device, offline…
    state: String,
    /// The virtual device it runs; `None` when its console didn't say.
    name: Option<String>,
    /// Whether it has finished booting; `None` when it couldn't be asked.
    booted: Option<bool>,
}

fn cancel_of(ctx: &Ctx<'_>) -> Cancel {
    let c = ctx.cancel.clone();
    Arc::new(move || c.is_cancelled())
}

/// The adb this session uses, with short deadlines: every question here is a quick one.
fn quick_adb(ctx: &Ctx<'_>) -> Option<Adb> {
    ctx.session.settings.engine_path("adb").map(|path| Adb::new(path).cancel(cancel_of(ctx)).timeout(QUICK).idle_timeout(QUICK))
}

fn adb_missing() -> ToolError {
    ToolError::new(ErrorCode::EngineMissing, "Android platform-tools (adb) isn't installed.")
        .with_remediation("Install it from Settings → Engines, or run engines_install with engine \"adb\".")
}

fn env(ctx: &Ctx<'_>) -> Environment {
    Environment {
        clean_room: ctx.session.clean_room(),
        engine: "adb".into(),
        engine_version: None,
        host: craft_anything_evidence::host_triple(),
        language_id: None,
    }
}

/// The emulators adb lists, each asked for its virtual device's name and whether it has booted.
/// A device that doesn't answer keeps `None`s; only a failing `adb devices` is an error.
fn running(adb: &Adb) -> Result<Vec<Running>, ToolError> {
    let mut out = Vec::new();
    for d in adb.devices()?.into_iter().filter(|d| emulator::is_emulator_serial(&d.serial)).take(64) {
        let online = d.is_online();
        let name = online.then(|| adb.targeted(&d.serial, ["emu", "avd", "name"]).ok()).flatten().and_then(|o| emulator::parse_avd_name(&o.stdout_text()));
        let booted = online.then(|| booted(adb, &d.serial)).flatten();
        out.push(Running { serial: d.serial, state: d.state, name, booted });
    }
    Ok(out)
}

/// Whether a device says it finished booting; `None` when it didn't answer.
fn booted(adb: &Adb, serial: &str) -> Option<bool> {
    let out = adb.shell(serial, "getprop sys.boot_completed").ok()?;
    out.success.then(|| emulator::boot_completed(&out.stdout_text()))
}

fn adb_emulators_list(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: ListParams = parse_params(params)?;
    let places = places(ctx.session, p.sdk_root.clone(), p.avd_home.clone());
    let avds = android_sdk::find_avds(&places.avd_homes);
    let can_start = android_sdk::find(&places).as_ref().and_then(emulator::program).is_some();
    let mut lims = Vec::new();
    let running = match quick_adb(ctx) {
        Some(adb) => Some(running(&adb)?),
        None => {
            lims.push(Limitation::new("adb_missing", "adb isn't installed, so which devices are running is unknown."));
            None
        }
    };
    if avds.is_none() {
        lims.push(Limitation::new("no_avd_folder", "No folder of virtual devices was found, so none are listed; that is not the same as having none."));
    }
    if avds.as_ref().is_some_and(|a| a.truncated) {
        lims.push(Limitation::new("truncated", format!("The folder had more than {} entries; the list may be short.", android_sdk::MAX_ENTRIES)));
    }
    if !can_start {
        lims.push(Limitation::new("no_emulator", "No Android SDK with an emulator was found, so adb_emulator_start can't start these."));
    }
    let devices: Vec<Value> = avds
        .iter()
        .flat_map(|a| a.devices.iter())
        .map(|d| {
            let on = running.as_ref().map(|r| r.iter().filter(|x| x.name.as_deref() == Some(d.name.as_str())).map(|x| x.serial.clone()).collect::<Vec<_>>());
            let mut v = serde_json::to_value(d).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut v {
                map.insert("running_as".into(), json!(on));
            }
            v
        })
        .collect();
    let on = running.as_ref().map(Vec::len);
    let summary = match on {
        Some(n) => format!("{} virtual device(s), {n} emulator(s) running.", devices.len()),
        None => format!("{} virtual device(s); which are running is unknown without adb.", devices.len()),
    };
    let locations: Vec<Location> =
        running.iter().flatten().map(|r| Location::new("device", &r.serial).with_label(r.name.clone().unwrap_or_else(|| r.state.clone()))).collect();
    let count = devices.len() as u64;
    let e = EvidenceBuilder::new(LIST_TOOL, Subject { kind: "engine".into(), sha256: None, name: Some("android-emulator".into()) }, env(ctx))
        .params(json!({"sdk_root": p.sdk_root, "avd_home": p.avd_home}))
        .authority(Authority::OurRun)
        .confidence(Confidence::Observed)
        .result(json!({
            "virtual_devices": devices,
            "avd_home": avds.as_ref().map(|a| a.home.clone()),
            "running": running,
            "can_start": can_start,
        }))
        .summary(summary)
        .limitations(lims)
        .locations(locations)
        .coverage(Coverage::complete(count))
        .build()?;
    Ok(ToolOutput::evidence(e))
}

/// The last part of the emulator's log, for an error: what it said when it gave up.
fn log_tail(log: &std::path::Path) -> String {
    let text = std::fs::read(log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let n = text.chars().count();
    text.chars().skip(n.saturating_sub(LOG_TAIL)).collect::<String>().trim().to_string()
}

fn adb_emulator_start(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: StartParams = parse_params(params)?;
    let name = emulator::validate_avd_name(&p.name)?.to_string();
    let wait = p.wait_seconds.unwrap_or(DEFAULT_WAIT_SECS);
    if wait > MAX_WAIT_SECS {
        return Err(ToolError::invalid(format!("wait_seconds can be at most {MAX_WAIT_SECS}.")));
    }
    let places = places(ctx.session, p.sdk_root.clone(), None);
    let program = android_sdk::find(&places).as_ref().and_then(emulator::program).ok_or_else(|| {
        ToolError::new(ErrorCode::EngineMissing, "No Android SDK with an emulator was found on this computer.").with_remediation(
            "Install the Android Emulator (Android Studio → SDK Manager → SDK Tools), or pass sdk_root. engines_android_sdk_check shows what was found.",
        )
    })?;
    let adb = quick_adb(ctx).ok_or_else(adb_missing)?;
    let known = android_sdk::find_avds(&places.avd_homes);
    let device = known.as_ref().and_then(|a| a.devices.iter().find(|d| d.name == name));
    match device {
        Some(d) if d.usable => {}
        Some(_) => {
            return Err(ToolError::new(ErrorCode::NotFound, format!("The virtual device {name} has no configuration on this computer."))
                .with_remediation("Recreate it in Android Studio's Device Manager."));
        }
        None => {
            let names: Vec<&str> = known.iter().flat_map(|a| a.devices.iter()).map(|d| d.name.as_str()).take(32).collect();
            let have = if names.is_empty() { "This computer has none.".to_string() } else { format!("This computer has: {}.", names.join(", ")) };
            return Err(ToolError::new(ErrorCode::NotFound, format!("There is no virtual device named {name}. {have}"))
                .with_remediation("Create one in Android Studio's Device Manager, or pick a name adb_emulators_list shows."));
        }
    }

    let options = StartOptions { no_window: p.no_window, cold_boot: p.cold_boot };
    let args = emulator::start_args(&name, options);
    let before = running(&adb)?;
    let already = before.iter().find(|r| r.name.as_deref() == Some(name.as_str())).cloned();
    let started = Instant::now();
    let (launched, log) = match &already {
        Some(_) => (None, None),
        None => {
            let dir = ctx.session.config_dir().join("logs");
            std::fs::create_dir_all(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
            let log = dir.join(format!("emulator-{name}.log"));
            ctx.report(None, format!("Starting the virtual device {name}…"));
            let launched = Launch::new(&program)
                .args(&args)
                .hide_console()
                .log_to(&log)
                .label("the Android emulator")
                .remediation("Reinstall the Android Emulator from Android Studio's SDK Manager.")
                .start_watched()?;
            (Some(launched), Some(log))
        }
    };

    // Wait for the device to appear in adb under this name, then for its boot to finish.
    let deadline = started + Duration::from_secs(u64::from(wait));
    let mut seen = already.clone();
    loop {
        ctx.cancel.check()?;
        if let Some(r) = &seen
            && r.booted == Some(true)
        {
            break;
        }
        // A launcher that gave up says why in its log; one that handed the device to another
        // process and left with code 0 is still starting.
        if let (Some(l), Some(log)) = (&launched, &log)
            && let Some(ended) = l.ended()
            && ended.code != Some(0)
            && seen.is_none()
        {
            let code = ended.code.map_or_else(|| "it was stopped".to_string(), |c| format!("exit code {c}"));
            let said = log_tail(log);
            let said = if said.is_empty() { String::new() } else { format!(" It printed:\n{said}") };
            return Err(ToolError::new(ErrorCode::ProgramFailed, format!("The Android emulator closed before {name} started ({code}).{said}"))
                .with_remediation(format!("Its whole output is in {}.", log.display())));
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        // adb may be restarting its server while the emulator registers; a failed look is tried again.
        if let Ok(now) = running(&adb) {
            seen = now.into_iter().find(|r| r.name.as_deref() == Some(name.as_str())).or(seen.filter(|_| false));
            if let Some(r) = &seen {
                ctx.report(None, if r.booted == Some(true) { format!("{name} is ready.") } else { format!("{name} is booting as {}…", r.serial) });
            }
        }
    }

    let booted = seen.as_ref().is_some_and(|r| r.booted == Some(true));
    let waited = started.elapsed().as_secs();
    let mut lims = Vec::new();
    if already.is_none() {
        lims.push(Limitation::new("keeps_running", "The emulator keeps running after this call; stop it with adb_emulator_stop."));
    }
    if !booted {
        lims.push(Limitation::new(
            "boot_not_seen",
            match &seen {
                Some(r) => format!("{} did not report a finished boot within {wait} s; it may still be starting.", r.serial),
                None => format!("No emulator named {name} appeared in adb within {wait} s; it may still be starting."),
            },
        ));
    }
    let summary = match (&seen, booted, &already) {
        (Some(r), true, Some(_)) => format!("{name} was already running as {}.", r.serial),
        (Some(r), true, None) => format!("Started {name} as {}; it booted in {waited} s.", r.serial),
        (Some(r), false, _) => format!("{name} is running as {} but had not finished booting after {waited} s.", r.serial),
        (None, _, _) => format!("The emulator was started, but {name} had not appeared in adb after {waited} s."),
    };
    let subject = Subject { kind: "device".into(), sha256: None, name: Some(seen.as_ref().map_or_else(|| name.clone(), |r| r.serial.clone())) };
    let e = EvidenceBuilder::new(START_TOOL, subject, env(ctx))
        .params(json!({"name": name, "no_window": p.no_window, "cold_boot": p.cold_boot, "wait_seconds": wait, "sdk_root": p.sdk_root}))
        .authority(Authority::OurRun)
        .confidence(Confidence::Observed)
        .result(json!({
            "name": name,
            "serial": seen.as_ref().map(|r| r.serial.clone()),
            "booted": booted,
            "already_running": already.is_some(),
            "waited_seconds": waited,
            "program": program,
            "args": if already.is_some() { None } else { Some(args) },
            "pid": launched.as_ref().map(|l| l.pid()),
            "log": log,
        }))
        .summary(summary)
        .limitations(lims)
        .locations(seen.iter().map(|r| Location::new("device", &r.serial).with_label(name.clone())).collect::<Vec<_>>())
        .coverage(Coverage::complete(1))
        .build()?;
    Ok(ToolOutput::evidence(e))
}

fn adb_emulator_stop(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: StopParams = parse_params(params)?;
    let serial = adb::validate_serial(&p.serial)?;
    if !emulator::is_emulator_serial(serial) {
        return Err(ToolError::invalid(format!("{serial} is not an emulator. Only virtual devices (emulator-5554) can be stopped here."))
            .with_remediation("adb_emulators_list shows the emulators that are running."));
    }
    let adb = quick_adb(ctx).ok_or_else(adb_missing)?;
    let Some(target) = running(&adb)?.into_iter().find(|r| r.serial == serial) else {
        return Err(ToolError::new(ErrorCode::NotFound, format!("No emulator is running as {serial}."))
            .with_remediation("adb_emulators_list shows the emulators that are running."));
    };
    let out = adb.targeted(serial, ["emu", "kill"])?;
    let said = format!("{}{}", out.stdout_text(), out.stderr_text());
    let started = Instant::now();
    let mut gone = false;
    while started.elapsed() < STOP_WAIT {
        ctx.cancel.check()?;
        if adb.devices().is_ok_and(|d| !d.iter().any(|d| d.serial == serial)) {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !gone {
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("nothing").trim().to_string();
        return Err(ToolError::new(
            ErrorCode::ProgramFailed,
            format!("{serial} was asked to shut down but adb still lists it after {} s. The emulator said: {last}", STOP_WAIT.as_secs()),
        )
        .with_remediation("Close the emulator's window, or try again in a moment."));
    }
    let e = EvidenceBuilder::new(STOP_TOOL, Subject { kind: "device".into(), sha256: None, name: Some(serial.to_string()) }, env(ctx))
        .params(json!({"serial": serial}))
        .authority(Authority::OurRun)
        .confidence(Confidence::Observed)
        .result(json!({"serial": serial, "name": target.name, "stopped": true, "waited_ms": started.elapsed().as_millis() as u64}))
        .summary(format!("Stopped {}.", target.name.clone().map_or_else(|| serial.to_string(), |n| format!("{n} ({serial})"))))
        .limitation(Limitation::new("device_changed", "This switched a virtual device off."))
        .coverage(Coverage::complete(1))
        .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native_common::test_support::{call, call_err};
    use crate::{CallOptions, Caller, Engine};
    use std::fs;
    use std::path::Path;

    fn avds() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("Phone.ini"), "target=android-37.0\n").unwrap();
        fs::create_dir_all(t.path().join("Phone.avd")).unwrap();
        fs::write(t.path().join("Phone.avd").join("config.ini"), "abi.type=x86_64\nhw.lcd.density=420\n").unwrap();
        t
    }

    /// An SDK folder with stand-in files where the emulator and adb would be. Nothing in these
    /// tests gets far enough to start either.
    fn sdk_with_programs() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::Builder::new().prefix("sdk").tempdir().unwrap();
        for dir in ["emulator", "platform-tools"] {
            fs::create_dir_all(t.path().join(dir)).unwrap();
        }
        fs::write(t.path().join("emulator").join(if cfg!(windows) { "emulator.exe" } else { "emulator" }), b"stand-in").unwrap();
        let adb = t.path().join("platform-tools").join(if cfg!(windows) { "adb.exe" } else { "adb" });
        fs::write(&adb, b"stand-in").unwrap();
        (t, adb)
    }

    fn engine_with_adb(adb: &Path) -> Engine {
        let mut s = crate::testing::session();
        s.settings.engines.tools.adb = Some(adb.to_path_buf());
        Engine::new(s)
    }

    #[test]
    fn devices_are_listed_without_adb_and_running_is_then_unknown() {
        let avds = avds();
        let mut s = crate::testing::session();
        s.settings.deep_mode = false;
        let v = call(&Engine::new(s), LIST_TOOL, json!({"avd_home": avds.path()}));
        let r = &v["result"];
        assert_eq!(r["virtual_devices"][0]["name"], "Phone");
        assert!(r["virtual_devices"][0]["running_as"].is_null(), "unknown, not an empty list");
        assert!(r["running"].is_null());
        assert_eq!(r["can_start"], false);
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert_eq!(codes, ["adb_missing", "no_emulator"]);
        assert_eq!(v["environment"]["clean_room"], true);
    }

    #[test]
    fn no_device_folder_is_said_not_shown_as_no_devices() {
        let v = call(&Engine::new(crate::testing::session()), LIST_TOOL, json!({}));
        assert_eq!(v["result"]["virtual_devices"].as_array().map(Vec::len), Some(0));
        assert!(v["result"]["avd_home"].is_null());
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"no_avd_folder"), "{codes:?}");
    }

    #[test]
    fn start_refuses_a_bad_name_or_wait_before_looking_for_anything() {
        let (_sdk, adb) = sdk_with_programs();
        let e = engine_with_adb(&adb);
        for name in ["", "-no-window", "a b", "a;rm"] {
            assert_eq!(call_err(&e, START_TOOL, json!({"name": name})).code, ErrorCode::BadInput, "{name:?}");
        }
        assert_eq!(call_err(&e, START_TOOL, json!({"name": "Phone", "wait_seconds": MAX_WAIT_SECS + 1})).code, ErrorCode::BadInput);
        assert_eq!(call_err(&e, START_TOOL, json!({"name": "Phone", "avd_home": "/x"})).code, ErrorCode::BadInput, "an unknown field");
    }

    #[test]
    fn start_without_an_emulator_says_the_emulator_is_missing() {
        let (sdk, adb) = sdk_with_programs();
        fs::remove_file(sdk.path().join("emulator").join(if cfg!(windows) { "emulator.exe" } else { "emulator" })).unwrap();
        let err = call_err(&engine_with_adb(&adb), START_TOOL, json!({"name": "Phone"}));
        assert_eq!(err.code, ErrorCode::EngineMissing);
        assert!(err.message.contains("emulator"), "{}", err.message);
    }

    #[test]
    fn start_of_an_unknown_device_is_not_found_and_starts_nothing() {
        let (sdk, adb) = sdk_with_programs();
        let e = engine_with_adb(&adb);
        let err = call_err(&e, START_TOOL, json!({"name": "Nope"}));
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("Nope"), "{}", err.message);
        assert!(!e.session().config_dir().join("logs").join("emulator-Nope.log").exists(), "no emulator was started");
        drop(sdk);
    }

    #[test]
    fn stop_takes_only_emulator_serials() {
        let (_sdk, adb) = sdk_with_programs();
        let e = engine_with_adb(&adb);
        let consented =
            |serial: &str| e.call(STOP_TOOL, json!({"serial": serial}), CallOptions::new(Caller::Mcp).with_consent(crate::Consent::Granted)).unwrap_err();
        for serial in ["R58M12345", "192.168.0.5:5555", "emulator-55x4"] {
            assert_eq!(consented(serial).code, ErrorCode::BadInput, "{serial}");
        }
    }

    #[test]
    fn stop_changes_a_device_so_it_asks_first() {
        let (_sdk, adb) = sdk_with_programs();
        let err = call_err(&engine_with_adb(&adb), STOP_TOOL, json!({"serial": "emulator-5554"}));
        assert_eq!(err.code, ErrorCode::ConsentRequired);
    }

    #[test]
    fn start_and_stop_need_adb() {
        let e = Engine::new(crate::testing::session());
        assert_eq!(call_err(&e, START_TOOL, json!({"name": "Phone"})).code, ErrorCode::EngineMissing);
        let err = e.call(STOP_TOOL, json!({"serial": "emulator-5554"}), CallOptions::new(Caller::Mcp).with_consent(crate::Consent::Granted)).unwrap_err();
        assert_eq!(err.code, ErrorCode::EngineMissing);
    }
}
