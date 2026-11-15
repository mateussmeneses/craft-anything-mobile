//! An iOS app archive (`.ipa`) read as one app with built-in readers: Info.plist, the main
//! executable (architectures, encryption, linked libraries, entitlements from its code
//! signature), the embedded provisioning profile, nested frameworks, extensions and watch apps,
//! and what the app is made with. Nothing is extracted.

use super::native_common::spec;
use super::static_input::{self, builder};
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_analysis::ios_app;
use craft_anything_evidence::{ErrorCode, Limitation, Location, ToolError};
use craft_anything_formats::{ipa, zip};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The tool's id.
pub const IPA_TOOL: &str = "apple_ipa_show";

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        IPA_TOOL,
        "iOS app archive",
        "Read an .ipa as one app with built-in readers, extracting nothing: the Payload/<Name>.app bundle and what else the archive holds (SwiftSupport, Symbols, iTunesMetadata.plist); Info.plist facts (bundle id, version and build, display name, minimum OS, device families, required capabilities, URL schemes, usage-description keys, App Transport Security exceptions, background modes, scenes and storyboards); the main executable's architectures, whether it is encrypted (LC_ENCRYPTION_INFO cryptid), linked system and embedded libraries and the entitlements in its code signature; the embedded provisioning profile (name, team, expiry, device list, kind, entitlements; its signature is not verified); embedded frameworks, app extensions and watch apps (paged with offset/limit); and what it is made with (Flutter, React Native, Unity, .NET, Cordova, Capacitor, SwiftUI, UIKit, Swift or Objective-C) from a typed signal table.",
        "See what an iPhone app is called, what it asks permission for, what it is allowed to do, who it was signed for and what it was made with.",
        Toolset::Apple,
        Effects::EVIDENCE | Effects::CARRIES_TARGET_CONTENT,
        Requires::Nothing,
        schema_of::<IpaParams>,
        apple_ipa_show,
    )]
}

fn d_limit() -> usize {
    100
}

/// Most rows one page may ask for.
const MAX_LIMIT: usize = 2_000;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IpaParams {
    /// Absolute path of the .ipa; the active target when omitted.
    #[serde(default)]
    pub path: Option<String>,
    /// First row of each long list (frameworks, extensions, watch apps, linked libraries).
    #[serde(default)]
    pub offset: usize,
    /// Rows per long list (1–2000).
    #[serde(default = "d_limit")]
    pub limit: usize,
}

fn apple_ipa_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: IpaParams = parse_params(params)?;
    if p.limit == 0 || p.limit > MAX_LIMIT {
        return Err(ToolError::invalid(format!("limit must be 1–{MAX_LIMIT}, not {}", p.limit)));
    }
    let input = static_input::load(ctx, p.path.as_deref(), static_input::DEFAULT_MAX_BYTES)
        .map_err(|e| e.with_remediation("Pass the absolute path of an .ipa of at most 256 MiB; larger archives aren't read yet."))?;
    let read = ipa::parse(&input.bytes, &ipa::Limits::default()).map_err(|e| match e {
        zip::ZipError::NotZip => ToolError::new(ErrorCode::NotSupported, format!("{} is not a ZIP archive, so it can't be an .ipa", input.name))
            .with_remediation("Pass an .ipa file (a ZIP holding Payload/<Name>.app)."),
        other => ToolError::new(ErrorCode::NotSupported, format!("Couldn't read {} as an .ipa: {other}", input.name))
            .with_remediation("Check the file is complete (a cut-off download can't be read)."),
    })?;
    if read.app.is_none() {
        return Err(ToolError::unsupported(format!("{} has no Payload/<Name>.app folder, so it isn't laid out as an iOS app", input.name))
            .with_remediation("Pass an .ipa; for an Android app use android_package_show, for any other ZIP core_archive_show."));
    }
    let r = ios_app::report(&read, p.offset, p.limit);
    let mut locations = Vec::new();
    if let Some(app) = &read.app {
        locations.push(Location::new("path", app.clone()).with_label("app bundle"));
    }
    if let Some(x) = &read.executable {
        locations.push(Location::new("path", x.path.clone()).with_label("main executable"));
    }
    let mut lims: Vec<Limitation> = r.limitations;
    lims.push(Limitation::new(
        "executable_inside_archive",
        "The apple_* tools take a Mach-O file on disk; this tool reads the executable inside the archive and doesn't extract it for them.",
    ));
    let b = builder(ctx, IPA_TOOL, input.subject.clone(), json!({"path": p.path, "offset": p.offset, "limit": p.limit}))
        .result(r.result)
        .summary(r.summary)
        .confidence(r.confidence)
        .limitations(lims)
        .coverage(r.coverage)
        .locations(locations)
        .target_content(true);
    Ok(ToolOutput::evidence(b.build()?))
}

#[cfg(test)]
mod tests {
    use super::super::native_common::test_support::{call, call_err, engine_with};
    use craft_anything_testkit::ios::{self, IpaSpec};
    use serde_json::json;

    #[test]
    fn an_ipa_is_read_as_one_app() {
        let e = engine_with("Notes.ipa", &ios::ipa(&IpaSpec { binary_info: true, ..IpaSpec::default() }));
        let r = call(&e, "apple_ipa_show", json!({}));
        let res = &r["result"];
        assert_eq!(res["info"]["bundle_id"], "com.example.notes");
        assert_eq!(res["info"]["encoding"], "binary");
        assert_eq!(res["executable"]["encrypted"], false);
        assert_eq!(res["executable"]["signature"]["entitlement_facts"]["application_identifier"], "ABCDE12345.com.example.notes");
        assert_eq!(res["profile"]["kind"], "app_store");
        assert_eq!(res["profile"]["signature_verified"], false);
        assert_eq!(res["made_with"]["primary"], "flutter");
        assert_eq!(res["extensions"]["rows"][0]["extension_point"], "com.apple.share-services");
        assert!(r["summary"].as_str().unwrap().contains("Notes (com.example.notes)"));
        assert_eq!(r["confidence"], "observed");
        assert!(r["limitations"].as_array().unwrap().iter().any(|l| l["code"] == "synthetic_fixtures_only"));
    }

    #[test]
    fn encrypted_paged_and_refusals() {
        let path = craft_anything_testkit::temp_file("Big.ipa", &ios::ipa(&IpaSpec { cryptid: 1, extra_frameworks: 5, ..IpaSpec::default() }));
        let e = engine_with("Notes.ipa", &ios::ipa(&IpaSpec::default()));
        let r = call(&e, "apple_ipa_show", json!({"path": path.to_string_lossy(), "offset": 2, "limit": 3}));
        assert!(r["summary"].as_str().unwrap().contains("ENCRYPTED"));
        assert!(r["limitations"].as_array().unwrap().iter().any(|l| l["code"] == "executable_encrypted"));
        let fw = &r["result"]["frameworks"];
        assert_eq!((fw["total"].as_u64(), fw["rows"].as_array().map(Vec::len), fw["next_offset"].as_u64()), (Some(8), Some(3), Some(5)));
        assert_eq!(r["coverage"]["status"], "partial");
        let not_app = craft_anything_testkit::temp_file("plain.zip", &ios::zip_of(&[("a.txt".into(), b"a".to_vec())]));
        assert!(call_err(&e, "apple_ipa_show", json!({"path": not_app.to_string_lossy()})).message.contains("Payload"));
        let junk = craft_anything_testkit::temp_file("junk.ipa", b"not an archive");
        assert!(call_err(&e, "apple_ipa_show", json!({"path": junk.to_string_lossy()})).message.contains("ZIP"));
        let good = ios::ipa(&IpaSpec::default());
        let cut = craft_anything_testkit::temp_file("cut.ipa", good.get(..good.len() / 3).unwrap());
        let _ = e.call("apple_ipa_show", json!({"path": cut.to_string_lossy()}), crate::CallOptions::new(crate::tool::Caller::Mcp));
        assert!(call_err(&e, "apple_ipa_show", json!({"limit": 0})).message.contains("limit"));
    }

    #[test]
    fn it_is_deep_only() {
        assert!(!crate::clean_room::allows("apple_ipa_show"));
    }
}
