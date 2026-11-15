//! iOS app archives (`.ipa`) read as one app: the ZIP's `Payload/<Name>.app/` bundle, its
//! Info.plist, the main executable's architectures, encryption, linked libraries and code-signature
//! entitlements, the embedded provisioning profile, and the bundles inside it (frameworks, app
//! extensions, watch apps) plus what else the archive carries (`SwiftSupport/`, `Symbols/`,
//! `iTunesMetadata.plist`).
//!
//! Nothing is extracted or written: entry names are compared as text and never joined to a path.
//! Every read is capped (entry count, bytes per entry, bytes decompressed in total, bundles read,
//! rows kept per list) and a cap that cut something says so. No signature is verified here.

use crate::{codesign, der, macho_deps, pkcs7, plist, zip};
use serde::Serialize;
use serde_json::Value as Json;
use std::collections::BTreeSet;

/// What one read may use.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// ZIP entries listed.
    pub max_entries: usize,
    /// Bytes one property list (Info.plist of any bundle) may decompress to.
    pub max_plist_bytes: u64,
    /// Bytes the main executable may decompress to.
    pub max_executable_bytes: u64,
    /// Bytes `embedded.mobileprovision` may decompress to.
    pub max_profile_bytes: u64,
    /// Bytes decompressed by the whole read.
    pub max_total_bytes: u64,
    /// Nested bundles (frameworks, extensions, watch apps) whose Info.plist is read.
    pub max_bundles: usize,
    /// Rows kept in each list; the total is always counted.
    pub max_rows: usize,
    /// Characters kept of one text value (a usage description).
    pub max_text_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_entries: 200_000,
            max_plist_bytes: 4 << 20,
            max_executable_bytes: 256 << 20,
            max_profile_bytes: 1 << 20,
            max_total_bytes: 512 << 20,
            max_bundles: 256,
            max_rows: 10_000,
            max_text_chars: 1_000,
        }
    }
}

/// A list kept up to a cap, with the complete count beside it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capped<T> {
    pub total: usize,
    pub items: Vec<T>,
    /// True when `items` holds fewer than `total`.
    pub rows_capped: bool,
}

impl<T> Default for Capped<T> {
    fn default() -> Self {
        Capped { total: 0, items: Vec::new(), rows_capped: false }
    }
}

impl<T> Capped<T> {
    fn push(&mut self, item: T, cap: usize) {
        self.total = self.total.saturating_add(1);
        if self.items.len() < cap {
            self.items.push(item);
        } else {
            self.rows_capped = true;
        }
    }
}

/// The whole read.
#[derive(Debug, Clone, Serialize)]
pub struct Ipa {
    /// ZIP entries listed.
    pub entries: usize,
    /// Entries the archive's end record declares.
    pub declared_entries: u64,
    /// The listing stopped at the entry cap or at damage.
    pub listing_truncated: bool,
    /// Every `Payload/<Name>.app` folder found (sorted).
    pub apps: Capped<String>,
    /// The one read (the only one, or the first in sorted order when there are several).
    pub app: Option<String>,
    pub extras: Extras,
    pub info: Option<Info>,
    pub info_error: Option<String>,
    pub executable: Option<Executable>,
    pub executable_error: Option<String>,
    pub profile: Option<Profile>,
    pub profile_error: Option<String>,
    /// `Frameworks/*.framework` and `Frameworks/*.dylib` inside the app.
    pub frameworks: Capped<Bundle>,
    /// `PlugIns/*.appex` and `Extensions/*.appex` inside the app (and inside its watch apps).
    pub extensions: Capped<Bundle>,
    /// `Watch/*.app` inside the app.
    pub watch_apps: Capped<Bundle>,
    /// Files under the app folder.
    pub app_files: usize,
    /// Nested bundles whose Info.plist wasn't read because `max_bundles` was reached.
    pub bundles_not_read: usize,
    /// A read was refused because the whole-read byte budget was spent.
    pub budget_exhausted: bool,
    /// Every entry name (lower case not applied), for signal tables; not serialized.
    #[serde(skip)]
    pub names: Vec<String>,
    pub warnings: Vec<String>,
}

/// What the archive holds beside the app.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Extras {
    /// Files under `SwiftSupport/` (Swift runtime libraries an App Store upload carries).
    pub swift_support: Capped<String>,
    /// Files under `Symbols/` (debug symbol files an upload may carry).
    pub symbols_files: usize,
    /// Files under `BCSymbolMaps/`.
    pub bcsymbolmaps_files: usize,
    /// `iTunesMetadata.plist` is present. Its contents are not read: it can hold the buyer's
    /// account details.
    pub itunes_metadata: bool,
    pub itunes_artwork: bool,
    /// Other top-level names (not `Payload`, `SwiftSupport`, `Symbols`, `BCSymbolMaps`,
    /// `iTunesMetadata.plist`, `iTunesArtwork`).
    pub other_top_level: Capped<String>,
}

/// A bundle inside the app.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bundle {
    /// Path inside the archive (`Payload/X.app/Frameworks/Y.framework`).
    pub path: String,
    pub name: String,
    /// `framework`, `dylib`, `appex` or `app`.
    pub kind: &'static str,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub executable: Option<String>,
    /// An extension's `NSExtensionPointIdentifier` (or `EXExtensionPointIdentifier`).
    pub extension_point: Option<String>,
    /// Why its Info.plist couldn't be read (absent for a dylib, which has none).
    pub info_error: Option<String>,
}

/// One usage-description key and its text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageDescription {
    pub key: String,
    pub text: String,
    /// The text was shortened to `max_text_chars`.
    pub shortened: bool,
}

/// One App Transport Security exception domain.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AtsDomain {
    pub domain: String,
    pub allows_insecure_http: Option<bool>,
    pub includes_subdomains: Option<bool>,
    pub minimum_tls: Option<String>,
}

/// App Transport Security settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Ats {
    pub allows_arbitrary_loads: Option<bool>,
    pub allows_arbitrary_loads_in_web_content: Option<bool>,
    pub allows_arbitrary_loads_for_media: Option<bool>,
    pub allows_local_networking: Option<bool>,
    pub exception_domains: Capped<AtsDomain>,
}

/// One scene configuration from `UIApplicationSceneManifest`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scene {
    pub role: String,
    pub name: Option<String>,
    pub delegate_class: Option<String>,
    pub storyboard: Option<String>,
}

/// Facts from the app's Info.plist.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Info {
    /// `binary` or `xml`.
    pub encoding: &'static str,
    pub keys: usize,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
    pub build: Option<String>,
    pub display_name: Option<String>,
    pub name: Option<String>,
    pub executable: Option<String>,
    pub minimum_os: Option<String>,
    pub platforms: Vec<String>,
    pub sdk: Option<String>,
    pub xcode: Option<String>,
    /// From `UIDeviceFamily`: `iphone`, `ipad`, `apple-tv`, `apple-watch`, `mac`, `apple-vision`.
    pub device_families: Vec<String>,
    /// `UIRequiredDeviceCapabilities`; a dictionary entry set to false reads `!name` (must be absent).
    pub required_capabilities: Capped<String>,
    pub url_schemes: Capped<String>,
    pub queried_schemes: Capped<String>,
    pub usage_descriptions: Capped<UsageDescription>,
    pub ats: Option<Ats>,
    pub background_modes: Capped<String>,
    pub launch_storyboard: Option<String>,
    pub main_storyboard: Option<String>,
    pub supports_multiple_scenes: Option<bool>,
    pub scenes: Capped<Scene>,
}

/// The entitlement keys that say most about an iOS app.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EntitlementFacts {
    pub keys: usize,
    pub application_identifier: Option<String>,
    pub team_identifier: Option<String>,
    pub keychain_groups: Capped<String>,
    /// `aps-environment`: `development` or `production`.
    pub push_environment: Option<String>,
    pub associated_domains: Capped<String>,
    pub app_groups: Capped<String>,
    pub get_task_allow: Option<bool>,
}

/// One architecture slice of the main executable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SliceFacts {
    pub arch: String,
    pub offset: u64,
    pub size: u64,
    /// `LC_ENCRYPTION_INFO(_64)` is present.
    pub has_encryption_info: bool,
    /// Its `cryptid`; non-zero means the code pages are encrypted.
    pub cryptid: Option<u32>,
    pub crypt_size: Option<u32>,
    pub platform: Option<String>,
    pub minimum_os: Option<String>,
    pub sdk: Option<String>,
    pub error: Option<String>,
}

/// One library the executable links.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Library {
    pub name: String,
    /// The load command: `load`, `weak`, `reexport`, `lazy`, `upward`.
    pub kind: &'static str,
    /// `system` (`/System/`, `/usr/lib/`), `embedded` (`@rpath`, `@executable_path`,
    /// `@loader_path`) or `other`.
    pub origin: &'static str,
}

/// The main executable's code signature, as far as it was read.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SignatureFacts {
    pub identifier: Option<String>,
    pub team_id: Option<String>,
    pub ad_hoc: bool,
    pub cms_present: bool,
    pub entitlements: Option<Json>,
    pub entitlement_facts: Option<EntitlementFacts>,
    /// DER entitlements are present but not decoded here.
    pub der_entitlements_bytes: Option<usize>,
    pub warnings: Vec<String>,
}

/// The main executable.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Executable {
    pub path: String,
    pub bytes: u64,
    pub universal: bool,
    pub slices: Vec<SliceFacts>,
    /// The slice whose libraries and signature were read (arm64 preferred).
    pub read_slice: Option<String>,
    /// Any slice has a non-zero `cryptid`.
    pub encrypted: bool,
    pub libraries: Capped<Library>,
    pub signature: Option<SignatureFacts>,
    pub signature_error: Option<String>,
}

/// One developer certificate a provisioning profile lists.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileCertificate {
    pub subject: Option<String>,
    pub not_after: Option<String>,
    pub bytes: usize,
}

/// The embedded provisioning profile (a CMS envelope around a property list).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Profile {
    pub bytes: usize,
    /// `cms_content` (the envelope's encapsulated content) or `located_plist` (the property list
    /// found by its XML markers when the envelope couldn't be read as CMS).
    pub read_via: &'static str,
    /// Always false: the envelope's signature is not checked.
    pub signature_verified: bool,
    pub cms_certificates: usize,
    pub name: Option<String>,
    pub app_id_name: Option<String>,
    pub uuid: Option<String>,
    pub team_ids: Vec<String>,
    pub team_name: Option<String>,
    pub platforms: Vec<String>,
    pub creation_date: Option<String>,
    pub expiration_date: Option<String>,
    /// Devices listed in `ProvisionedDevices`, or none when the key is absent.
    pub provisioned_devices: Option<usize>,
    pub provisions_all_devices: bool,
    /// `enterprise` (all devices), `development` (devices, get-task-allow), `ad_hoc` (devices) or
    /// `app_store` (no device list). Read from the profile's keys, not from Apple.
    pub kind: &'static str,
    pub entitlements: Option<Json>,
    pub entitlement_facts: Option<EntitlementFacts>,
    pub certificates: Capped<ProfileCertificate>,
}

/// True when a ZIP's entries include a `Payload/<Name>.app/` folder.
pub fn looks_like_ipa(names: &[&str]) -> bool {
    names.iter().any(|n| app_root(n).is_some())
}

/// The `Payload/<Name>.app` an entry name sits in.
fn app_root(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("Payload/")?;
    let (first, _) = rest.split_once('/')?;
    if first.len() > 4 && first.to_ascii_lowercase().ends_with(".app") && valid_component(first) { name.get(..("Payload/".len() + first.len())) } else { None }
}

/// A name component that is a plain name (no traversal, no separators).
fn valid_component(c: &str) -> bool {
    !c.is_empty() && c != "." && c != ".." && !c.contains('\\') && !c.contains('\0') && !c.contains(':')
}

/// Read the archive. Fails only when it isn't a readable ZIP; everything after that is reported
/// in the result.
pub fn parse(data: &[u8], limits: &Limits) -> Result<Ipa, zip::ZipError> {
    let z = zip::parse(data, limits.max_entries)?;
    let mut ipa = Ipa {
        entries: z.entries.len(),
        declared_entries: z.declared_entries,
        listing_truncated: z.truncated,
        apps: Capped::default(),
        app: None,
        extras: Extras::default(),
        info: None,
        info_error: None,
        executable: None,
        executable_error: None,
        profile: None,
        profile_error: None,
        frameworks: Capped::default(),
        extensions: Capped::default(),
        watch_apps: Capped::default(),
        app_files: 0,
        bundles_not_read: 0,
        budget_exhausted: false,
        names: z.entries.iter().map(|e| e.name.clone()).collect(),
        warnings: z.warnings.iter().map(|w| format!("archive: {w}")).collect(),
    };
    let mut reader = Reader { data, z: &z, left: limits.max_total_bytes, exhausted: false };

    // Top level.
    let mut apps = BTreeSet::new();
    let mut other_top = BTreeSet::new();
    for e in &z.entries {
        if let Some(root) = app_root(&e.name) {
            apps.insert(root.to_string());
            continue;
        }
        let first = e.name.split('/').next().unwrap_or("");
        match first {
            "SwiftSupport" if !e.is_dir => ipa.extras.swift_support.push(e.name.clone(), limits.max_rows),
            "Symbols" if !e.is_dir => ipa.extras.symbols_files = ipa.extras.symbols_files.saturating_add(1),
            "BCSymbolMaps" if !e.is_dir => ipa.extras.bcsymbolmaps_files = ipa.extras.bcsymbolmaps_files.saturating_add(1),
            "iTunesMetadata.plist" => ipa.extras.itunes_metadata = true,
            "iTunesArtwork" | "iTunesArtwork@2x" => ipa.extras.itunes_artwork = true,
            "Payload" | "SwiftSupport" | "Symbols" | "BCSymbolMaps" | "" => {}
            other => {
                other_top.insert(other.to_string());
            }
        }
    }
    for o in other_top {
        ipa.extras.other_top_level.push(o, limits.max_rows);
    }
    for a in &apps {
        ipa.apps.push(a.clone(), limits.max_rows);
    }
    let Some(root) = apps.into_iter().next() else {
        return Ok(ipa);
    };
    if ipa.apps.total > 1 {
        ipa.warnings.push(format!("Payload holds {} app folders; read {root}, the first in sorted order", ipa.apps.total));
    }
    ipa.app = Some(root.clone());
    let prefix = format!("{root}/");
    ipa.app_files = z.entries.iter().filter(|e| !e.is_dir && e.name.starts_with(&prefix)).count();

    // Info.plist.
    let info_value = match reader.read(&format!("{prefix}Info.plist"), limits.max_plist_bytes) {
        Ok(Some(bytes)) => match plist::parse(&bytes) {
            Ok(v) => {
                let encoding = if bytes.starts_with(b"bplist") { "binary" } else { "xml" };
                ipa.info = Some(info_facts(&v, encoding, limits));
                Some(v)
            }
            Err(e) => {
                ipa.info_error = Some(format!("Info.plist: {e}"));
                None
            }
        },
        Ok(None) => {
            ipa.info_error = Some("the app has no Info.plist".into());
            None
        }
        Err(e) => {
            ipa.info_error = Some(format!("Info.plist: {e}"));
            None
        }
    };

    // The main executable: CFBundleExecutable, else the folder's name without `.app`.
    let folder = root.rsplit('/').next().unwrap_or("");
    let fallback = folder.get(..folder.len().saturating_sub(4)).unwrap_or("").to_string();
    let exe_name = info_value.as_ref().and_then(|v| v.get("CFBundleExecutable")).and_then(plist::Value::as_str).map(str::to_string).unwrap_or(fallback);
    if exe_name.is_empty() || !valid_component(&exe_name) || exe_name.contains('/') {
        ipa.executable_error = Some(format!("the executable name {exe_name:?} isn't a plain file name"));
    } else {
        let path = format!("{prefix}{exe_name}");
        match reader.read(&path, limits.max_executable_bytes) {
            Ok(Some(bytes)) => match executable_facts(&path, &bytes, limits) {
                Ok(x) => ipa.executable = Some(x),
                Err(e) => ipa.executable_error = Some(e),
            },
            Ok(None) => ipa.executable_error = Some(format!("{path} is not in the archive")),
            Err(e) => ipa.executable_error = Some(format!("{path}: {e}")),
        }
    }

    // The provisioning profile.
    match reader.read(&format!("{prefix}embedded.mobileprovision"), limits.max_profile_bytes) {
        Ok(Some(bytes)) => match profile_facts(&bytes, limits) {
            Ok(p) => ipa.profile = Some(p),
            Err(e) => ipa.profile_error = Some(e),
        },
        Ok(None) => {}
        Err(e) => ipa.profile_error = Some(format!("embedded.mobileprovision: {e}")),
    }

    // Nested bundles, found by their folder names.
    let mut found: BTreeSet<(String, &'static str)> = BTreeSet::new();
    for e in &z.entries {
        let Some(rest) = e.name.strip_prefix(&prefix) else { continue };
        let parts: Vec<&str> = rest.split('/').collect();
        match parts.as_slice() {
            ["Frameworks", name, ..] if lower_ends(name, ".framework") && parts.len() > 2 => {
                found.insert((format!("{prefix}Frameworks/{name}"), "framework"));
            }
            ["Frameworks", name] if lower_ends(name, ".dylib") && !e.is_dir => {
                found.insert((format!("{prefix}Frameworks/{name}"), "dylib"));
            }
            ["PlugIns" | "Extensions", name, _, ..] if lower_ends(name, ".appex") => {
                found.insert((format!("{prefix}{}/{name}", parts.first().unwrap_or(&"")), "appex"));
            }
            ["Watch", app, _, ..] if lower_ends(app, ".app") => {
                found.insert((format!("{prefix}Watch/{app}"), "app"));
                if let [_, _, "PlugIns", ext, _, ..] = parts.as_slice()
                    && lower_ends(ext, ".appex")
                {
                    found.insert((format!("{prefix}Watch/{app}/PlugIns/{ext}"), "appex"));
                }
            }
            _ => {}
        }
    }
    let mut read_bundles = 0usize;
    for (path, kind) in found {
        let name = path.rsplit('/').next().unwrap_or("").to_string();
        if !valid_component(&name) {
            continue;
        }
        let mut b = Bundle { path: path.clone(), name, kind, bundle_id: None, version: None, executable: None, extension_point: None, info_error: None };
        if kind != "dylib" {
            if read_bundles < limits.max_bundles {
                read_bundles = read_bundles.saturating_add(1);
                bundle_info(&mut reader, &mut b, limits);
            } else {
                ipa.bundles_not_read = ipa.bundles_not_read.saturating_add(1);
                b.info_error = Some("not read: the bundle cap was reached".into());
            }
        }
        let list = match kind {
            "appex" => &mut ipa.extensions,
            "app" => &mut ipa.watch_apps,
            _ => &mut ipa.frameworks,
        };
        list.push(b, limits.max_rows);
    }
    ipa.budget_exhausted = reader.exhausted;
    Ok(ipa)
}

fn lower_ends(name: &str, suffix: &str) -> bool {
    name.len() > suffix.len() && name.to_ascii_lowercase().ends_with(suffix)
}

/// Reads entries by exact name against one byte budget for the whole read.
struct Reader<'a> {
    data: &'a [u8],
    z: &'a zip::ZipArchive,
    left: u64,
    exhausted: bool,
}

impl Reader<'_> {
    /// `Ok(None)` when there's no such file entry.
    fn read(&mut self, name: &str, cap: u64) -> Result<Option<Vec<u8>>, String> {
        let Some(e) = self.z.entries.iter().find(|e| e.name == name && !e.is_dir) else {
            return Ok(None);
        };
        if e.uncompressed_size > cap {
            return Err(format!("{} bytes, over the {cap}-byte limit for this file", e.uncompressed_size));
        }
        if e.uncompressed_size > self.left {
            self.exhausted = true;
            return Err("not read: the read's total decompression budget is spent".into());
        }
        let bytes = zip::read_entry(self.data, e, cap.min(self.left)).map_err(|err| err.to_string())?;
        self.left = self.left.saturating_sub(bytes.len() as u64);
        Ok(Some(bytes))
    }
}

fn s(v: &plist::Value, key: &str) -> Option<String> {
    v.get(key).and_then(plist::Value::as_str).map(str::to_string)
}

fn b(v: &plist::Value, key: &str) -> Option<bool> {
    match v.get(key) {
        Some(plist::Value::Bool(x)) => Some(*x),
        _ => None,
    }
}

fn strings(v: Option<&plist::Value>) -> impl Iterator<Item = &str> {
    v.and_then(plist::Value::as_array).unwrap_or(&[]).iter().filter_map(plist::Value::as_str)
}

fn shorten(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((i, _)) => (text.get(..i).unwrap_or("").to_string(), true),
        None => (text.to_string(), false),
    }
}

fn info_facts(v: &plist::Value, encoding: &'static str, limits: &Limits) -> Info {
    let rows = limits.max_rows;
    let mut info = Info {
        encoding,
        keys: match v {
            plist::Value::Dict(d) => d.len(),
            _ => 0,
        },
        bundle_id: s(v, "CFBundleIdentifier"),
        version: s(v, "CFBundleShortVersionString"),
        build: s(v, "CFBundleVersion"),
        display_name: s(v, "CFBundleDisplayName"),
        name: s(v, "CFBundleName"),
        executable: s(v, "CFBundleExecutable"),
        minimum_os: s(v, "MinimumOSVersion").or_else(|| s(v, "LSMinimumSystemVersion")),
        platforms: strings(v.get("CFBundleSupportedPlatforms")).take(32).map(str::to_string).collect(),
        sdk: s(v, "DTSDKName"),
        xcode: s(v, "DTXcode"),
        launch_storyboard: s(v, "UILaunchStoryboardName"),
        main_storyboard: s(v, "UIMainStoryboardFile"),
        ..Info::default()
    };
    // UIDeviceFamily: an integer array (sometimes a single integer).
    let fam = |n: i128| match n {
        1 => "iphone".to_string(),
        2 => "ipad".to_string(),
        3 => "apple-tv".to_string(),
        4 => "apple-watch".to_string(),
        6 => "mac".to_string(),
        7 => "apple-vision".to_string(),
        other => format!("family {other}"),
    };
    match v.get("UIDeviceFamily") {
        Some(plist::Value::Array(a)) => info.device_families = a.iter().filter_map(plist::Value::as_int).take(16).map(fam).collect(),
        Some(plist::Value::Integer(n)) => info.device_families = vec![fam(*n)],
        _ => {}
    }
    match v.get("UIRequiredDeviceCapabilities") {
        Some(plist::Value::Array(a)) => a.iter().filter_map(plist::Value::as_str).for_each(|c| info.required_capabilities.push(c.to_string(), rows)),
        Some(plist::Value::Dict(d)) => d.iter().for_each(|(k, val)| {
            let absent = matches!(val, plist::Value::Bool(false));
            info.required_capabilities.push(if absent { format!("!{k}") } else { k.clone() }, rows)
        }),
        _ => {}
    }
    for t in v.get("CFBundleURLTypes").and_then(plist::Value::as_array).unwrap_or(&[]) {
        strings(t.get("CFBundleURLSchemes")).for_each(|x| info.url_schemes.push(x.to_string(), rows));
    }
    strings(v.get("LSApplicationQueriesSchemes")).for_each(|x| info.queried_schemes.push(x.to_string(), rows));
    strings(v.get("UIBackgroundModes")).for_each(|x| info.background_modes.push(x.to_string(), rows));
    if let plist::Value::Dict(d) = v {
        for (k, val) in d {
            if k.ends_with("UsageDescription") {
                let (text, shortened) = shorten(val.as_str().unwrap_or(""), limits.max_text_chars);
                info.usage_descriptions.push(UsageDescription { key: k.clone(), text, shortened }, rows);
            }
        }
    }
    if let Some(a) = v.get("NSAppTransportSecurity") {
        let mut ats = Ats {
            allows_arbitrary_loads: b(a, "NSAllowsArbitraryLoads"),
            allows_arbitrary_loads_in_web_content: b(a, "NSAllowsArbitraryLoadsInWebContent"),
            allows_arbitrary_loads_for_media: b(a, "NSAllowsArbitraryLoadsForMedia"),
            allows_local_networking: b(a, "NSAllowsLocalNetworking"),
            exception_domains: Capped::default(),
        };
        if let Some(plist::Value::Dict(domains)) = a.get("NSExceptionDomains") {
            for (domain, d) in domains {
                ats.exception_domains.push(
                    AtsDomain {
                        domain: domain.clone(),
                        allows_insecure_http: b(d, "NSExceptionAllowsInsecureHTTPLoads").or_else(|| b(d, "NSThirdPartyExceptionAllowsInsecureHTTPLoads")),
                        includes_subdomains: b(d, "NSIncludesSubdomains"),
                        minimum_tls: s(d, "NSExceptionMinimumTLSVersion").or_else(|| s(d, "NSThirdPartyExceptionMinimumTLSVersion")),
                    },
                    rows,
                );
            }
        }
        info.ats = Some(ats);
    }
    if let Some(m) = v.get("UIApplicationSceneManifest") {
        info.supports_multiple_scenes = b(m, "UIApplicationSupportsMultipleScenes");
        if let Some(plist::Value::Dict(roles)) = m.get("UISceneConfigurations") {
            for (role, configs) in roles {
                for c in configs.as_array().unwrap_or(&[]) {
                    info.scenes.push(
                        Scene {
                            role: role.clone(),
                            name: s(c, "UISceneConfigurationName"),
                            delegate_class: s(c, "UISceneDelegateClassName"),
                            storyboard: s(c, "UISceneStoryboardFile"),
                        },
                        rows,
                    );
                }
            }
        }
    }
    info
}

/// The entitlement keys an iOS app is most often asked about, from entitlements as JSON.
pub fn entitlement_facts(ents: &Json, max_rows: usize) -> EntitlementFacts {
    let get_s = |k: &str| ents.get(k).and_then(Json::as_str).map(str::to_string);
    let list = |k: &str| {
        let mut c = Capped::default();
        match ents.get(k) {
            Some(Json::Array(a)) => a.iter().filter_map(Json::as_str).for_each(|x| c.push(x.to_string(), max_rows)),
            Some(Json::String(x)) => c.push(x.clone(), max_rows),
            _ => {}
        }
        c
    };
    EntitlementFacts {
        keys: ents.as_object().map(serde_json::Map::len).unwrap_or(0),
        application_identifier: get_s("application-identifier"),
        team_identifier: get_s("com.apple.developer.team-identifier"),
        keychain_groups: list("keychain-access-groups"),
        push_environment: get_s("aps-environment"),
        associated_domains: list("com.apple.developer.associated-domains"),
        app_groups: list("com.apple.security.application-groups"),
        get_task_allow: ents.get("get-task-allow").and_then(Json::as_bool),
    }
}

const MAX_SLICES: usize = 16;

fn library_kind(cmd: &str) -> &'static str {
    match cmd {
        "LC_LOAD_WEAK_DYLIB" | "weak" => "weak",
        "LC_REEXPORT_DYLIB" | "reexport" => "reexport",
        "LC_LAZY_LOAD_DYLIB" | "lazy" => "lazy",
        "LC_LOAD_UPWARD_DYLIB" | "upward" => "upward",
        _ => "load",
    }
}

/// Where a linked library's install name says it lives.
pub fn library_origin(name: &str) -> &'static str {
    if name.starts_with("/System/") || name.starts_with("/usr/lib/") {
        "system"
    } else if name.starts_with("@rpath/") || name.starts_with("@executable_path/") || name.starts_with("@loader_path/") {
        "embedded"
    } else {
        "other"
    }
}

fn executable_facts(path: &str, data: &[u8], limits: &Limits) -> Result<Executable, String> {
    let all = macho_deps::slices(data).map_err(|e| format!("{path} isn't a readable Mach-O: {e}"))?;
    let mut x = Executable { path: path.to_string(), bytes: data.len() as u64, universal: macho_deps::is_fat(data), ..Executable::default() };
    let picked = macho_deps::pick_slice(&all, None);
    for sl in all.iter().take(MAX_SLICES) {
        let mut f = SliceFacts {
            arch: sl.arch.clone(),
            offset: sl.offset,
            size: sl.size,
            has_encryption_info: false,
            cryptid: None,
            crypt_size: None,
            platform: None,
            minimum_os: None,
            sdk: None,
            error: None,
        };
        let Some(bytes) = macho_deps::slice_bytes(data, sl) else {
            f.error = Some("the slice runs past the end of the file".into());
            x.slices.push(f);
            continue;
        };
        match macho_deps::parse_thin(bytes) {
            Ok(d) => {
                if let Some(enc) = &d.encryption {
                    f.has_encryption_info = true;
                    f.cryptid = Some(enc.cryptid);
                    f.crypt_size = Some(enc.cryptsize);
                    if enc.cryptid != 0 {
                        x.encrypted = true;
                    }
                }
                if let Some(bv) = d.build_versions.first() {
                    f.platform = Some(bv.platform.clone());
                    f.minimum_os = Some(bv.minos.clone());
                    f.sdk = bv.sdk.clone();
                }
                if picked.as_ref().is_some_and(|p| p.offset == sl.offset && p.arch == sl.arch) {
                    x.read_slice = Some(sl.arch.clone());
                    for lib in &d.dylibs {
                        x.libraries.push(Library { name: lib.name.clone(), kind: library_kind(lib.kind), origin: library_origin(&lib.name) }, limits.max_rows);
                    }
                    match d.code_signature {
                        Some((off, size)) => {
                            let start = off as usize;
                            match bytes.get(start..start.saturating_add(size as usize)) {
                                Some(blob) => match codesign::parse(blob) {
                                    Ok(sig) => x.signature = Some(signature_facts(&sig, limits)),
                                    Err(e) => x.signature_error = Some(format!("code signature: {e}")),
                                },
                                None => x.signature_error = Some("the code signature runs past the end of the slice".into()),
                            }
                        }
                        None => x.signature_error = Some("the executable has no code signature".into()),
                    }
                }
            }
            Err(e) => f.error = Some(e.to_string()),
        }
        x.slices.push(f);
    }
    if all.len() > MAX_SLICES {
        x.signature_error.get_or_insert_with(|| format!("only the first {MAX_SLICES} of {} slices were read", all.len()));
    }
    Ok(x)
}

fn signature_facts(sig: &codesign::Signature, limits: &Limits) -> SignatureFacts {
    let cd = sig.code_directories.first();
    SignatureFacts {
        identifier: cd.and_then(|c| c.identifier.clone()),
        team_id: cd.and_then(|c| c.team_id.clone()),
        ad_hoc: sig.ad_hoc,
        cms_present: sig.cms_signature_bytes.is_some(),
        entitlement_facts: sig.entitlements.as_ref().map(|e| entitlement_facts(e, limits.max_rows)),
        entitlements: sig.entitlements.clone(),
        der_entitlements_bytes: sig.der_entitlements_bytes,
        warnings: sig.warnings.clone(),
    }
}

fn date_text(v: Option<&plist::Value>) -> Option<String> {
    match v? {
        plist::Value::Date(plist::DateValue::Seconds(s)) => Some(plist::apple_date_iso(*s)),
        plist::Value::Date(plist::DateValue::Text(t)) => Some(t.clone()),
        plist::Value::String(t) => Some(t.clone()),
        _ => None,
    }
}

/// The XML property list inside a profile envelope, found by its markers.
fn locate_plist(data: &[u8]) -> Option<&[u8]> {
    let start = crate::find_bytes(data, b"<?xml", data.len()).or_else(|| crate::find_bytes(data, b"<plist", data.len()))?;
    let rest = data.get(start..)?;
    let end = crate::find_bytes(rest, b"</plist>", rest.len())?.checked_add(b"</plist>".len())?;
    rest.get(..end)
}

const MAX_PROFILE_CERTS: usize = 16;

fn profile_facts(data: &[u8], limits: &Limits) -> Result<Profile, String> {
    let cms = pkcs7::parse_signed_data(data);
    let (payload, read_via): (Vec<u8>, &'static str) = match cms.as_ref().and_then(|c| c.econtent.clone()) {
        Some(c) if plist::looks_like_plist(&c) => (c, "cms_content"),
        _ => match locate_plist(data) {
            Some(p) => (p.to_vec(), "located_plist"),
            None => return Err("embedded.mobileprovision: no property list was found inside the envelope".into()),
        },
    };
    let v = plist::parse(&payload).map_err(|e| format!("embedded.mobileprovision property list: {e}"))?;
    let devices = v.get("ProvisionedDevices").and_then(plist::Value::as_array).map(<[plist::Value]>::len);
    let all = b(&v, "ProvisionsAllDevices").unwrap_or(false);
    let ents = v.get("Entitlements").map(plist::Value::to_json);
    let get_task_allow = ents.as_ref().and_then(|e| e.get("get-task-allow")).and_then(Json::as_bool).unwrap_or(false);
    let kind = if all {
        "enterprise"
    } else if devices.is_some() {
        if get_task_allow { "development" } else { "ad_hoc" }
    } else {
        "app_store"
    };
    let mut certificates = Capped::default();
    for c in v.get("DeveloperCertificates").and_then(plist::Value::as_array).unwrap_or(&[]) {
        if let plist::Value::Data(d) = c {
            let info = der::certificate(d);
            certificates.push(
                ProfileCertificate { subject: info.as_ref().map(|i| i.subject.clone()), not_after: info.as_ref().map(|i| i.not_after.clone()), bytes: d.len() },
                MAX_PROFILE_CERTS,
            );
        }
    }
    Ok(Profile {
        bytes: data.len(),
        read_via,
        signature_verified: false,
        cms_certificates: cms.as_ref().map(|c| c.certificates.len()).unwrap_or(0),
        name: s(&v, "Name"),
        app_id_name: s(&v, "AppIDName"),
        uuid: s(&v, "UUID"),
        team_ids: strings(v.get("TeamIdentifier")).take(16).map(str::to_string).collect(),
        team_name: s(&v, "TeamName"),
        platforms: strings(v.get("Platform")).take(16).map(str::to_string).collect(),
        creation_date: date_text(v.get("CreationDate")),
        expiration_date: date_text(v.get("ExpirationDate")),
        provisioned_devices: devices,
        provisions_all_devices: all,
        kind,
        entitlement_facts: ents.as_ref().map(|e| entitlement_facts(e, limits.max_rows)),
        entitlements: ents,
        certificates,
    })
}

fn bundle_info(reader: &mut Reader<'_>, b: &mut Bundle, limits: &Limits) {
    match reader.read(&format!("{}/Info.plist", b.path), limits.max_plist_bytes) {
        Ok(Some(bytes)) => match plist::parse(&bytes) {
            Ok(v) => {
                b.bundle_id = s(&v, "CFBundleIdentifier");
                b.version = s(&v, "CFBundleShortVersionString").or_else(|| s(&v, "CFBundleVersion"));
                b.executable = s(&v, "CFBundleExecutable");
                b.extension_point = v
                    .get("NSExtension")
                    .and_then(|x| s(x, "NSExtensionPointIdentifier"))
                    .or_else(|| v.get("EXAppExtensionAttributes").and_then(|x| s(x, "EXExtensionPointIdentifier")));
            }
            Err(e) => b.info_error = Some(format!("Info.plist: {e}")),
        },
        Ok(None) => b.info_error = Some("no Info.plist".into()),
        Err(e) => b.info_error = Some(format!("Info.plist: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_testkit::apple;
    use craft_anything_testkit::ios::{self, IpaSpec};

    fn read(spec: &IpaSpec) -> Ipa {
        parse(&ios::ipa(spec), &Limits::default()).unwrap()
    }

    #[test]
    fn a_well_formed_app_is_read_as_one_app() {
        let ipa = read(&IpaSpec::default());
        assert_eq!((ipa.apps.total, ipa.app.as_deref()), (1, Some("Payload/Notes.app")));
        let info = ipa.info.as_ref().unwrap();
        assert_eq!(info.encoding, "xml");
        assert_eq!(info.bundle_id.as_deref(), Some("com.example.notes"));
        assert_eq!((info.version.as_deref(), info.build.as_deref()), (Some("2.1"), Some("42")));
        assert_eq!(info.minimum_os.as_deref(), Some("15.0"));
        assert_eq!(info.device_families, vec!["iphone", "ipad"]);
        assert_eq!(info.required_capabilities.items, vec!["arm64"]);
        assert_eq!(info.url_schemes.items, vec!["notes", "com.example.notes"]);
        assert_eq!(info.usage_descriptions.total, 2);
        assert_eq!(info.background_modes.items, vec!["remote-notification", "fetch"]);
        let ats = info.ats.as_ref().unwrap();
        assert_eq!(ats.allows_arbitrary_loads, Some(false));
        assert_eq!(ats.exception_domains.items[0].domain, "legacy.example.com");
        assert_eq!(ats.exception_domains.items[0].allows_insecure_http, Some(true));
        assert_eq!(info.scenes.items[0].delegate_class.as_deref(), Some("Notes.SceneDelegate"));
        assert_eq!(info.launch_storyboard.as_deref(), Some("LaunchScreen"));

        let x = ipa.executable.as_ref().unwrap();
        assert_eq!(x.read_slice.as_deref(), Some("arm64"));
        assert!(!x.encrypted);
        assert_eq!(x.slices[0].cryptid, Some(0));
        assert_eq!(x.slices[0].minimum_os.as_deref(), Some("15.0.0"));
        assert_eq!(x.libraries.total, 6);
        assert_eq!(x.libraries.items.iter().filter(|l| l.origin == "embedded").count(), 2);
        assert_eq!(x.libraries.items.iter().find(|l| l.name.contains("App.framework")).unwrap().kind, "weak");
        let sig = x.signature.as_ref().unwrap();
        assert_eq!(sig.team_id.as_deref(), Some("ABCDE12345"));
        let ef = sig.entitlement_facts.as_ref().unwrap();
        assert_eq!(ef.application_identifier.as_deref(), Some("ABCDE12345.com.example.notes"));
        assert_eq!(ef.team_identifier.as_deref(), Some("ABCDE12345"));
        assert_eq!(ef.keychain_groups.total, 2);
        assert_eq!(ef.push_environment.as_deref(), Some("production"));
        assert_eq!(ef.associated_domains.items, vec!["applinks:notes.example.com"]);
        assert_eq!(ef.app_groups.items, vec!["group.com.example.notes"]);
        assert_eq!(ef.get_task_allow, Some(false));

        let p = ipa.profile.as_ref().unwrap();
        assert_eq!(p.read_via, "cms_content");
        assert!(!p.signature_verified);
        assert_eq!(p.cms_certificates, 1);
        assert_eq!(p.name.as_deref(), Some("Notes Distribution"));
        assert_eq!(p.team_ids, vec!["ABCDE12345"]);
        assert_eq!(p.kind, "app_store");
        // The fixture writer puts one fixed date in XML.
        assert_eq!(p.expiration_date.as_deref(), Some("2001-01-02T00:00:00Z"));
        assert_eq!(p.certificates.total, 1);
        assert!(p.certificates.items[0].subject.as_deref().unwrap().contains("Example Distribution"));
        assert_eq!(p.entitlement_facts.as_ref().unwrap().push_environment.as_deref(), Some("production"));

        let fw: Vec<(&str, &str)> = ipa.frameworks.items.iter().map(|b| (b.name.as_str(), b.kind)).collect();
        assert_eq!(fw, vec![("App.framework", "framework"), ("Flutter.framework", "framework"), ("libswiftCore.dylib", "dylib")]);
        assert_eq!(ipa.frameworks.items[1].bundle_id.as_deref(), Some("io.flutter.flutter"));
        assert_eq!(ipa.extensions.total, 1);
        assert_eq!(ipa.extensions.items[0].extension_point.as_deref(), Some("com.apple.share-services"));
        assert_eq!(ipa.watch_apps.items[0].bundle_id.as_deref(), Some("com.example.notes.watchkitapp"));
        assert_eq!(ipa.extras.swift_support.total, 1);
        assert_eq!(ipa.extras.symbols_files, 1);
        assert!(ipa.extras.itunes_metadata);
        assert!(ipa.info_error.is_none() && ipa.executable_error.is_none() && ipa.profile_error.is_none());
        assert!(!ipa.budget_exhausted);
    }

    #[test]
    fn a_binary_info_plist_reads_the_same() {
        let bin = read(&IpaSpec { binary_info: true, ..IpaSpec::default() });
        let xml = read(&IpaSpec::default());
        let (b, x) = (bin.info.unwrap(), xml.info.unwrap());
        assert_eq!(b.encoding, "binary");
        assert_eq!(Info { encoding: "xml", ..b }, x);
    }

    #[test]
    fn an_encrypted_executable_is_said_to_be() {
        let ipa = read(&IpaSpec { cryptid: 1, ..IpaSpec::default() });
        let x = ipa.executable.unwrap();
        assert!(x.encrypted);
        assert_eq!((x.slices[0].cryptid, x.slices[0].crypt_size), (Some(1), Some(0x1000)));
    }

    #[test]
    fn no_payload_and_two_apps() {
        let zip = ios::zip_of(&[("Notes.app/Info.plist".into(), b"x".to_vec()), ("readme.txt".into(), b"hi".to_vec())]);
        let ipa = parse(&zip, &Limits::default()).unwrap();
        assert_eq!((ipa.apps.total, ipa.app.is_none()), (0, true));
        assert_eq!(ipa.extras.other_top_level.items, vec!["Notes.app", "readme.txt"]);
        let two = read(&IpaSpec { second_app: true, ..IpaSpec::default() });
        assert_eq!(two.apps.items, vec!["Payload/Notes.app", "Payload/Other.app"]);
        assert_eq!(two.app.as_deref(), Some("Payload/Notes.app"));
        assert!(two.warnings.iter().any(|w| w.contains("2 app folders")));
    }

    #[test]
    fn caps_keep_totals_and_say_so() {
        let data = ios::ipa(&IpaSpec { extra_frameworks: 30, ..IpaSpec::default() });
        let limits = Limits { max_rows: 5, max_bundles: 10, ..Limits::default() };
        let ipa = parse(&data, &limits).unwrap();
        assert_eq!(ipa.frameworks.total, 33);
        assert_eq!(ipa.frameworks.items.len(), 5);
        assert!(ipa.frameworks.rows_capped);
        // 32 framework folders (the dylib has no Info.plist), an extension and a watch app
        // compete for 10 reads.
        assert_eq!(ipa.bundles_not_read, 32 + 2 - 10);
        assert!(!ipa.extensions.rows_capped);
    }

    #[test]
    fn an_oversized_plist_and_a_spent_budget_are_errors_not_reads() {
        let data = ios::ipa(&IpaSpec::default());
        let small = Limits { max_plist_bytes: 64, ..Limits::default() };
        let ipa = parse(&data, &small).unwrap();
        assert!(ipa.info.is_none());
        assert!(ipa.info_error.as_deref().unwrap().contains("over the 64-byte limit"), "{:?}", ipa.info_error);
        assert!(ipa.frameworks.items.iter().filter(|b| b.kind != "dylib").all(|b| b.info_error.is_some()));
        // The executable is still found by the folder's name.
        assert!(ipa.executable.is_some());
        let tight = Limits { max_total_bytes: 512, ..Limits::default() };
        let spent = parse(&data, &tight).unwrap();
        assert!(spent.budget_exhausted);
        assert!(spent.executable_error.as_deref().unwrap().contains("budget"), "{:?}", spent.executable_error);
    }

    #[test]
    fn a_profile_outside_readable_cms_is_found_by_its_markers() {
        let mut files = ios::ipa_files(&IpaSpec { profile: false, ..IpaSpec::default() });
        files.push(("Payload/Notes.app/embedded.mobileprovision".into(), ios::mobileprovision_unreadable_envelope(&ios::profile_value(Some(3), false))));
        let p = parse(&ios::zip_of(&files), &Limits::default()).unwrap().profile.unwrap();
        assert_eq!((p.read_via, p.provisioned_devices, p.kind), ("located_plist", Some(3), "ad_hoc"));
        let ent = ios::mobileprovision(&ios::profile_value(None, true));
        assert_eq!(profile_facts(&ent, &Limits::default()).unwrap().kind, "enterprise");
        assert!(profile_facts(b"\x30\x80 nothing here", &Limits::default()).is_err());
    }

    #[test]
    fn hostile_archives_never_panic() {
        let good = ios::ipa(&IpaSpec::default());
        assert!(parse(good.get(..good.len() / 2).unwrap(), &Limits::default()).is_err());
        assert!(parse(b"PK\x05\x06", &Limits::default()).is_err());
        for m in craft_anything_testkit::mutations(&good) {
            let _ = parse(&m, &Limits::default());
        }
        for seed in 0..32 {
            let _ = parse(&craft_anything_testkit::noise(seed, 4096), &Limits::default());
        }
        // A traversal-shaped executable name is refused, not followed.
        let plist = apple::to_xml(&apple::dict(vec![("CFBundleExecutable", apple::s("../../etc/passwd"))])).into_bytes();
        let evil = ios::zip_of(&[("Payload/X.app/Info.plist".into(), plist)]);
        let ipa = parse(&evil, &Limits::default()).unwrap();
        assert!(ipa.executable.is_none());
        assert!(ipa.executable_error.unwrap().contains("plain file name"));
        // Hostile executables and profiles.
        for m in craft_anything_testkit::mutations(&ios::ios_macho(0)) {
            let _ = executable_facts("x", &m, &Limits::default());
        }
        for m in craft_anything_testkit::mutations(&ios::mobileprovision(&ios::profile_value(Some(2), false))) {
            let _ = profile_facts(&m, &Limits::default());
        }
    }
}
