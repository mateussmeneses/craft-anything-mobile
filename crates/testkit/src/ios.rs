//! Synthetic iOS app archives (`.ipa`): a `Payload/Notes.app` with an Info.plist (XML or binary),
//! a thin arm64 executable with an encryption load command and a code signature carrying iOS
//! entitlements, an `embedded.mobileprovision` (a CMS envelope around a property list), embedded
//! frameworks, an app extension, a watch app and the archive's extras. Nothing here comes from a
//! real app.

use crate::android::{certificate_der, der, der_oid};
use crate::apple::{P, assemble_superblob, code_directory_blob, dict, lc_dylib, s, to_bplist, to_xml};

/// What [`ipa`] puts in the archive.
#[derive(Debug, Clone)]
pub struct IpaSpec {
    /// Write the app's Info.plist as a binary plist (else XML).
    pub binary_info: bool,
    /// The executable's `cryptid` (non-zero: encrypted, as an App Store download is).
    pub cryptid: u32,
    pub profile: bool,
    /// Extra empty frameworks (`Frameworks/ExtraN.framework`) beyond Flutter's two.
    pub extra_frameworks: usize,
    /// A second `Payload/Other.app`.
    pub second_app: bool,
}

impl Default for IpaSpec {
    fn default() -> Self {
        IpaSpec { binary_info: false, cryptid: 0, profile: true, extra_frameworks: 0, second_app: false }
    }
}

/// The app's Info.plist as a value.
pub fn info_value() -> P {
    dict(vec![
        ("CFBundleIdentifier", s("com.example.notes")),
        ("CFBundleShortVersionString", s("2.1")),
        ("CFBundleVersion", s("42")),
        ("CFBundleDisplayName", s("Notes")),
        ("CFBundleName", s("Notes")),
        ("CFBundleExecutable", s("Notes")),
        ("MinimumOSVersion", s("15.0")),
        ("CFBundleSupportedPlatforms", P::Array(vec![s("iPhoneOS")])),
        ("DTSDKName", s("iphoneos17.0")),
        ("DTXcode", s("1500")),
        ("UIDeviceFamily", P::Array(vec![P::Int(1), P::Int(2)])),
        ("UIRequiredDeviceCapabilities", P::Array(vec![s("arm64")])),
        ("CFBundleURLTypes", P::Array(vec![dict(vec![("CFBundleURLSchemes", P::Array(vec![s("notes"), s("com.example.notes")]))])])),
        ("LSApplicationQueriesSchemes", P::Array(vec![s("maps")])),
        ("NSCameraUsageDescription", s("Scan paper notes.")),
        ("NSLocationWhenInUseUsageDescription", s("Tag notes with a place.")),
        (
            "NSAppTransportSecurity",
            dict(vec![
                ("NSAllowsArbitraryLoads", P::Bool(false)),
                (
                    "NSExceptionDomains",
                    dict(vec![(
                        "legacy.example.com",
                        dict(vec![("NSExceptionAllowsInsecureHTTPLoads", P::Bool(true)), ("NSIncludesSubdomains", P::Bool(true))]),
                    )]),
                ),
            ]),
        ),
        ("UIBackgroundModes", P::Array(vec![s("remote-notification"), s("fetch")])),
        ("UILaunchStoryboardName", s("LaunchScreen")),
        (
            "UIApplicationSceneManifest",
            dict(vec![
                ("UIApplicationSupportsMultipleScenes", P::Bool(false)),
                (
                    "UISceneConfigurations",
                    dict(vec![(
                        "UIWindowSceneSessionRoleApplication",
                        P::Array(vec![dict(vec![
                            ("UISceneConfigurationName", s("Default Configuration")),
                            ("UISceneDelegateClassName", s("Notes.SceneDelegate")),
                            ("UISceneStoryboardFile", s("Main")),
                        ])]),
                    )]),
                ),
            ]),
        ),
    ])
}

/// The iOS entitlements both the executable and the profile carry.
pub fn entitlements_value() -> P {
    dict(vec![
        ("application-identifier", s("ABCDE12345.com.example.notes")),
        ("com.apple.developer.team-identifier", s("ABCDE12345")),
        ("keychain-access-groups", P::Array(vec![s("ABCDE12345.com.example.notes"), s("ABCDE12345.shared")])),
        ("aps-environment", s("production")),
        ("com.apple.developer.associated-domains", P::Array(vec![s("applinks:notes.example.com")])),
        ("com.apple.security.application-groups", P::Array(vec![s("group.com.example.notes")])),
        ("get-task-allow", P::Bool(false)),
    ])
}

fn be(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn le_words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// A thin arm64 iOS executable: UIKit, Foundation, the Objective-C and Swift runtimes, `@rpath`
/// Flutter and App frameworks, an iOS build version, `LC_ENCRYPTION_INFO_64` with `cryptid`, and
/// a code signature (code directory plus XML entitlements from [`entitlements_value`]).
pub fn ios_macho(cryptid: u32) -> Vec<u8> {
    let xml = to_xml(&entitlements_value());
    let mut ents = Vec::new();
    be(&mut ents, 0xfade_7171);
    be(&mut ents, (8 + xml.len()) as u32);
    ents.extend_from_slice(xml.as_bytes());
    let sig = assemble_superblob(&[(0, code_directory_blob()), (5, ents)]);
    let mut cmds: Vec<Vec<u8>> = vec![
        lc_dylib(0xc, "/System/Library/Frameworks/UIKit.framework/UIKit"),
        lc_dylib(0xc, "/System/Library/Frameworks/Foundation.framework/Foundation"),
        lc_dylib(0xc, "/usr/lib/libobjc.A.dylib"),
        lc_dylib(0xc, "/usr/lib/swift/libswiftCore.dylib"),
        lc_dylib(0xc, "@rpath/Flutter.framework/Flutter"),
        lc_dylib(0x8000_0018, "@rpath/App.framework/App"),
        // LC_BUILD_VERSION: iOS (2), minimum 15.0, SDK 17.0, no tools.
        le_words(&[0x32, 24, 2, 0x000f_0000, 0x0011_0000, 0]),
        // LC_ENCRYPTION_INFO_64.
        le_words(&[0x2c, 24, 0x4000, 0x1000, cryptid, 0]),
    ];
    let cmds_len: usize = cmds.iter().map(Vec::len).sum::<usize>() + 16;
    let sig_off = (32 + cmds_len + 15) & !15;
    cmds.push(le_words(&[0x1d, 16, sig_off as u32, sig.len() as u32]));
    let mut m = le_words(&[0xfeed_facf, 0x0100_000c, 0, 2, cmds.len() as u32, cmds_len as u32, 0, 0]);
    for c in &cmds {
        m.extend_from_slice(c);
    }
    m.resize(sig_off, 0);
    m.extend_from_slice(&sig);
    m
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

fn set(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x31, &parts.concat())
}

/// The property list a provisioning profile carries. `devices`: `ProvisionedDevices` entries
/// (`None` leaves the key out); `all_devices`: `ProvisionsAllDevices`.
pub fn profile_value(devices: Option<usize>, all_devices: bool) -> P {
    let mut items = vec![
        ("Name", s("Notes Distribution")),
        ("AppIDName", s("Notes")),
        ("UUID", s("00000000-1111-2222-3333-444444444444")),
        ("TeamIdentifier", P::Array(vec![s("ABCDE12345")])),
        ("TeamName", s("Example Team")),
        ("Platform", P::Array(vec![s("iOS")])),
        ("CreationDate", P::Date(86_400.0)),
        ("ExpirationDate", P::Date(86_400.0)),
        ("Entitlements", entitlements_value()),
        ("DeveloperCertificates", P::Array(vec![P::Data(certificate_der("Example Distribution", "Example Team"))])),
    ];
    if let Some(n) = devices {
        items.push(("ProvisionedDevices", P::Array((0..n).map(|i| s(&format!("00008030-{i:016X}"))).collect())));
    }
    if all_devices {
        items.push(("ProvisionsAllDevices", P::Bool(true)));
    }
    dict(items)
}

/// `embedded.mobileprovision`: CMS SignedData whose encapsulated content is the XML of `plist`,
/// with one certificate and a signer whose signature bytes are fake.
pub fn mobileprovision(plist: &P) -> Vec<u8> {
    let content = to_xml(plist);
    let cert = certificate_der("Example CA", "Example Team");
    let sha256 = seq(&[der_oid("2.16.840.1.101.3.4.2.1")]);
    let rsa = seq(&[der_oid("1.2.840.113549.1.1.1"), der(0x05, &[])]);
    let issuer = seq(&[set(&[seq(&[der_oid("2.5.4.3"), der(0x0c, b"Example CA")])])]);
    let signer = seq(&[der(0x02, &[1]), seq(&[issuer, der(0x02, &[1, 2, 3, 4, 5])]), sha256.clone(), rsa, der(0x04, &[0x5a; 16])]);
    let sd = seq(&[
        der(0x02, &[1]),
        set(&[sha256]),
        seq(&[der_oid("1.2.840.113549.1.7.1"), der(0xa0, &der(0x04, content.as_bytes()))]),
        der(0xa0, &cert),
        set(&[signer]),
    ]);
    seq(&[der_oid("1.2.840.113549.1.7.2"), der(0xa0, &sd)])
}

/// A profile whose envelope isn't readable CMS: the property list sits between junk bytes.
pub fn mobileprovision_unreadable_envelope(plist: &P) -> Vec<u8> {
    let mut v = vec![0x30, 0x80, 0x06, 0x09, 0xff, 0xff];
    v.extend_from_slice(to_xml(plist).as_bytes());
    v.extend_from_slice(&[0, 0, 0xa0, 0x80]);
    v
}

fn bundle_plist(id: &str, exe: &str, extra: Vec<(&str, P)>) -> Vec<u8> {
    let mut items = vec![("CFBundleIdentifier", s(id)), ("CFBundleShortVersionString", s("1.0")), ("CFBundleExecutable", s(exe))];
    items.extend(extra);
    to_xml(&dict(items)).into_bytes()
}

/// The files of the archive [`ipa`] builds, as (name, bytes).
pub fn ipa_files(spec: &IpaSpec) -> Vec<(String, Vec<u8>)> {
    let app = "Payload/Notes.app";
    let info = if spec.binary_info { to_bplist(&info_value()) } else { to_xml(&info_value()).into_bytes() };
    let mut f: Vec<(String, Vec<u8>)> = vec![
        (format!("{app}/Info.plist"), info),
        (format!("{app}/Notes"), ios_macho(spec.cryptid)),
        (format!("{app}/Frameworks/Flutter.framework/Info.plist"), bundle_plist("io.flutter.flutter", "Flutter", vec![])),
        (format!("{app}/Frameworks/Flutter.framework/Flutter"), b"not code".to_vec()),
        (format!("{app}/Frameworks/App.framework/Info.plist"), bundle_plist("io.flutter.flutter.app", "App", vec![])),
        (format!("{app}/Frameworks/App.framework/App"), b"not code".to_vec()),
        (format!("{app}/Frameworks/App.framework/flutter_assets/AssetManifest.json"), b"{}".to_vec()),
        (format!("{app}/Frameworks/libswiftCore.dylib"), b"not code".to_vec()),
        (
            format!("{app}/PlugIns/Share.appex/Info.plist"),
            bundle_plist("com.example.notes.share", "Share", vec![("NSExtension", dict(vec![("NSExtensionPointIdentifier", s("com.apple.share-services"))]))]),
        ),
        (format!("{app}/PlugIns/Share.appex/Share"), b"not code".to_vec()),
        (format!("{app}/Watch/NotesWatch.app/Info.plist"), bundle_plist("com.example.notes.watchkitapp", "NotesWatch", vec![])),
        ("SwiftSupport/iphoneos/libswiftCore.dylib".into(), b"not code".to_vec()),
        ("Symbols/00112233-4455-6677-8899-AABBCCDDEEFF.symbols".into(), b"not symbols".to_vec()),
        ("iTunesMetadata.plist".into(), to_xml(&dict(vec![("itemName", s("Notes"))])).into_bytes()),
    ];
    if spec.profile {
        f.push((format!("{app}/embedded.mobileprovision"), mobileprovision(&profile_value(None, false))));
    }
    for i in 0..spec.extra_frameworks {
        f.push((format!("{app}/Frameworks/Extra{i:03}.framework/Info.plist"), bundle_plist(&format!("com.example.extra{i}"), "Extra", vec![])));
    }
    if spec.second_app {
        f.push(("Payload/Other.app/Info.plist".into(), bundle_plist("com.example.other", "Other", vec![])));
    }
    f
}

/// A stored ZIP of [`ipa_files`].
pub fn ipa(spec: &IpaSpec) -> Vec<u8> {
    zip_of(&ipa_files(spec))
}

/// A stored ZIP of owned (name, bytes) pairs.
pub fn zip_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    crate::electron::zip_stored(&refs)
}
