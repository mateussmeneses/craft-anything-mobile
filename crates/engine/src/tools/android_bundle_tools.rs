//! One Android app delivered as several files (`android_bundle_show`): a split APK set (`.apks`,
//! `.xapk`, or any ZIP of APKs) or an Android App Bundle (`.aab`), read as one app. Each APK's or
//! module's manifest is read (binary XML in an APK, protocol buffers in a bundle) and the parts
//! are named: the base, feature splits and configuration splits (ABI, screen density, language).

use super::native_common::{page, spec};
use super::static_input::{self, builder};
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_evidence::{Authority, Confidence, Coverage, CoverageStatus, ErrorCode, Limitation, Location, ToolError};
use craft_anything_formats::{aapt_proto, axml, zip};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const BUNDLE_TOOL: &str = "android_bundle_show";

/// Biggest file read.
const MAX_FILE: u64 = 4 << 30;
/// Biggest APK inside a set, decompressed.
const MAX_INNER: u64 = 1 << 30;
/// Biggest manifest read.
const MAX_MANIFEST: u64 = 16 << 20;
/// Most APKs or modules read from one file.
const MAX_PARTS: usize = 256;

pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec(
            BUNDLE_TOOL,
            "Android app in several files",
            "Read an app shipped as several pieces, as one app: a split APK set (.apks, .xapk or any ZIP holding APKs) or an Android App Bundle (.aab). For each APK or bundle module: whether it is the base, a feature split or a configuration split (and what it configures: ABI, screen density, language), its manifest's package, version, SDK levels and components (a bundle's manifests are protocol buffers, read with a built-in reader), its DEX files and native ABIs. Says when the parts disagree on package or version. A single APK is read as a set of one; use android_package_show for its signing and full manifest.",
            "See every piece of an Android app that comes in several files, and which piece is which.",
            Toolset::Android,
            Effects::EVIDENCE,
            Requires::Nothing,
            schema_of::<BundleParams>,
            android_bundle_show,
        ),
        spec(
            ODEX_TOOL,
            "Compiled Android code (ODEX/VDEX)",
            "Say what a file Android's runtime made from an app's code is: a .vdex (the format version; for 027, Android 12 and later, its sections, how many DEX files it covers and whether their bytecode is inside it; for 019 and 021 its header and DEX section sizes), an .odex/.oat (an ELF with machine code compiled from the DEX: its OAT version, and for the OAT versions whose layout is known the instruction set, the DEX files it was compiled from with their checksums, paged with offset/limit, and every setting dex2oat recorded, such as the compiler filter and class path) or an .art image (its version only). Says what can't be read: compiled machine code is not turned back into Java or Kotlin, and for an OAT version outside the known layouts only its settings are found, by key.",
            "See what an .odex, .vdex or .art file from an Android device holds, and what can't be read from it.",
            Toolset::Android,
            Effects::EVIDENCE,
            Requires::Nothing,
            schema_of::<OdexParams>,
            android_odex_show,
        ),
    ]
}

pub const ODEX_TOOL: &str = "android_odex_show";

fn android_odex_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    use craft_anything_formats::art_files::{self, ArtFile};
    let p: OdexParams = parse_params(params)?;
    let input = static_input::load(ctx, p.path.as_deref(), MAX_FILE)?;
    let file = art_files::parse_any(&input.bytes).ok_or_else(|| {
        ToolError::unsupported(format!("{} is neither a VDEX file, an ELF with an OAT header nor an ART image", input.name))
            .with_remediation("Pass an .odex, .oat, .vdex or .art file (from /data/app/<app>/oat/<abi>/ or /system/framework/ on a device).")
    })?;
    let mut lims = vec![Limitation::new(
        "compiled_code_not_decompiled",
        "Machine code compiled from the DEX is not turned back into Java or Kotlin; read the app's own DEX (in its APK) for that.",
    )];
    let mut coverage = Coverage::complete(1);
    let mut result = json!(file);
    let summary = match &file {
        ArtFile::Vdex { version, dex_files, embedded_dex, warnings, .. } => {
            lims.extend(warnings.iter().take(20).map(|w| Limitation::new("vdex_warning", w.clone())));
            if *embedded_dex == 0 {
                lims.push(Limitation::new(
                    "no_bytecode_inside",
                    "No DEX bytecode is inside this VDEX (since Android 12 it stays in the APK), so there is no code to read from it.",
                ));
            }
            if version != "027" {
                lims.push(Limitation::new(
                    "vdex_layout_from_documentation",
                    "VDEX layouts before 027 were written from the runtime's published sources and haven't been checked on a real file; verification data and quickening data are not read.",
                ));
            }
            format!(
                "{}: VDEX version {version}, {} DEX file(s) covered, {embedded_dex} with their bytecode inside.",
                input.name,
                dex_files.map_or("an unknown number of".to_string(), |n| n.to_string())
            )
        }
        ArtFile::Oat { version, header, settings, dex_list, embedded_dex, warnings, .. } => {
            lims.extend(warnings.iter().take(20).map(|w| Limitation::new("oat_warning", w.clone())));
            let filter = settings.iter().find(|(k, _)| k == "compiler-filter").map_or("not recorded", |(_, v)| v.as_str());
            match header {
                Some(h) => {
                    if !h.layout_checked.contains("real file") {
                        lims.push(Limitation::new(
                            "oat_layout_from_documentation",
                            format!("OAT {version}'s header layout was written from the runtime's published sources ({}) and hasn't been checked on a real file.", h.layout_checked),
                        ));
                    }
                    if dex_list.is_empty() && h.dex_file_count > 0 && warnings.is_empty() {
                        lims.push(Limitation::new(
                            "oat_dex_list_not_read",
                            format!("OAT {version}'s DEX records aren't laid out here, so the {} DEX file(s) it lists are counted but not named.", h.dex_file_count),
                        ));
                    }
                    lims.push(Limitation::new(
                        "oat_tables_not_followed",
                        "Per-class tables, type lookup tables and .bss mappings are given only as the offsets each DEX record holds; they aren't read.",
                    ));
                }
                None => lims.push(Limitation::new(
                    "oat_header_not_read",
                    format!(
                        "OAT version {version} isn't one whose header layout is known here (known: {}); its instruction set and DEX file list are not read, only settings found by their keys.",
                        art_files::known_oat_versions().join(", ")
                    ),
                )),
            }
            let (page, cov) = page(dex_list, p.offset, p.limit.unwrap_or(DEX_PAGE));
            if !dex_list.is_empty() {
                coverage = cov;
            }
            if let Some(obj) = result.as_object_mut() {
                obj.insert("dex_list".into(), Value::Array(page));
                obj.insert("dex_list_total".into(), json!(dex_list.len()));
            }
            let isa = header.as_ref().map_or_else(String::new, |h| format!(" for {}", h.instruction_set));
            let listed = match (header, dex_list.first()) {
                (Some(h), Some(first)) => format!(" {} DEX file(s) listed, the first {};", h.dex_file_count, first.location),
                (Some(h), None) => format!(" {} DEX file(s) counted;", h.dex_file_count),
                _ => String::new(),
            };
            format!("{}: OAT version {version} (compiled code){isa}, compiler filter {filter};{listed} {embedded_dex} DEX header(s) found in it.", input.name)
        }
        ArtFile::Image { version } => {
            lims.push(Limitation::new(
                "art_image_not_read",
                "Only the ART image's magic and version are read; the objects, classes and methods it lays out for the runtime are not.",
            ));
            format!("{}: ART image version {version} (objects laid out for the runtime to map).", input.name)
        }
    };
    let e = builder(ctx, ODEX_TOOL, input.subject.clone(), json!({"path": p.path, "offset": p.offset, "limit": p.limit}))
        .authority(Authority::OriginalFiles)
        .location(Location::new("path", input.path.display().to_string()))
        .result(result)
        .summary(summary)
        .confidence(Confidence::Observed)
        .limitations(lims)
        .coverage(coverage)
        .build()?;
    Ok(ToolOutput::evidence(e))
}

/// DEX records shown per page by default.
const DEX_PAGE: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OdexParams {
    /// Absolute path of the .odex, .oat, .vdex or .art file; the active target when left out.
    #[serde(default)]
    path: Option<String>,
    /// First DEX record of an OAT file's list to show.
    #[serde(default)]
    offset: usize,
    /// How many DEX records to show (100 by default).
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BundleParams {
    /// Absolute path of the .apks, .xapk, .aab or .apk; the active target when left out.
    #[serde(default)]
    path: Option<String>,
}

/// What a manifest says about the part it belongs to.
fn part_facts(doc: &axml::Document) -> Value {
    let m = axml::manifest(doc);
    let root = doc.nodes.iter().find_map(|n| match n {
        axml::Node::Start { name, attributes, .. } if name == "manifest" => Some(attributes.as_slice()),
        _ => None,
    });
    let get = |name: &str| root.and_then(|a| a.iter().find(|x| x.name == name)).map(|x| x.value.clone());
    let split = get("split").filter(|s| !s.is_empty());
    let feature = get("isFeatureSplit").as_deref() == Some("true");
    let kind = match (&split, feature) {
        (None, _) => "base",
        (Some(_), true) => "feature",
        (Some(_), false) => "config",
    };
    json!({
        "kind": kind,
        "split": split,
        "config_for": get("configForSplit").filter(|s| !s.is_empty()),
        "configures": split.as_deref().map(configures),
        "package": m.package,
        "version_code": m.version_code,
        "version_name": m.version_name,
        "min_sdk": m.min_sdk,
        "target_sdk": m.target_sdk,
        "compile_sdk": m.compile_sdk,
        "components": m.components.len(),
        "exported_components": m.components.iter().filter(|c| c.exported).count(),
        "permissions": m.uses_permissions.len(),
        "manifest_truncated": doc.truncated,
    })
}

/// What a configuration split's name says it holds.
fn configures(split: &str) -> &'static str {
    let last = split.rsplit('.').next().unwrap_or(split).to_ascii_lowercase();
    const ABIS: &[&str] = &["arm64_v8a", "armeabi_v7a", "armeabi", "x86", "x86_64", "mips", "mips64", "riscv64"];
    const DENSITIES: &[&str] = &["ldpi", "mdpi", "tvdpi", "hdpi", "xhdpi", "xxhdpi", "xxxhdpi", "nodpi", "anydpi"];
    if ABIS.contains(&last.as_str()) {
        "abi"
    } else if DENSITIES.contains(&last.as_str()) {
        "density"
    } else if (2..=3).contains(&last.len()) && last.chars().all(|c| c.is_ascii_lowercase()) || last.contains("_r") && last.len() <= 7 {
        "language"
    } else if split.contains(".config.") || split.starts_with("config.") {
        "other"
    } else {
        "feature"
    }
}

/// DEX files and native ABIs under `prefix` in an archive.
fn code_of(z: &zip::ZipArchive, prefix: &str, dex_dir: &str) -> (usize, Vec<String>) {
    let dex = z
        .entries
        .iter()
        .filter(|e| e.name.strip_prefix(prefix).and_then(|r| r.strip_prefix(dex_dir)).is_some_and(|r| r.ends_with(".dex") && !r.contains('/')))
        .count();
    let abis: BTreeSet<String> = z
        .entries
        .iter()
        .filter_map(|e| e.name.strip_prefix(prefix)?.strip_prefix("lib/")?.split_once('/').map(|(abi, _)| abi.to_string()))
        .filter(|a| !a.is_empty())
        .take(64)
        .collect();
    (dex, abis.into_iter().collect())
}

/// One APK inside a set: its manifest facts and code.
fn read_apk(bytes: &[u8], file: &str) -> Value {
    let z = match zip::parse(bytes, 500_000) {
        Ok(z) => z,
        Err(e) => return json!({"file": file, "error": format!("not a readable APK: {e}")}),
    };
    let (dex, abis) = code_of(&z, "", "");
    let Some(m) = z.entries.iter().find(|e| e.name == "AndroidManifest.xml") else {
        return json!({"file": file, "error": "no AndroidManifest.xml", "dex_files": dex, "abis": abis});
    };
    let doc = zip::read_entry(bytes, m, MAX_MANIFEST).map_err(|e| e.to_string()).and_then(|b| axml::parse(&b).map_err(|e| e.to_string()));
    match doc {
        Ok(doc) => {
            let mut v = part_facts(&doc);
            if let Value::Object(o) = &mut v {
                o.insert("file".into(), json!(file));
                o.insert("dex_files".into(), json!(dex));
                o.insert("abis".into(), json!(abis));
            }
            v
        }
        Err(e) => json!({"file": file, "error": format!("manifest: {e}"), "dex_files": dex, "abis": abis}),
    }
}

fn android_bundle_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: BundleParams = parse_params(params)?;
    let input = static_input::load(ctx, p.path.as_deref(), MAX_FILE)?;
    if !zip::looks_like_zip(&input.bytes) {
        return Err(ToolError::unsupported(format!("{} is not a ZIP, so not an .apks, .xapk, .aab or .apk", input.name)));
    }
    let z = zip::parse(&input.bytes, 500_000).map_err(|e| ToolError::new(ErrorCode::NotSupported, format!("Couldn't read the ZIP directory: {e}")))?;
    let mut lims = Vec::new();
    let mut parts = Vec::new();
    let mut truncated = false;

    let modules: Vec<&str> =
        z.entries.iter().filter_map(|e| e.name.strip_suffix("/manifest/AndroidManifest.xml")).filter(|m| !m.is_empty() && !m.contains('/')).collect();
    let format = if !modules.is_empty() {
        for (i, module) in modules.iter().enumerate() {
            ctx.cancel.check()?;
            if i >= MAX_PARTS {
                truncated = true;
                break;
            }
            let name = format!("{module}/manifest/AndroidManifest.xml");
            let prefix = format!("{module}/");
            let (dex, abis) = code_of(&z, &prefix, "dex/");
            let doc = z
                .entries
                .iter()
                .find(|e| e.name == name)
                .ok_or_else(|| "missing".to_string())
                .and_then(|e| zip::read_entry(&input.bytes, e, MAX_MANIFEST).map_err(|e| e.to_string()))
                .and_then(|b| aapt_proto::parse(&b).map_err(|e| e.to_string()));
            let mut v = match doc {
                Ok(doc) => part_facts(&doc),
                Err(e) => json!({"error": format!("manifest: {e}")}),
            };
            if let Value::Object(o) = &mut v {
                o.insert("module".into(), json!(module));
                o.insert("dex_files".into(), json!(dex));
                o.insert("abis".into(), json!(abis));
                o.insert("assets".into(), json!(z.entries.iter().filter(|e| e.name.starts_with(&format!("{prefix}assets/"))).count()));
                // In a bundle every module is a split of one app; only `base` is the base.
                if *module != "base" && o.get("kind").and_then(Value::as_str) == Some("base") {
                    o.insert("kind".into(), json!("feature"));
                }
            }
            parts.push(v);
        }
        lims.push(Limitation::new(
            "bundle_not_signed_apks",
            "A bundle is what a store turns into APKs; the APKs a device gets (and their signatures) are made from it later, so signing isn't read here.",
        ));
        "aab"
    } else {
        let apks: Vec<&zip::ZipEntry> = z.entries.iter().filter(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".apk")).collect();
        if apks.is_empty() {
            if z.entries.iter().any(|e| e.name == "AndroidManifest.xml") {
                parts.push(read_apk(&input.bytes, &input.name));
                "apk"
            } else {
                return Err(ToolError::unsupported(format!("{} holds no APKs and no bundle modules", input.name))
                    .with_remediation("Pass an .apks, .xapk, .aab or .apk file."));
            }
        } else {
            for (i, e) in apks.iter().enumerate() {
                ctx.cancel.check()?;
                if i >= MAX_PARTS {
                    truncated = true;
                    break;
                }
                ctx.report(Some(i as f32 / apks.len() as f32), format!("Reading {}", e.name));
                match zip::read_entry(&input.bytes, e, MAX_INNER) {
                    Ok(b) => parts.push(read_apk(&b, &e.name)),
                    Err(err) => parts.push(json!({"file": e.name, "error": err.to_string()})),
                }
            }
            "apk-set"
        }
    };
    if truncated {
        lims.push(Limitation::new("truncated", format!("Only the first {MAX_PARTS} parts were read.")));
    }

    let base = parts.iter().find(|p| p["kind"].as_str() == Some("base"));
    let packages: BTreeSet<&str> = parts.iter().filter_map(|p| p["package"].as_str()).collect();
    let versions: BTreeSet<&str> = parts.iter().filter_map(|p| p["version_code"].as_str()).collect();
    let failed = parts.iter().filter(|p| p.get("error").is_some()).count();
    if base.is_none() {
        lims.push(Limitation::new("no_base", "No part is the base (a manifest without a split name); the set may be incomplete."));
    }
    if packages.len() > 1 {
        lims.push(Limitation::new(
            "packages_differ",
            format!("The parts name different packages: {}.", packages.iter().copied().collect::<Vec<_>>().join(", ")),
        ));
    }
    if versions.len() > 1 {
        lims.push(Limitation::new(
            "versions_differ",
            format!("The parts have different version codes: {}.", versions.iter().copied().collect::<Vec<_>>().join(", ")),
        ));
    }
    if failed > 0 {
        lims.push(Limitation::new("parts_unreadable", format!("{failed} part(s) couldn't be read; each says why.")));
    }
    let others: Vec<&str> = z
        .entries
        .iter()
        .map(|e| e.name.as_str())
        .filter(|n| matches!(*n, "manifest.json" | "toc.pb" | "BundleConfig.pb" | "icon.png") || n.to_ascii_lowercase().ends_with(".obb"))
        .take(64)
        .collect();
    let count = |k: &str| parts.iter().filter(|p| p["kind"].as_str() == Some(k)).count();
    let package = base.and_then(|b| b["package"].as_str()).or_else(|| packages.iter().next().copied());
    let summary = format!(
        "{} ({format}): {} part(s): {} base, {} feature, {} configuration split(s){}.",
        package.unwrap_or("unknown package"),
        parts.len(),
        count("base"),
        count("feature"),
        count("config"),
        if failed > 0 { format!(", {failed} unreadable") } else { String::new() }
    );
    let mut coverage = Coverage::complete(parts.len() as u64);
    if truncated || failed > 0 {
        coverage.status = CoverageStatus::Partial;
    }
    let e = builder(ctx, BUNDLE_TOOL, input.subject.clone(), json!({"path": p.path}))
        .authority(Authority::OriginalFiles)
        .location(Location::new("path", input.path.display().to_string()))
        .result(json!({
            "format": format,
            "package": package,
            "version_code": base.and_then(|b| b.get("version_code")).cloned(),
            "min_sdk": base.and_then(|b| b.get("min_sdk")).cloned(),
            "target_sdk": base.and_then(|b| b.get("target_sdk")).cloned(),
            "parts": parts,
            "other_files": others,
        }))
        .summary(summary)
        .confidence(Confidence::Observed)
        .limitations(lims)
        .coverage(coverage)
        .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::native_common::test_support::{call, call_err};

    #[test]
    fn a_split_name_says_what_it_configures() {
        for (s, want) in [
            ("config.arm64_v8a", "abi"),
            ("config.xxhdpi", "density"),
            ("config.pt", "language"),
            ("config.zh_rtw", "language"),
            ("dynamic_feature", "feature"),
            ("dynamic_feature.config.x86_64", "abi"),
        ] {
            assert_eq!(configures(s), want, "{s}");
        }
    }

    fn engine() -> crate::Engine {
        let mut s = crate::testing::session();
        s.settings.deep_mode = true;
        crate::Engine::new(s)
    }

    use craft_anything_testkit::android as fx;
    use craft_anything_testkit::electron::zip_stored;

    /// An APK holding a manifest with these root attributes and these files.
    fn apk(package: &str, root: Vec<fx::AxAttr<'_>>, files: &[(&str, &[u8])]) -> Vec<u8> {
        use fx::AxNode::{End, Start};
        let mut attrs = vec![fx::AxAttr { name: "package", res_id: None, val: fx::AxVal::Str(package) }, fx::a("versionCode", 0x0101_021b, fx::AxVal::Int(7))];
        attrs.extend(root);
        let m = fx::axml(&[Start("manifest", attrs), Start("application", vec![]), End("application"), End("manifest")]);
        let mut all: Vec<(&str, &[u8])> = vec![("AndroidManifest.xml", &m)];
        all.extend_from_slice(files);
        zip_stored(&all)
    }

    #[test]
    fn a_split_set_is_read_as_one_app() {
        let split = |s| fx::AxAttr { name: "split", res_id: None, val: fx::AxVal::Str(s) };
        let base = apk("com.example.app", vec![], &[("classes.dex", b"dex\n035\0"), ("classes2.dex", b"x")]);
        let abi = apk("com.example.app", vec![split("config.arm64_v8a")], &[("lib/arm64-v8a/libx.so", b"\x7fELF")]);
        let feature = apk("com.example.app", vec![split("camera"), fx::a("isFeatureSplit", 0x0101_055b, fx::AxVal::Bool(true))], &[]);
        let odd = apk("com.other", vec![split("config.fr")], &[]);
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("app.apks");
        std::fs::write(
            &f,
            zip_stored(&[
                ("splits/base-master.apk", &base),
                ("splits/base-arm64_v8a.apk", &abi),
                ("splits/camera-master.apk", &feature),
                ("splits/base-fr.apk", &odd),
                ("toc.pb", b""),
            ]),
        )
        .unwrap();
        let v = call(&engine(), BUNDLE_TOOL, json!({"path": f}));
        let r = &v["result"];
        assert_eq!((r["format"].as_str(), r["package"].as_str(), r["version_code"].as_str()), (Some("apk-set"), Some("com.example.app"), Some("7")));
        let kinds: Vec<(&str, &str)> = r["parts"].as_array().unwrap().iter().map(|p| (p["file"].as_str().unwrap(), p["kind"].as_str().unwrap())).collect();
        assert_eq!(
            kinds,
            vec![
                ("splits/base-master.apk", "base"),
                ("splits/base-arm64_v8a.apk", "config"),
                ("splits/camera-master.apk", "feature"),
                ("splits/base-fr.apk", "config")
            ]
        );
        assert_eq!(r["parts"][0]["dex_files"], 2);
        assert_eq!(r["parts"][1]["abis"], json!(["arm64-v8a"]));
        assert_eq!(r["parts"][1]["configures"], "abi");
        assert_eq!(r["parts"][3]["configures"], "language");
        assert_eq!(r["other_files"], json!(["toc.pb"]));
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"packages_differ"), "{codes:?}");
    }

    #[test]
    fn a_bundle_is_read_by_module() {
        fn var(mut v: usize) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let b = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    out.push(b);
                    return out;
                }
                out.push(b | 0x80);
            }
        }
        let field = |num: u8, b: &[u8]| [vec![num << 3 | 2], var(b.len()), b.to_vec()].concat();
        let attr = |name: &str, value: &str| field(4, &[field(2, name.as_bytes()), field(3, value.as_bytes())].concat());
        let manifest = |attrs: &[Vec<u8>]| field(1, &[field(3, b"manifest"), attrs.concat()].concat());
        let base = manifest(&[attr("package", "com.example.app"), attr("versionCode", "12")]);
        let feature = manifest(&[attr("package", "com.example.app"), attr("split", "maps"), attr("isFeatureSplit", "true")]);
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("app.aab");
        std::fs::write(
            &f,
            zip_stored(&[
                ("base/manifest/AndroidManifest.xml", &base),
                ("base/dex/classes.dex", b"x"),
                ("base/lib/x86_64/liba.so", b"x"),
                ("maps/manifest/AndroidManifest.xml", &feature),
                ("BundleConfig.pb", b""),
            ]),
        )
        .unwrap();
        let v = call(&engine(), BUNDLE_TOOL, json!({"path": f}));
        let r = &v["result"];
        assert_eq!((r["format"].as_str(), r["package"].as_str(), r["version_code"].as_str()), (Some("aab"), Some("com.example.app"), Some("12")));
        assert_eq!((r["parts"][0]["module"].as_str(), r["parts"][0]["kind"].as_str()), (Some("base"), Some("base")));
        assert_eq!((r["parts"][0]["dex_files"].as_u64(), &r["parts"][0]["abis"]), (Some(1), &json!(["x86_64"])));
        assert_eq!((r["parts"][1]["module"].as_str(), r["parts"][1]["kind"].as_str()), (Some("maps"), Some("feature")));
    }

    #[test]
    fn odex_and_vdex_files_are_named_and_others_refused() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("base.vdex");
        let mut vdex = b"vdex027\0".to_vec();
        vdex.extend(1u32.to_le_bytes());
        vdex.extend([0, 0, 0, 0, 24, 0, 0, 0, 4, 0, 0, 0, 1, 2, 3, 4]);
        std::fs::write(&v, &vdex).unwrap();
        let r = call(&engine(), ODEX_TOOL, json!({"path": v}));
        assert_eq!((r["result"]["kind"].as_str(), r["result"]["dex_files"].as_u64()), (Some("vdex"), Some(1)));
        let codes: Vec<&str> = r["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"no_bytecode_inside"), "{codes:?}");
        let o = d.path().join("base.odex");
        std::fs::write(&o, [b"\x7fELF".as_slice(), &[0; 12], b"oat\n238\0compiler-filter\0verify\0"].concat()).unwrap();
        let r = call(&engine(), ODEX_TOOL, json!({"path": o}));
        assert_eq!(r["result"]["kind"], "oat");
        assert!(r["summary"].as_str().unwrap().contains("verify"));
        let codes: Vec<&str> = r["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"oat_header_not_read"), "{codes:?}");
        let a = d.path().join("boot.art");
        std::fs::write(&a, b"art\n108\0\0\0\0\0").unwrap();
        let r = call(&engine(), ODEX_TOOL, json!({"path": a}));
        assert_eq!((r["result"]["kind"].as_str(), r["result"]["version"].as_str()), (Some("image"), Some("108")));
        let x = d.path().join("x.bin");
        std::fs::write(&x, b"hello").unwrap();
        assert_eq!(call_err(&engine(), ODEX_TOOL, json!({"path": x})).code, ErrorCode::NotSupported);
    }

    #[test]
    fn an_oat_files_header_and_dex_list_are_read_and_paged() {
        use craft_anything_testkit::android::{OatDexSpec, oat_file};
        let d = tempfile::tempdir().unwrap();
        let o = d.path().join("base.odex");
        let dexes: Vec<OatDexSpec> =
            (0..3).map(|i| OatDexSpec { location: format!("/data/app/x/base.apk!classes{i}.dex"), checksum: i, dex_offset: 0 }).collect();
        std::fs::write(&o, oat_file("275", 2, &dexes, &[("compiler-filter", "speed-profile")], 64)).unwrap();
        let r = call(&engine(), ODEX_TOOL, json!({"path": o, "offset": 1, "limit": 1}));
        let res = &r["result"];
        assert_eq!((res["header"]["instruction_set"].as_str(), res["header"]["dex_file_count"].as_u64()), (Some("arm64"), Some(3)));
        assert_eq!((res["dex_list_total"].as_u64(), res["dex_list"].as_array().unwrap().len()), (Some(3), 1));
        assert_eq!(res["dex_list"][0]["location"], "/data/app/x/base.apk!classes1.dex");
        assert_eq!(r["coverage"]["next_offset"], 2);
        let s = r["summary"].as_str().unwrap();
        assert!(s.contains("arm64") && s.contains("speed-profile") && s.contains("3 DEX file(s) listed"), "{s}");
        // A layout known only from documentation says so.
        std::fs::write(&o, oat_file("225", 2, &dexes, &[], 64)).unwrap();
        let r = call(&engine(), ODEX_TOOL, json!({"path": o}));
        let codes: Vec<&str> = r["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"oat_layout_from_documentation"), "{codes:?}");
    }

    #[test]
    fn a_zip_with_no_app_in_it_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.zip");
        std::fs::write(&f, zip_stored(&[("readme.txt", b"hi")])).unwrap();
        assert_eq!(call_err(&engine(), BUNDLE_TOOL, json!({"path": f})).code, ErrorCode::NotSupported);
        let g = d.path().join("y.apks");
        std::fs::write(&g, b"not a zip").unwrap();
        assert_eq!(call_err(&engine(), BUNDLE_TOOL, json!({"path": g})).code, ErrorCode::NotSupported);
    }

    #[test]
    fn a_set_with_a_broken_part_says_so() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.apks");
        std::fs::write(&f, zip_stored(&[("splits/base-master.apk", b"garbage")])).unwrap();
        let v = call(&engine(), BUNDLE_TOOL, json!({"path": f}));
        assert_eq!(v["result"]["format"], "apk-set");
        assert!(v["result"]["parts"][0]["error"].is_string());
        let codes: Vec<&str> = v["limitations"].as_array().unwrap().iter().filter_map(|l| l["code"].as_str()).collect();
        assert!(codes.contains(&"no_base") && codes.contains(&"parts_unreadable"), "{codes:?}");
    }
}
