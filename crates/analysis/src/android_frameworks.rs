//! Which frameworks an Android app is built with, from the files in its package and the class
//! names its DEX files define or use. Each finding carries the signals that support it and a
//! strength; the signals are a typed table ([`SIGNALS`]), not code.
//!
//! What this can't see: Kotlin Multiplatform's shared code compiles to ordinary JVM classes and
//! leaves no marker of its own (Compose Multiplatform does); a framework whose files were renamed
//! or whose classes were obfuscated past their package names.

use serde::Serialize;

/// How much one signal says on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    /// Present in many apps that don't use the framework for their screens.
    Weak,
    /// Usually means the framework, but a library could bring it in.
    Medium,
    /// Only an app built with the framework has it.
    Strong,
}

/// What a framework is to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Draws the app's screens and runs on several platforms.
    CrossPlatform,
    /// Android's own way of drawing screens.
    NativeUi,
    /// Shows web content inside the app.
    Web,
    /// The language the app's code is written in.
    Language,
}

/// Where a signal is looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A file in the package whose path contains this (lower case).
    File(&'static str),
    /// A resource type the resource table defines entries for (`layout`), whatever the files are
    /// named (release builds often shorten `res/` paths).
    ResourceType(&'static str),
    /// A class name in the DEX files (a descriptor such as `Lio/flutter/`) starting with this.
    Class(&'static str),
}

/// One row of the table.
#[derive(Debug, Clone, Copy)]
pub struct Signal {
    pub framework: &'static str,
    pub place: Place,
    pub strength: Strength,
}

/// The frameworks known, with their display names and roles.
pub const FRAMEWORKS: &[(&str, &str, Role)] = &[
    ("flutter", "Flutter", Role::CrossPlatform),
    ("react-native", "React Native", Role::CrossPlatform),
    ("dotnet", ".NET for Android (MAUI or Xamarin)", Role::CrossPlatform),
    ("unity", "Unity", Role::CrossPlatform),
    ("godot", "Godot", Role::CrossPlatform),
    ("compose-multiplatform", "Compose Multiplatform", Role::CrossPlatform),
    ("cordova", "Apache Cordova", Role::Web),
    ("capacitor", "Capacitor", Role::Web),
    ("webview", "Web pages in a WebView", Role::Web),
    ("jetpack-compose", "Jetpack Compose", Role::NativeUi),
    ("android-views", "Android Views (XML layouts)", Role::NativeUi),
    ("kotlin", "Kotlin", Role::Language),
];

const fn s(framework: &'static str, place: Place, strength: Strength) -> Signal {
    Signal { framework, place, strength }
}
use Place::{Class, File, ResourceType};
use Strength::{Medium, Strong, Weak};

/// What says which framework.
pub const SIGNALS: &[Signal] = &[
    s("flutter", File("libflutter.so"), Strong),
    s("flutter", File("flutter_assets/"), Strong),
    s("flutter", File("/libapp.so"), Medium),
    s("flutter", Class("Lio/flutter/embedding/"), Strong),
    s("react-native", File("libreactnativejni.so"), Strong),
    s("react-native", File("index.android.bundle"), Strong),
    s("react-native", File("libhermes"), Medium),
    s("react-native", Class("Lcom/facebook/react/"), Strong),
    s("dotnet", File("libmonodroid.so"), Strong),
    s("dotnet", File("libmonosgen"), Strong),
    s("dotnet", File("libassemblies."), Strong),
    s("dotnet", File("assemblies/"), Medium),
    s("dotnet", Class("Lmono/android/"), Strong),
    s("dotnet", Class("Lmicrosoft/maui/"), Strong),
    s("unity", File("libunity.so"), Strong),
    s("unity", File("libil2cpp.so"), Medium),
    s("unity", Class("Lcom/unity3d/player/"), Strong),
    s("godot", File("libgodot_android.so"), Strong),
    s("godot", Class("Lorg/godotengine/godot/"), Strong),
    s("compose-multiplatform", Class("Lorg/jetbrains/compose/"), Medium),
    s("compose-multiplatform", File("composeresources/"), Medium),
    s("cordova", Class("Lorg/apache/cordova/"), Strong),
    s("cordova", File("assets/www/cordova.js"), Strong),
    s("capacitor", Class("Lcom/getcapacitor/"), Strong),
    s("capacitor", File("capacitor.config.json"), Strong),
    s("webview", Class("Landroid/webkit/WebView;"), Weak),
    s("webview", File("assets/www/index.html"), Medium),
    s("webview", File("assets/public/index.html"), Medium),
    s("jetpack-compose", Class("Landroidx/compose/ui/platform/ComposeView;"), Strong),
    s("jetpack-compose", Class("Landroidx/activity/compose/"), Strong),
    s("jetpack-compose", Class("Landroidx/compose/runtime/"), Medium),
    s("android-views", File("res/layout/"), Medium),
    s("android-views", ResourceType("layout"), Medium),
    s("android-views", Class("Landroidx/appcompat/app/AppCompatActivity;"), Weak),
    s("android-views", Class("Landroidx/fragment/app/Fragment;"), Weak),
    s("kotlin", Class("Lkotlin/Metadata;"), Strong),
    s("kotlin", Class("Lkotlin/jvm/internal/Intrinsics;"), Strong),
];

/// Most examples kept per signal.
const MAX_EXAMPLES: usize = 3;

/// One framework found, with what says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub framework: &'static str,
    pub name: &'static str,
    pub role: Role,
    /// The strongest signal found.
    pub strength: Strength,
    /// Each signal that matched: what was looked for and a few of the files or classes that matched.
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// `file` or `class`.
    pub kind: &'static str,
    pub pattern: &'static str,
    pub strength: Strength,
    pub matches: usize,
    pub examples: Vec<String>,
}

/// What an app is built with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Frameworks {
    pub found: Vec<Finding>,
    /// The framework that most likely draws the screens: a cross-platform or web one with a
    /// strong signal, else Compose with a strong signal, else Android Views when any layout is
    /// there.
    pub primary: Option<&'static str>,
}

/// Read the frameworks from a package's file paths and its DEX class names (type descriptors).
pub fn detect<'a>(files: impl Iterator<Item = &'a str> + Clone, classes: impl Iterator<Item = &'a str> + Clone, resource_types: &[&str]) -> Frameworks {
    let mut found: Vec<Finding> = Vec::new();
    for &(id, name, role) in FRAMEWORKS {
        let mut evidence = Vec::new();
        for sig in SIGNALS.iter().filter(|s| s.framework == id) {
            let (kind, pattern, hits): (&str, &str, Vec<&str>) = match sig.place {
                File(p) => ("file", p, files.clone().filter(|f| f.to_ascii_lowercase().contains(p)).collect()),
                Class(p) => ("class", p, classes.clone().filter(|c| c.starts_with(p)).collect()),
                ResourceType(t) => ("resource_type", t, resource_types.iter().copied().filter(|r| *r == t).collect()),
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
        .or_else(|| found.iter().find(|f| f.framework == "android-views").map(|f| f.framework));
    Frameworks { found, primary }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(files: &[&str], classes: &[&str]) -> Frameworks {
        detect(files.iter().copied(), classes.iter().copied(), &[])
    }

    #[test]
    fn every_signal_names_a_known_framework() {
        for s in SIGNALS {
            assert!(FRAMEWORKS.iter().any(|f| f.0 == s.framework), "{}", s.framework);
            if let File(p) = s.place {
                assert_eq!(p, p.to_ascii_lowercase(), "file patterns are matched on lower case");
            }
        }
    }

    #[test]
    fn a_flutter_app_is_flutter_though_it_has_an_activity_and_layouts() {
        let f = run(
            &["lib/arm64-v8a/libflutter.so", "lib/arm64-v8a/libapp.so", "res/layout/x.xml"],
            &["Lio/flutter/embedding/android/FlutterActivity;", "Lkotlin/Metadata;"],
        );
        assert_eq!(f.primary, Some("flutter"));
        let ids: Vec<&str> = f.found.iter().map(|x| x.framework).collect();
        assert_eq!(ids, vec!["flutter", "android-views", "kotlin"]);
        assert_eq!(f.found[0].strength, Strong);
        assert_eq!(f.found[0].evidence.len(), 3);
    }

    #[test]
    fn compose_needs_a_strong_sign_to_be_the_primary_one() {
        let only_runtime = run(&["res/layout/a.xml"], &["Landroidx/compose/runtime/Composer;"]);
        assert_eq!(only_runtime.primary, Some("android-views"));
        let compose = run(&["res/layout/a.xml"], &["Landroidx/activity/compose/ComponentActivityKt;"]);
        assert_eq!(compose.primary, Some("jetpack-compose"));
        // Layouts known from the resource table when the files were renamed.
        let shortened = detect(["res/a1.xml"].into_iter(), std::iter::empty(), &["layout", "string"]);
        assert_eq!(shortened.primary, Some("android-views"));
        assert_eq!(shortened.found[0].evidence[0].kind, "resource_type");
    }

    #[test]
    fn a_web_shell_and_nothing_at_all() {
        let cap = run(&["assets/capacitor.config.json", "assets/public/index.html"], &["Lcom/getcapacitor/BridgeActivity;", "Landroid/webkit/WebView;"]);
        assert_eq!(cap.primary, Some("capacitor"));
        assert!(cap.found.iter().any(|f| f.framework == "webview" && f.strength == Medium));
        let none = run(&["classes.dex"], &["Lcom/example/Main;"]);
        assert_eq!((none.primary, none.found.len()), (None, 0));
    }

    #[test]
    fn examples_are_capped() {
        let files: Vec<String> = (0..50).map(|i| format!("res/layout/l{i}.xml")).collect();
        let f = detect(files.iter().map(String::as_str), std::iter::empty(), &[]);
        assert_eq!((f.found[0].evidence[0].matches, f.found[0].evidence[0].examples.len()), (50, MAX_EXAMPLES));
    }
}
