//! An iOS app archive (`.ipa`) as one app: the facts [`craft_anything_formats::ipa`] reads, turned
//! into a result with paged lists, honest limitations and coverage, plus which frameworks the app
//! is made with, from a typed signal table ([`SIGNALS`]).
//!
//! The Android table ([`crate::android_frameworks`]) looks at DEX class names and resource types,
//! which an iOS app doesn't have, so iOS keeps its own table of the same shape: file paths in the
//! archive and the install names of the libraries the main executable links.

use crate::android_frameworks::{Evidence, Finding, Frameworks, Role, Strength};
use craft_anything_evidence::{Confidence, Coverage, CoverageStatus, Limitation};
use craft_anything_formats::ipa::{Capped, Ipa};
use serde::Serialize;
use serde_json::{Value, json};

/// Where an iOS signal is looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// An archive path containing this (lower case).
    File(&'static str),
    /// A library install name the main executable links, containing this (lower case).
    Library(&'static str),
}

/// One row of the table.
#[derive(Debug, Clone, Copy)]
pub struct Signal {
    pub framework: &'static str,
    pub place: Place,
    pub strength: Strength,
}

/// The frameworks known on iOS, with their display names and roles.
pub const FRAMEWORKS: &[(&str, &str, Role)] = &[
    ("flutter", "Flutter", Role::CrossPlatform),
    ("react-native", "React Native", Role::CrossPlatform),
    ("unity", "Unity", Role::CrossPlatform),
    ("dotnet", ".NET for iOS (MAUI or Xamarin)", Role::CrossPlatform),
    ("cordova", "Apache Cordova", Role::Web),
    ("capacitor", "Capacitor", Role::Web),
    ("webview", "Web pages in a WebKit view", Role::Web),
    ("swiftui", "SwiftUI", Role::NativeUi),
    ("uikit", "UIKit", Role::NativeUi),
    ("swift", "Swift", Role::Language),
    ("objective-c", "Objective-C runtime", Role::Language),
];

const fn s(framework: &'static str, place: Place, strength: Strength) -> Signal {
    Signal { framework, place, strength }
}
use Place::{File, Library};
use Strength::{Medium, Strong, Weak};

/// What says which framework.
pub const SIGNALS: &[Signal] = &[
    s("flutter", File("/frameworks/flutter.framework/"), Strong),
    s("flutter", File("/flutter_assets/"), Strong),
    s("flutter", File("/frameworks/app.framework/"), Medium),
    s("flutter", Library("@rpath/flutter.framework/"), Strong),
    s("react-native", File("/main.jsbundle"), Strong),
    s("react-native", File("/hermes.framework/"), Medium),
    s("react-native", Library("hermes.framework/"), Medium),
    s("unity", File("/frameworks/unityframework.framework/"), Strong),
    s("unity", Library("unityframework.framework/"), Strong),
    s("unity", File("/data/managed/"), Medium),
    s("dotnet", File("/microsoft.ios.dll"), Strong),
    s("dotnet", File("/xamarin.ios.dll"), Strong),
    s("dotnet", File("/microsoft.maui.dll"), Strong),
    s("dotnet", File("libmonosgen"), Medium),
    s("cordova", File("/www/cordova.js"), Strong),
    s("capacitor", File("/capacitor.config.json"), Strong),
    s("capacitor", Library("capacitor.framework/"), Strong),
    s("webview", Library("/webkit.framework/webkit"), Weak),
    s("swiftui", Library("/swiftui.framework/swiftui"), Strong),
    s("uikit", Library("/uikit.framework/uikit"), Medium),
    s("uikit", File(".storyboardc/"), Medium),
    s("swift", Library("libswiftcore.dylib"), Strong),
    s("swift", File("swiftsupport/"), Strong),
    s("swift", File("/frameworks/libswift"), Medium),
    s("objective-c", Library("/usr/lib/libobjc.a.dylib"), Weak),
];

/// Most examples kept per signal.
const MAX_EXAMPLES: usize = 3;

/// What an iOS app is made with, and the language its own code most likely is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MadeWith {
    #[serde(flatten)]
    pub frameworks: Frameworks,
    /// `swift` when the Swift runtime is linked or shipped, else `objective-c` when only the
    /// Objective-C runtime is; a Swift app links both.
    pub language: Option<&'static str>,
}

/// Read the frameworks from archive paths and linked library install names.
pub fn detect<'a>(files: impl Iterator<Item = &'a str> + Clone, libraries: impl Iterator<Item = &'a str> + Clone) -> MadeWith {
    let mut found: Vec<Finding> = Vec::new();
    for &(id, name, role) in FRAMEWORKS {
        let mut evidence = Vec::new();
        for sig in SIGNALS.iter().filter(|s| s.framework == id) {
            let (kind, pattern, hits): (&str, &str, Vec<&str>) = match sig.place {
                File(p) => ("file", p, files.clone().filter(|f| f.to_ascii_lowercase().contains(p)).collect()),
                Library(p) => ("library", p, libraries.clone().filter(|l| l.to_ascii_lowercase().contains(p)).collect()),
            };
            if !hits.is_empty() {
                evidence.push(Evidence {
                    kind,
                    pattern,
                    strength: sig.strength,
                    matches: hits.len(),
                    examples: hits.iter().take(MAX_EXAMPLES).map(|h| h.to_string()).collect(),
                });
            }
        }
        if let Some(strength) = evidence.iter().map(|e| e.strength).max() {
            found.push(Finding { framework: id, name, role, strength, evidence });
        }
    }
    let strong = |role: Role| found.iter().find(|f| f.role == role && f.strength == Strong).map(|f| f.framework);
    let primary = strong(Role::CrossPlatform)
        .or_else(|| found.iter().find(|f| f.role == Role::Web && f.framework != "webview" && f.strength == Strong).map(|f| f.framework))
        .or_else(|| strong(Role::NativeUi))
        .or_else(|| found.iter().find(|f| f.framework == "uikit").map(|f| f.framework));
    let has = |id: &str| found.iter().any(|f| f.framework == id);
    let language = if has("swift") {
        Some("swift")
    } else if has("objective-c") {
        Some("objective-c")
    } else {
        None
    };
    MadeWith { frameworks: Frameworks { found, primary }, language }
}

/// The result of reading an `.ipa`, ready for an evidence record.
#[derive(Debug, Clone)]
pub struct Report {
    pub result: Value,
    pub summary: String,
    pub limitations: Vec<Limitation>,
    pub coverage: Coverage,
    pub confidence: Confidence,
}

/// One page of a capped list: the complete total, the rows from `offset`, whether the reader
/// kept fewer rows than the total, and where the next page starts.
fn page<T: Serialize>(list: &Capped<T>, offset: usize, limit: usize) -> (Value, bool) {
    let rows: Vec<Value> = list.items.iter().skip(offset).take(limit).map(|x| serde_json::to_value(x).unwrap_or(Value::Null)).collect();
    let end = offset.saturating_add(rows.len());
    let more = end < list.items.len();
    (json!({"total": list.total, "offset": offset, "rows": rows, "rows_capped": list.rows_capped, "next_offset": if more { Some(end) } else { None }}), more)
}

/// Build the report. `offset` and `limit` page every long list (frameworks, extensions, watch
/// apps, linked libraries) by the same window.
pub fn report(ipa: &Ipa, offset: usize, limit: usize) -> Report {
    let limit = limit.max(1);
    let mut lims = vec![Limitation::new(
        "synthetic_fixtures_only",
        "This reader has been tested on synthetic archives only; no real .ipa has been read with it yet, so layouts real build tools produce may hold surprises.",
    )];
    let mut partial = ipa.listing_truncated;
    let mut more_pages = false;

    if ipa.listing_truncated {
        lims.push(Limitation::new(
            "listing_truncated",
            format!(
                "The archive's listing stopped after {} of {} declared entries (a cap or damage); later files are not looked at.",
                ipa.entries, ipa.declared_entries
            ),
        ));
    }
    if ipa.apps.total > 1 {
        partial = true;
        lims.push(Limitation::new(
            "several_apps",
            format!("Payload holds {} app folders; only {} was read.", ipa.apps.total, ipa.app.as_deref().unwrap_or("the first")),
        ));
    }
    if let Some(e) = &ipa.info_error {
        partial = true;
        lims.push(Limitation::new("info_plist_unreadable", format!("The app's Info.plist wasn't read: {e}.")));
    }
    if let Some(e) = &ipa.executable_error {
        partial = true;
        lims.push(Limitation::new("executable_unread", format!("The main executable wasn't read: {e}.")));
    }
    let x = ipa.executable.as_ref();
    if x.is_some_and(|x| x.encrypted) {
        lims.push(Limitation::new(
            "executable_encrypted",
            "The main executable is encrypted (LC_ENCRYPTION_INFO cryptid is not 0, as in an App Store download): its code can't be read or disassembled from this file. Its headers, linked libraries and entitlements sit outside the encrypted range and are read; reading the code needs a decrypted copy.",
        ));
    }
    if let Some(sig) = x.and_then(|x| x.signature.as_ref()) {
        if sig.entitlements.is_none() && sig.der_entitlements_bytes.is_some() {
            lims.push(Limitation::new("der_entitlements_only", "The signature carries entitlements only in DER form, which isn't decoded here."));
        }
    } else if let Some(e) = x.and_then(|x| x.signature_error.as_ref()) {
        lims.push(Limitation::new("no_entitlements", format!("No entitlements were read from the executable: {e}.")));
    }
    match (&ipa.profile, &ipa.profile_error) {
        (_, Some(e)) => {
            partial = true;
            lims.push(Limitation::new("profile_unreadable", format!("The provisioning profile wasn't read: {e}.")));
        }
        (Some(p), None) if p.read_via == "located_plist" => lims.push(Limitation::new(
            "profile_envelope_unreadable",
            "The provisioning profile's CMS envelope couldn't be read as SignedData; its property list was found by its XML markers instead.",
        )),
        (None, None) if ipa.app.is_some() => {
            lims.push(Limitation::new("no_profile", "The app has no embedded.mobileprovision; App Store downloads and simulator builds don't carry one."))
        }
        _ => {}
    }
    lims.push(Limitation::new(
        "signatures_not_verified",
        "No signature is verified: not the executable's code signature, not the provisioning profile's CMS signature. A profile's kind (development, ad hoc, enterprise, App Store) is read from its own keys.",
    ));
    if ipa.extras.itunes_metadata {
        lims.push(Limitation::new("itunes_metadata_not_read", "iTunesMetadata.plist is present but not read: it can hold the buyer's account details."));
    }
    if ipa.budget_exhausted {
        partial = true;
        lims.push(Limitation::new("byte_budget_spent", "The read's decompression budget ran out, so some files inside the app weren't read."));
    }
    if ipa.bundles_not_read > 0 {
        partial = true;
        lims.push(Limitation::new(
            "bundles_not_read",
            format!("{} nested bundles are listed without reading their Info.plist (bundle cap reached).", ipa.bundles_not_read),
        ));
    }
    let mut capped_lists = Vec::new();
    for (name, capped) in [
        ("frameworks", ipa.frameworks.rows_capped),
        ("extensions", ipa.extensions.rows_capped),
        ("watch_apps", ipa.watch_apps.rows_capped),
        ("libraries", x.is_some_and(|x| x.libraries.rows_capped)),
        ("apps", ipa.apps.rows_capped),
    ] {
        if capped {
            capped_lists.push(name);
        }
    }
    if let Some(i) = &ipa.info {
        for (name, capped) in [
            ("required_capabilities", i.required_capabilities.rows_capped),
            ("url_schemes", i.url_schemes.rows_capped),
            ("queried_schemes", i.queried_schemes.rows_capped),
            ("usage_descriptions", i.usage_descriptions.rows_capped),
            ("background_modes", i.background_modes.rows_capped),
            ("scenes", i.scenes.rows_capped),
        ] {
            if capped {
                capped_lists.push(name);
            }
        }
    }
    if !capped_lists.is_empty() {
        partial = true;
        lims.push(Limitation::new(
            "rows_capped",
            format!("Some lists kept fewer rows than they have; each shows its complete total: {}.", capped_lists.join(", ")),
        ));
    }
    lims.push(Limitation::new(
        "signals_from_names",
        "What the app is made with is read from file paths and linked library names only (inferred): renamed or statically linked frameworks leave fewer signs.",
    ));
    lims.extend(ipa.warnings.iter().take(50).map(|w| Limitation::new("warning", w.clone())));

    let (frameworks, m1) = page(&ipa.frameworks, offset, limit);
    let (extensions, m2) = page(&ipa.extensions, offset, limit);
    let (watch_apps, m3) = page(&ipa.watch_apps, offset, limit);
    let libraries = x.map(|x| page(&x.libraries, offset, limit));
    more_pages |= m1 || m2 || m3 || libraries.as_ref().is_some_and(|l| l.1);

    let libs: Vec<&str> = x.map(|x| x.libraries.items.iter().map(|l| l.name.as_str()).collect()).unwrap_or_default();
    let made = detect(ipa.names.iter().map(String::as_str), libs.iter().copied());

    let executable = x.map(|x| {
        json!({
            "path": x.path, "bytes": x.bytes, "universal": x.universal, "slices": x.slices, "read_slice": x.read_slice,
            "encrypted": x.encrypted, "libraries": libraries.as_ref().map(|l| l.0.clone()),
            "system_libraries": x.libraries.items.iter().filter(|l| l.origin == "system").count(),
            "embedded_libraries": x.libraries.items.iter().filter(|l| l.origin == "embedded").count(),
            "signature": x.signature, "signature_error": x.signature_error,
        })
    });
    let result = json!({
        "archive": {
            "entries": ipa.entries, "declared_entries": ipa.declared_entries, "listing_truncated": ipa.listing_truncated,
            "apps": ipa.apps, "app": ipa.app, "app_files": ipa.app_files,
        },
        "extras": ipa.extras,
        "info": ipa.info,
        "executable": executable,
        "profile": ipa.profile,
        "frameworks": frameworks,
        "extensions": extensions,
        "watch_apps": watch_apps,
        "made_with": {"confidence": "inferred", "found": made.frameworks.found, "primary": made.frameworks.primary, "language": made.language},
        "errors": {"info": ipa.info_error, "executable": ipa.executable_error, "profile": ipa.profile_error},
        "bundles_not_read": ipa.bundles_not_read,
        "budget_exhausted": ipa.budget_exhausted,
        "warnings": ipa.warnings,
    });

    let name_of = |id: &str| made.frameworks.found.iter().find(|f| f.framework == id).map(|f| f.name).unwrap_or("");
    let summary = match (&ipa.app, &ipa.info) {
        (None, _) => "No Payload/<Name>.app folder: this archive isn't laid out as an iOS app.".to_string(),
        (Some(app), info) => {
            let i = info.as_ref();
            let title = i.and_then(|i| i.display_name.clone().or_else(|| i.name.clone())).unwrap_or_else(|| app.trim_start_matches("Payload/").to_string());
            let mut parts = vec![format!(
                "{title} ({}) {} ({})",
                i.and_then(|i| i.bundle_id.as_deref()).unwrap_or("unknown bundle id"),
                i.and_then(|i| i.version.as_deref()).unwrap_or("?"),
                i.and_then(|i| i.build.as_deref()).unwrap_or("?"),
            )];
            if let Some(os) = i.and_then(|i| i.minimum_os.as_deref()) {
                parts.push(format!("needs iOS {os} or later"));
            }
            match x {
                Some(x) => parts.push(format!(
                    "executable for {} ({}), {} linked libraries",
                    x.slices.iter().map(|s| s.arch.as_str()).collect::<Vec<_>>().join(", "),
                    if x.encrypted { "ENCRYPTED: its code can't be read" } else { "not encrypted" },
                    x.libraries.total
                )),
                None => parts.push("executable not read".into()),
            }
            parts.push(format!("{} frameworks, {} extensions, {} watch apps", ipa.frameworks.total, ipa.extensions.total, ipa.watch_apps.total));
            match &ipa.profile {
                Some(p) => parts.push(format!(
                    "{} profile{}{} (signature not verified)",
                    p.kind.replace('_', " "),
                    p.team_name.as_deref().map(|t| format!(" from {t}")).unwrap_or_default(),
                    p.expiration_date.as_deref().map(|d| format!(", expires {d}")).unwrap_or_default()
                )),
                None => parts.push("no provisioning profile".into()),
            }
            if let Some(p) = made.frameworks.primary {
                parts.push(format!("made with {}", name_of(p)));
            }
            if let Some(l) = made.language {
                parts.push(format!("code in {}", if l == "swift" { "Swift" } else { "Objective-C" }));
            }
            format!("{}.", parts.join("; "))
        }
    };

    let coverage = Coverage {
        status: if partial || more_pages || ipa.app.is_none() { CoverageStatus::Partial } else { CoverageStatus::Complete },
        examined: Some(ipa.entries as u64),
        total: Some(ipa.declared_entries.max(ipa.entries as u64)),
        next_offset: if more_pages { Some(offset.saturating_add(limit) as u64) } else { None },
    };
    Report { result, summary, limitations: lims, coverage, confidence: Confidence::Observed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_formats::ipa::{self, Limits};
    use craft_anything_testkit::ios::{self, IpaSpec};

    #[test]
    fn every_signal_names_a_known_framework_in_lower_case() {
        for s in SIGNALS {
            assert!(FRAMEWORKS.iter().any(|f| f.0 == s.framework), "{}", s.framework);
            let (File(p) | Library(p)) = s.place;
            assert_eq!(p, p.to_ascii_lowercase(), "patterns are matched on lower case");
        }
    }

    #[test]
    fn a_flutter_app_in_swift() {
        let read = ipa::parse(&ios::ipa(&IpaSpec::default()), &Limits::default()).unwrap();
        let r = report(&read, 0, 100);
        assert_eq!(r.result["made_with"]["primary"], "flutter");
        assert_eq!(r.result["made_with"]["language"], "swift");
        assert!(r.summary.contains("Notes (com.example.notes) 2.1 (42)"), "{}", r.summary);
        assert!(r.summary.contains("made with Flutter"), "{}", r.summary);
        assert!(r.summary.contains("app store profile from Example Team"), "{}", r.summary);
        assert_eq!(r.coverage.status, CoverageStatus::Complete);
        let codes: Vec<&str> = r.limitations.iter().map(|l| l.code.as_str()).collect();
        assert!(codes.contains(&"synthetic_fixtures_only") && codes.contains(&"signatures_not_verified") && codes.contains(&"itunes_metadata_not_read"));
        assert!(!codes.contains(&"executable_encrypted"));
        assert_eq!(r.result["executable"]["libraries"]["total"], 6);
        assert_eq!(r.result["executable"]["embedded_libraries"], 2);
        assert_eq!(r.result["executable"]["signature"]["entitlement_facts"]["push_environment"], "production");
    }

    #[test]
    fn encrypted_and_paged_and_capped() {
        let read =
            ipa::parse(&ios::ipa(&IpaSpec { cryptid: 1, extra_frameworks: 12, ..IpaSpec::default() }), &Limits { max_rows: 10, ..Limits::default() }).unwrap();
        let r = report(&read, 0, 4);
        assert!(r.summary.contains("ENCRYPTED"), "{}", r.summary);
        assert!(r.limitations.iter().any(|l| l.code == "executable_encrypted"));
        let fw = &r.result["frameworks"];
        assert_eq!((fw["total"].as_u64(), fw["rows"].as_array().map(Vec::len)), (Some(15), Some(4)));
        assert_eq!((fw["rows_capped"].as_bool(), fw["next_offset"].as_u64()), (Some(true), Some(4)));
        assert_eq!(r.coverage.status, CoverageStatus::Partial);
        assert_eq!(r.coverage.next_offset, Some(4));
        assert!(r.limitations.iter().any(|l| l.code == "rows_capped" && l.message.contains("frameworks")));
        let last = report(&read, 8, 4);
        assert_eq!(last.result["frameworks"]["rows"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn web_shells_native_and_nothing() {
        let rn =
            detect(["Payload/A.app/main.jsbundle"].into_iter(), ["/usr/lib/libobjc.A.dylib", "/System/Library/Frameworks/UIKit.framework/UIKit"].into_iter());
        assert_eq!((rn.frameworks.primary, rn.language), (Some("react-native"), Some("objective-c")));
        let cap = detect(["Payload/A.app/capacitor.config.json"].into_iter(), ["/System/Library/Frameworks/WebKit.framework/WebKit"].into_iter());
        assert_eq!(cap.frameworks.primary, Some("capacitor"));
        let sw = detect(std::iter::empty(), ["/System/Library/Frameworks/SwiftUI.framework/SwiftUI", "/usr/lib/swift/libswiftCore.dylib"].into_iter());
        assert_eq!((sw.frameworks.primary, sw.language), (Some("swiftui"), Some("swift")));
        let none = detect(std::iter::empty(), std::iter::empty());
        assert_eq!((none.frameworks.primary, none.language, none.frameworks.found.len()), (None, None, 0));
    }

    #[test]
    fn no_app_is_partial_and_says_so() {
        let zip = ios::zip_of(&[("readme.txt".into(), b"hi".to_vec())]);
        let read = ipa::parse(&zip, &Limits::default()).unwrap();
        let r = report(&read, 0, 10);
        assert!(r.summary.starts_with("No Payload"));
        assert_eq!(r.coverage.status, CoverageStatus::Partial);
    }
}
