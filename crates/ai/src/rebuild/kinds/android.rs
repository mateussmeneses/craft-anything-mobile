//! Android apps: recognised from an Android package target (an APK, a split set or a bundle), an
//! Android stack, or a request that asks for one. Built with Gradle (or the stack's own build) into
//! a debug APK, and checked by building.

use super::*;

pub struct AndroidKind;

impl AndroidKind {
    pub const ID: &'static str = "android";
    /// The stack used when the request names none.
    pub const DEFAULT_STACK: &'static str = "Kotlin + Jetpack Compose";
    /// The stacks that build Android apps (named in `stacks.toml`).
    pub const STACKS: &'static [&'static str] =
        &["Kotlin + Jetpack Compose", "Kotlin + Android Views", "Flutter", "React Native", ".NET MAUI", "Kotlin Multiplatform"];
}

/// Endings of files that are an Android app.
const PACKAGE_ENDINGS: &[&str] = &[".apk", ".apks", ".xapk", ".aab"];

/// What the brief says a build must be, for every Android stack.
pub const ANDROID_BRIEF: &str = "The result is an Android app: build a debug APK with the stack's own build and keep it where that build puts it. Give it a package name (applicationId) of your own, never the original's, so both can be installed side by side. Ask only for the permissions the features you rebuild need. It must start on an emulator or phone without a development server, a network login or anything only you have.";

/// What the brief adds when the target is an Android package and Deep mode lets the builder read it.
pub const STUDY_PACKAGE: &str = "The original is an Android package. Before writing code, study it with Craft Anything's tools: android_bundle_show (for a split set or bundle: which part is which), android_package_show (package, SDK levels, permissions, components), android_frameworks_find (what it was made with), android_components_list (its screens and what other apps can start) and android_resources_read (its strings, per language). Rebuild what it does in your own code; don't copy its code, resources or name.";

fn is_package(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    PACKAGE_ENDINGS.iter().any(|e| p.ends_with(e))
}

impl TargetKind for AndroidKind {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn words(&self) -> KindWords {
        KindWords {
            button: "Run on Android",
            icon: "smartphone",
            ready: "Your Android app is ready",
            change_hint: "Ask for a change, e.g. \"a bigger search bar\" or \"add a settings screen\"",
        }
    }

    fn detect(&self, cx: &DetectCx) -> Option<u32> {
        if Self::STACKS.contains(&cx.stack) {
            // Picked as the stack: an Android app, unless a game engine said otherwise first.
            Some(150)
        } else if matches!(cx.target, TargetRef::Local { path } if is_package(&path.to_string_lossy())) {
            Some(150)
        } else if cx.says(&[
            " android app",
            " android application",
            " apk ",
            " app android",
            " aplicativo android",
            " aplicativo para android",
            " app para android",
            " android-app",
            " application android",
            " android 应用",
            " 安卓应用",
            " 安卓",
        ]) {
            // Below a game (60): "an Android game" is a game.
            Some(50)
        } else {
            None
        }
    }

    fn default_stack(&self, _cx: &DetectCx) -> &'static str {
        Self::DEFAULT_STACK
    }

    fn goal(&self) -> &'static str {
        "an Android app with the same core features"
    }

    fn brief_sections(&self, cx: &BriefCx) -> Vec<String> {
        let mut s = Vec::new();
        if cx.ws.manifest.own_project() {
            return s;
        }
        s.push(ANDROID_BRIEF.to_string());
        if cx.deep && matches!(&cx.ws.manifest.target, TargetRef::Local { path } if is_package(&path.to_string_lossy())) {
            s.push(STUDY_PACKAGE.to_string());
        }
        s
    }

    /// Its screens are on a device, not on this computer, so the builder doesn't photograph them.
    fn self_screenshots(&self) -> bool {
        false
    }

    fn checks(&self, _ws: &Workspace) -> Vec<Box<dyn Check>> {
        vec![Box::new(BuildCheck)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn detect(request: &str, target: &TargetRef, stack: &str) -> Option<u32> {
        AndroidKind.detect(&DetectCx::new(request, target, stack))
    }

    #[test]
    fn it_is_recognised_from_the_target_the_stack_and_the_words() {
        let none = TargetRef::None;
        for f in ["notes.apk", "Notes.APKS", "x.xapk", "app.aab"] {
            assert_eq!(detect("rebuild this", &TargetRef::Local { path: PathBuf::from(format!("/tmp/{f}")) }, ""), Some(150), "{f}");
        }
        assert_eq!(detect("rebuild this", &TargetRef::Local { path: PathBuf::from("/tmp/notes.exe") }, ""), None);
        assert_eq!(detect("a notes app", &none, "Flutter"), Some(150));
        assert_eq!(detect("make an android app for notes", &none, ""), Some(50));
        assert_eq!(detect("um aplicativo android de notas", &none, ""), Some(50));
        assert_eq!(detect("a notes app", &none, ""), None);
        assert_eq!(detect("a desktop app in kotlin", &none, ""), None);
    }

    #[test]
    fn every_android_stack_is_in_the_table() {
        for s in AndroidKind::STACKS {
            assert!(crate::rebuild::stacks::get(s).is_some(), "{s}");
        }
        assert!(AndroidKind::STACKS.contains(&AndroidKind::DEFAULT_STACK));
    }
}
