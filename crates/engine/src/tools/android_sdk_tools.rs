//! The Android SDK and its virtual devices, as this computer has them.
//!
//! `engines_android_sdk_check` reads folders only (`craft_anything_engines::android_sdk`): it runs
//! no SDK program and contacts no device, so it answers at once, touches no target and runs in
//! Clean room. Where it looks follows the engine lookup: a session that finds only configured
//! engines (`engines.only_configured`, as tests use) looks at a folder it is given and at the
//! configured adb's own SDK, never at this computer's environment.

use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::android_sdk::{self, Places, RootSource};
use craft_anything_evidence::{Authority, Confidence, Coverage, Environment, EvidenceBuilder, Limitation, Location, Subject, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

/// The tool's id, for code that needs to name it.
pub const SDK_CHECK_TOOL: &str = "engines_android_sdk_check";

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        SDK_CHECK_TOOL,
        "Android SDK on this computer",
        "Find the Android SDK (sdk_root if given, then the SDK of the adb in use, ANDROID_HOME, ANDROID_SDK_ROOT and the usual install folder) and report what it holds: platform tools, emulator, build tools, platforms, NDKs, command-line tools and system images, each with the revision it states. Also lists the virtual devices (AVDs) with their system image, ABI, screen size, density and memory. Reads folders only: runs no SDK program and contacts no device.",
        "See whether this computer can build Android apps and which virtual phones it has.",
        Toolset::Setup,
        Effects::MUTATES_SESSION,
        Requires::Nothing,
        schema_of::<SdkCheckParams>,
        engines_android_sdk_check,
    )]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SdkCheckParams {
    /// An SDK folder to read instead of looking for one.
    #[serde(default)]
    sdk_root: Option<PathBuf>,
    /// The folder that holds the virtual devices' `.ini` files, instead of the usual one.
    #[serde(default)]
    avd_home: Option<PathBuf>,
}

/// Where this session looks for an SDK and its virtual devices: the folders given, the configured
/// adb's own SDK, and (unless the session finds only configured engines) this computer's
/// environment and usual folders.
pub(crate) fn places(session: &crate::Session, sdk_root: Option<PathBuf>, avd_home: Option<PathBuf>) -> Places {
    let settings = &session.settings;
    let mut places = if settings.engines.only_configured { Places::default() } else { Places::here() };
    places.adb = settings.engine_path("adb");
    places.given = sdk_root;
    if let Some(home) = avd_home {
        places.avd_homes = vec![home];
    }
    places
}

fn engines_android_sdk_check(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: SdkCheckParams = parse_params(params)?;
    let places = places(ctx.session, p.sdk_root.clone(), p.avd_home.clone());

    let sdk = android_sdk::find(&places);
    let avds = android_sdk::find_avds(&places.avd_homes);
    let mut lims = vec![Limitation::new(
        "folders_only",
        "Read from folders and their source.properties files. No SDK program was run, so licences, updates and whether each tool starts were not checked.",
    )];
    if let Some(given) = &p.sdk_root
        && sdk.as_ref().is_none_or(|s| s.source != RootSource::Given)
    {
        lims.push(Limitation::new(
            "given_folder_not_an_sdk",
            format!("{} has none of an SDK's folders (platform-tools, build-tools, platforms, cmdline-tools).", given.display()),
        ));
    }
    if sdk.as_ref().is_some_and(|s| s.truncated) || avds.as_ref().is_some_and(|a| a.truncated) {
        lims.push(Limitation::new("truncated", format!("A folder had more than {} entries; the lists may be short.", android_sdk::MAX_ENTRIES)));
    }
    if avds.is_none() {
        lims.push(Limitation::new("no_avd_folder", "No folder of virtual devices was found, so none are listed; that is not the same as having none."));
    }

    let devices = avds.as_ref().map(|a| a.devices.len()).unwrap_or(0);
    let usable = avds.as_ref().map(|a| a.devices.iter().filter(|d| d.usable).count()).unwrap_or(0);
    // What building and running an app each need, decided on what was seen.
    let can_build = sdk.as_ref().is_some_and(|s| !s.build_tools.is_empty() && !s.platforms.is_empty());
    let can_emulate = sdk.as_ref().is_some_and(|s| s.emulator.is_some()) && usable > 0;
    let summary = match &sdk {
        Some(s) => format!(
            "Android SDK at {}: {} build tools, {} platform(s), {} NDK(s), {} system image(s){}; {} virtual device(s).",
            s.root.display(),
            s.build_tools.len(),
            s.platforms.len(),
            s.ndks.len(),
            s.system_images.len(),
            if s.emulator.is_some() { ", emulator installed" } else { ", no emulator" },
            devices
        ),
        None => format!("No Android SDK was found. Install Android Studio or the command-line tools, or pass sdk_root. {devices} virtual device(s)."),
    };
    let mut locations = Vec::new();
    if let Some(s) = &sdk {
        locations.push(Location::new("path", s.root.display().to_string()).with_label("Android SDK"));
    }
    if let Some(a) = &avds {
        locations.extend(a.devices.iter().take(64).map(|d| Location::new("device", &d.name).with_label("virtual device")));
    }
    let examined =
        sdk.as_ref().map(|s| s.build_tools.len() + s.platforms.len() + s.ndks.len() + s.cmdline_tools.len() + s.system_images.len()).unwrap_or(0) + devices;
    let env = Environment {
        clean_room: ctx.session.clean_room(),
        engine: "builtin".into(),
        engine_version: None,
        host: craft_anything_evidence::host_triple(),
        language_id: None,
    };
    let e = EvidenceBuilder::new(SDK_CHECK_TOOL, Subject { kind: "engine".into(), sha256: None, name: Some("android-sdk".into()) }, env)
        .params(json!({"sdk_root": p.sdk_root, "avd_home": p.avd_home}))
        .authority(Authority::OurRun)
        .confidence(Confidence::Observed)
        .result(json!({
            "found": sdk.is_some(),
            "sdk": sdk,
            "virtual_devices": avds,
            "can_build": can_build,
            "can_emulate": can_emulate,
            "looked_in": places.candidates().into_iter().map(|(path, source)| json!({"path": path, "source": source})).collect::<Vec<_>>(),
        }))
        .summary(summary)
        .limitations(lims)
        .locations(locations)
        .coverage(Coverage::complete(examined as u64))
        .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use crate::tools::native_common::test_support::{call, call_err};
    use std::fs;
    use std::path::Path;

    fn dir(root: &Path, rel: &str, revision: &str) {
        let d = root.join(rel);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("source.properties"), format!("Pkg.Revision={revision}\n")).unwrap();
    }

    fn sdk() -> tempfile::TempDir {
        let t = tempfile::Builder::new().prefix("sdk").tempdir().unwrap();
        dir(t.path(), "platform-tools", "37.0.0");
        dir(t.path(), "emulator", "36.4.9");
        dir(t.path(), "build-tools/36.1.0", "36.1.0");
        dir(t.path(), "platforms/android-36", "2");
        dir(t.path(), "system-images/android-37.0/google_apis/x86_64", "1");
        t
    }

    fn avds() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        fs::write(t.path().join("Phone.ini"), "target=android-37.0\n").unwrap();
        fs::create_dir_all(t.path().join("Phone.avd")).unwrap();
        fs::write(
            t.path().join("Phone.avd").join("config.ini"),
            "abi.type=x86_64\nhw.lcd.width=1080\nhw.lcd.height=2400\nhw.lcd.density=420\nhw.ramSize=2048\n",
        )
        .unwrap();
        t
    }

    #[test]
    fn a_given_sdk_and_device_folder_are_reported_as_seen() {
        let (sdk, avds) = (sdk(), avds());
        let e = Engine::new(crate::testing::session());
        let v = call(&e, SDK_CHECK_TOOL, json!({"sdk_root": sdk.path(), "avd_home": avds.path()}));
        let r = &v["result"];
        assert_eq!(r["found"], true);
        assert_eq!(r["sdk"]["source"], "given");
        assert_eq!(r["sdk"]["build_tools"][0]["name"], "36.1.0");
        assert_eq!(r["sdk"]["platform_tools"]["revision"], "37.0.0");
        assert_eq!(r["sdk"]["system_images"][0]["abi"], "x86_64");
        assert_eq!(r["virtual_devices"]["devices"][0]["name"], "Phone");
        assert_eq!(r["virtual_devices"]["devices"][0]["density"], 420);
        assert_eq!((r["can_build"].as_bool(), r["can_emulate"].as_bool()), (Some(true), Some(true)));
        assert_eq!(v["confidence"], "observed");
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert_eq!(codes, ["folders_only"]);
    }

    #[test]
    fn nothing_configured_finds_nothing_whatever_this_computer_has() {
        let e = Engine::new(crate::testing::session());
        let v = call(&e, SDK_CHECK_TOOL, json!({}));
        let r = &v["result"];
        assert_eq!(r["found"], false);
        assert!(r["sdk"].is_null() && r["virtual_devices"].is_null());
        assert_eq!((r["can_build"].as_bool(), r["can_emulate"].as_bool()), (Some(false), Some(false)));
        assert_eq!(r["looked_in"].as_array().map(Vec::len), Some(0));
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"no_avd_folder"), "{codes:?}");
    }

    #[test]
    fn the_configured_adb_names_its_sdk() {
        let sdk = sdk();
        let adb = sdk.path().join("platform-tools").join("adb.exe");
        fs::write(&adb, b"stand-in").unwrap();
        let mut s = crate::testing::session();
        s.settings.engines.tools.adb = Some(adb);
        let v = call(&Engine::new(s), SDK_CHECK_TOOL, json!({}));
        assert_eq!(v["result"]["sdk"]["source"], "adb");
        assert_eq!(v["result"]["can_emulate"], false, "an emulator with no virtual device can't emulate");
    }

    #[test]
    fn a_given_folder_that_is_no_sdk_says_so_and_is_not_reported_as_one() {
        let empty = tempfile::tempdir().unwrap();
        let e = Engine::new(crate::testing::session());
        let v = call(&e, SDK_CHECK_TOOL, json!({"sdk_root": empty.path()}));
        assert_eq!(v["result"]["found"], false);
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"given_folder_not_an_sdk"), "{codes:?}");
    }

    #[test]
    fn it_runs_in_clean_room_and_refuses_unknown_fields() {
        let mut s = crate::testing::session();
        s.settings.deep_mode = false;
        let e = Engine::new(s);
        let v = call(&e, SDK_CHECK_TOOL, json!({}));
        assert_eq!(v["environment"]["clean_room"], true);
        call_err(&e, SDK_CHECK_TOOL, json!({"serial": "emulator-5554"}));
    }
}
