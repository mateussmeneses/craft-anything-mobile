//! What state an app is in on a device (`adb_app_state_show`): the activity on screen, the back
//! stack of each task, whether the app is running, and the permissions it asked for and holds.
//!
//! Read from `dumpsys activity activities`, `dumpsys package <pkg>` and `pidof <pkg>`. Activity
//! and permission names are the app's own, from inside it, so this runs only in Deep mode.

use super::adb_tools::{adb_for, dev_ev, ensure_online, serial_of};
use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::adb;
use craft_anything_engines::android_state::{self, PermissionKind};
use craft_anything_evidence::{Coverage, ErrorCode, Limitation, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub const STATE_TOOL: &str = "adb_app_state_show";

/// How long the device gets for one dumpsys.
const DUMP_TIMEOUT: Duration = Duration::from_secs(45);

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        STATE_TOOL,
        "App state on device",
        "Say what state apps are in on a device: the activity on screen and taking input, every task's back stack (top first, as the back button walks it), and, for package (the app on screen when none is given), whether it is running (its process ids) and its permissions: those it asks for, and those the device holds for it, install and run-time, each granted or not. Read from dumpsys; changes nothing.",
        "See which screen of an app is open, what's behind it, and which permissions it has.",
        Toolset::Adb,
        Effects::MUTATES_SESSION | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK | Effects::CARRIES_TARGET_CONTENT,
        Requires::Engine("adb"),
        schema_of::<StateParams>,
        adb_app_state_show,
    )]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StateParams {
    /// Device serial from adb_devices_find (e.g. emulator-5554).
    serial: String,
    /// The app to report on (e.g. com.example.app); the app on screen when left out.
    #[serde(default)]
    package: Option<String>,
}

fn adb_app_state_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: StateParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let asked = p.package.as_deref().map(adb::validate_package).transpose()?.map(str::to_string);
    let adb = adb_for(ctx)?.timeout(DUMP_TIMEOUT).idle_timeout(DUMP_TIMEOUT);
    ensure_online(&adb, serial)?;

    let out = adb.shell(serial, "dumpsys activity activities")?;
    let text = out.stdout_text();
    if !out.success && text.trim().is_empty() {
        return Err(
            ToolError::new(ErrorCode::ProgramFailed, "The device did not list its activities.").with_remediation("Check the device is unlocked and try again.")
        );
    }
    let stack = android_state::parse_activities(&text);
    let mut lims = Vec::new();
    if stack.tasks.is_empty() {
        lims.push(Limitation::new(
            "no_tasks_read",
            "No task was read from the device's listing; its format may differ from the Android versions this reads (checked on Android 17).",
        ));
    }
    if stack.truncated {
        lims.push(Limitation::new(
            "truncated",
            format!("Lists were cut at {} tasks of {} activities.", android_state::MAX_TASKS, android_state::MAX_ACTIVITIES),
        ));
    }

    let package = asked.clone().or_else(|| stack.resumed.as_ref().map(|r| r.package.clone()));
    let mut app = Value::Null;
    if let Some(pkg) = &package {
        // Checked again: a package taken from the device's own listing is still an outside value.
        let pkg = adb::validate_package(pkg)?;
        let pids: Vec<u32> = adb
            .shell(serial, &format!("pidof {pkg}"))
            .map(|o| o.stdout_text().split_whitespace().filter_map(|w| w.parse().ok()).take(64).collect())
            .unwrap_or_default();
        let dump = adb.shell(serial, &format!("dumpsys package {pkg}"))?;
        let perms = android_state::parse_permissions(&dump.stdout_text(), pkg);
        if !perms.found {
            lims.push(Limitation::new("package_not_listed", format!("The device's package listing doesn't name {pkg}; it may not be installed.")));
        }
        if perms.truncated {
            lims.push(Limitation::new("permissions_truncated", format!("Permissions were cut at {}.", android_state::MAX_PERMISSIONS)));
        }
        let runtime_denied: Vec<&str> =
            perms.held.iter().filter(|x| x.kind == PermissionKind::Runtime && x.granted == Some(false)).map(|x| x.name.as_str()).collect();
        let granted = perms.held.iter().filter(|x| x.granted == Some(true)).count();
        app = json!({
            "package": pkg,
            "installed": perms.found,
            "running": !pids.is_empty(),
            "pids": pids,
            "on_screen": stack.resumed.as_ref().is_some_and(|r| r.package == pkg),
            "tasks": stack.tasks_of(pkg).map(|t| t.id).collect::<Vec<_>>(),
            "requested_permissions": perms.requested,
            "held_permissions": perms.held,
            "granted_count": granted,
            "runtime_denied": runtime_denied,
        });
    } else {
        lims.push(Limitation::new("no_app", "No app was named and none is on screen, so no app's permissions were read."));
    }

    let summary = match (&stack.resumed, &app) {
        (Some(r), Value::Object(a)) => format!(
            "{} is on screen; {} task(s). {}: {}, {} permission(s) granted.",
            r.class,
            stack.tasks.len(),
            a.get("package").and_then(Value::as_str).unwrap_or_default(),
            if a.get("running").and_then(Value::as_bool) == Some(true) { "running" } else { "not running" },
            a.get("granted_count").and_then(Value::as_u64).unwrap_or(0)
        ),
        (Some(r), _) => format!("{} is on screen; {} task(s).", r.class, stack.tasks.len()),
        (None, _) => format!("No activity is on screen; {} task(s).", stack.tasks.len()),
    };
    let e = dev_ev(ctx, STATE_TOOL, serial, json!({"serial": serial, "package": asked}))
        .result(json!({"resumed": stack.resumed, "tasks": stack.tasks, "app": app}))
        .summary(summary)
        .limitations(lims)
        .coverage(Coverage::complete(stack.tasks.len() as u64))
        .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use crate::tools::native_common::test_support::call_err;

    fn engine(deep: bool, with_adb: bool) -> (Engine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut s = crate::testing::session();
        s.settings.deep_mode = deep;
        if with_adb {
            let adb = dir.path().join("adb.exe");
            std::fs::write(&adb, b"stand-in").unwrap();
            s.settings.engines.tools.adb = Some(adb);
        }
        (Engine::new(s), dir)
    }

    #[test]
    fn it_runs_only_in_deep_mode() {
        let (e, _d) = engine(false, true);
        let err = call_err(&e, STATE_TOOL, json!({"serial": "emulator-5554"}));
        assert_ne!(err.code, ErrorCode::BadInput);
        assert!(crate::clean_room::TOOLS.iter().all(|t| *t != STATE_TOOL));
    }

    #[test]
    fn bad_input_is_refused_before_the_device_is_asked() {
        let (e, _d) = engine(true, true);
        for params in
            [json!({"serial": "a b"}), json!({"serial": "emulator-5554", "package": "x; reboot"}), json!({"serial": "emulator-5554", "pkg": "a"}), json!({})]
        {
            assert_eq!(call_err(&e, STATE_TOOL, params.clone()).code, ErrorCode::BadInput, "{params}");
        }
    }

    #[test]
    fn it_needs_adb() {
        let (e, _d) = engine(true, false);
        assert_eq!(call_err(&e, STATE_TOOL, json!({"serial": "emulator-5554"})).code, ErrorCode::EngineMissing);
    }
}
