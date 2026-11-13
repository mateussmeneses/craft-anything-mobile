//! This computer's default Android device: the one Play and the Android checks use when none is
//! named. Kept in `<config_dir>/android-devices.json` ([`crate::android_device`]), never in a
//! project. It touches no target and starts nothing, so it runs in Clean room.

use super::android_sdk_tools::places;
use super::native_common::spec;
use crate::android_device::{self, DeviceRef, FileState, Seen};
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::{android_sdk, emulator};
use craft_anything_evidence::ToolError;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_SET_TOOL: &str = "adb_default_device_set";

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        DEFAULT_SET_TOOL,
        "Set the default Android device",
        "Choose the device project_play and the Android checks use when none is named: a phone's adb serial (adb_devices_find) or a virtual device's name (adb_emulators_list; an emulator is kept by name, since its emulator-5554 serial changes from boot to boot). `clear` removes the default; with neither, says what it is. Kept for this computer only (android-devices.json beside config.toml), never in a project. Starts nothing and contacts no device.",
        "Choose which phone or emulator your apps open on.",
        Toolset::Adb,
        Effects::WRITES_FILESYSTEM,
        Requires::Nothing,
        schema_of::<DefaultParams>,
        adb_default_device_set,
    )]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DefaultParams {
    /// A phone's adb serial or a virtual device's name. Leave out to only read the default.
    #[serde(default)]
    device: Option<String>,
    /// Remove the default.
    #[serde(default)]
    clear: bool,
}

/// What a kept default looks like in a result.
fn shown(d: &Option<DeviceRef>) -> Value {
    serde_json::to_value(d).unwrap_or(Value::Null)
}

fn adb_default_device_set(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: DefaultParams = parse_params(params)?;
    let path = android_device::path_in(&ctx.session.config_dir());
    let set = match (&p.device, p.clear) {
        (Some(_), true) => return Err(ToolError::invalid("Pass either device or clear, not both.")),
        (None, false) => None,
        (None, true) => Some(None),
        (Some(text), false) => {
            if emulator::is_emulator_serial(text) {
                return Err(ToolError::invalid(format!("{text} is an emulator's serial, which changes from boot to boot."))
                    .with_remediation("Pass the virtual device's name instead, as adb_emulators_list shows it."));
            }
            // Only the virtual devices this computer has are looked at: no adb, no device.
            let avds = android_sdk::find_avds(&places(ctx.session, None, None).avd_homes);
            let seen = Seen { avds: avds.iter().flat_map(|a| a.devices.iter()).map(|d| d.name.clone()).collect(), ..Seen::default() };
            Some(Some(DeviceRef::parse(text, &seen)?))
        }
    };
    let mut set_aside = None;
    if let Some(default) = set {
        set_aside = android_device::save_default(&path, default)?.set_aside;
    }
    let loaded = android_device::load(&path);
    let note = match &loaded.state {
        FileState::Current => None,
        FileState::Newer(v) => Some(format!("The file was written by a newer Craft Anything (version {v}); it is read as far as it can be and left as it is.")),
        FileState::Broken(why) => Some(format!("The file can't be read ({why}), so there is no default; setting one moves it aside.")),
    };
    Ok(ToolOutput::Value(json!({
        "default": shown(&loaded.file.default),
        "changed": p.device.is_some() || p.clear,
        "file": path.display().to_string(),
        "set_aside": set_aside.map(|p| p.display().to_string()),
        "note": note,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Caller, Engine};

    fn call(e: &Engine, p: Value) -> Result<Value, ToolError> {
        e.call(DEFAULT_SET_TOOL, p, CallOptions::new(Caller::Mcp)).map(|o| o.to_json()["result"].clone())
    }

    #[test]
    fn the_default_is_set_read_and_cleared_in_the_sessions_config_folder() {
        let mut s = crate::testing::session();
        s.settings.deep_mode = false;
        let e = Engine::new(s);
        let path = android_device::path_in(&e.session().config_dir());
        assert_eq!(call(&e, json!({})).unwrap()["default"], Value::Null);
        assert!(!path.exists(), "reading writes nothing");
        let r = call(&e, json!({"device": "R58M12345"})).unwrap();
        assert_eq!(r["default"], json!({"serial": "R58M12345"}), "{r}");
        assert_eq!(call(&e, json!({})).unwrap()["default"], json!({"serial": "R58M12345"}));
        assert!(path.is_file());
        assert_eq!(call(&e, json!({"clear": true})).unwrap()["default"], Value::Null);
    }

    #[test]
    fn an_emulator_serial_or_a_bad_name_is_refused_before_anything_is_written() {
        let e = Engine::new(crate::testing::session());
        for bad in [json!({"device": "emulator-5554"}), json!({"device": "a b"}), json!({"device": "x", "clear": true}), json!({"nope": 1})] {
            assert_eq!(call(&e, bad.clone()).unwrap_err().code, craft_anything_evidence::ErrorCode::BadInput, "{bad}");
        }
        assert!(!android_device::path_in(&e.session().config_dir()).exists());
    }
}
