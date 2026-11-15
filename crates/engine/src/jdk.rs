//! Choosing a JDK for a build tool that only works under some of them (Gradle stops on one that is
//! too new: "Unsupported class file major version 71"). The supported range is a table
//! (`jdk.toml`); JDKs are found by searching the folders people install them in, not by a list of
//! versions, and a build is started with `JAVA_HOME` set to the one chosen. Nothing here starts a
//! program: a JDK's version is read from the `release` file it ships.

use craft_anything_engines::table::{Base, KnownRoot};
use craft_anything_evidence::{ErrorCode, ToolError};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The JDK versions a tool runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct Range {
    pub min: u32,
    pub max: u32,
    /// Chosen when several are installed; else the newest within the range.
    pub prefer: u32,
}

#[derive(serde::Deserialize)]
struct Row {
    name: String,
    #[serde(flatten)]
    range: Range,
}

#[derive(Default, serde::Deserialize)]
struct Table {
    range: Vec<Row>,
}

/// The supported range for a tool by its name in `jdk.toml`. A malformed table is covered by a test.
pub fn range(name: &str) -> Option<Range> {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| toml::from_str(include_str!("jdk.toml")).unwrap_or_default()).range.iter().find(|r| r.name == name).map(|r| r.range)
}

impl Range {
    fn allows(&self, major: u32) -> bool {
        (self.min..=self.max).contains(&major)
    }

    fn words(&self) -> String {
        format!("JDK {} to {} (JDK {} works best)", self.min, self.max, self.prefer)
    }
}

/// A JDK found on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jdk {
    /// The folder `JAVA_HOME` names (it holds `bin/` and `release`).
    pub home: PathBuf,
    /// Its Java feature version (21 for "21.0.5", 8 for "1.8.0_392").
    pub major: u32,
}

/// Where JDKs live, and how many folder levels below each to look: system and per-user installs,
/// the JDKs that IDEs and Gradle download, version managers, and the one bundled with Android
/// Studio (`jbr/`). Searched, so any version and vendor folder name is found.
const ROOTS: &[KnownRoot] = &[
    KnownRoot { os: "windows", base: Base::Env("ProgramFiles"), rel: "", depth: 3 },
    KnownRoot { os: "windows", base: Base::Env("ProgramFiles(x86)"), rel: "", depth: 3 },
    KnownRoot { os: "windows", base: Base::Env("LOCALAPPDATA"), rel: "Programs", depth: 3 },
    KnownRoot { os: "windows", base: Base::Env("LOCALAPPDATA"), rel: "JetBrains/Toolbox/apps", depth: 6 },
    KnownRoot { os: "macos", base: Base::Dir("/Library/Java/JavaVirtualMachines"), rel: "", depth: 3 },
    KnownRoot { os: "macos", base: Base::Env("HOME"), rel: "Library/Java/JavaVirtualMachines", depth: 3 },
    KnownRoot { os: "macos", base: Base::Dir("/Applications"), rel: "", depth: 4 },
    KnownRoot { os: "macos", base: Base::Dir("/opt/homebrew/opt"), rel: "", depth: 5 },
    KnownRoot { os: "linux", base: Base::Dir("/usr/lib/jvm"), rel: "", depth: 1 },
    KnownRoot { os: "linux", base: Base::Dir("/usr/java"), rel: "", depth: 1 },
    KnownRoot { os: "linux", base: Base::Dir("/opt"), rel: "", depth: 3 },
    KnownRoot { os: "linux", base: Base::Env("HOME"), rel: "android-studio", depth: 2 },
    KnownRoot { os: "linux", base: Base::Env("HOME"), rel: ".local/share/JetBrains/Toolbox/apps", depth: 6 },
    // The same in every home folder: IDE downloads, Gradle's own toolchain downloads, version managers.
    KnownRoot { os: "any", base: Base::Env("HOME"), rel: ".jdks", depth: 1 },
    KnownRoot { os: "any", base: Base::Env("HOME"), rel: ".gradle/jdks", depth: 1 },
    KnownRoot { os: "any", base: Base::Env("HOME"), rel: ".sdkman/candidates/java", depth: 1 },
    KnownRoot { os: "any", base: Base::Env("USERPROFILE"), rel: ".jdks", depth: 1 },
    KnownRoot { os: "any", base: Base::Env("USERPROFILE"), rel: ".gradle/jdks", depth: 1 },
];

/// Most folders looked into under one root, so a huge tree can't hold a build for long.
const MAX_FOLDERS_PER_ROOT: usize = 4000;

/// Where to look for a JDK: the environment's own choice, what `java` on PATH belongs to, and
/// the folders to search. Held as data so a test can use fake folders.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub java_home: Option<PathBuf>,
    /// The `java` program found on PATH.
    pub path_java: Option<PathBuf>,
    pub roots: Vec<(PathBuf, usize)>,
}

impl Sources {
    /// This computer's.
    pub fn host() -> Self {
        let os = std::env::consts::OS;
        let roots = ROOTS
            .iter()
            .filter(|k| k.os == os || k.os == "any")
            .filter_map(|k| {
                let base = match k.base {
                    Base::Env(var) => PathBuf::from(std::env::var_os(var).filter(|v| !v.is_empty())?),
                    Base::Dir(dir) => PathBuf::from(dir),
                };
                Some((base.join(k.rel), k.depth))
            })
            .collect();
        let exe = if cfg!(windows) { "java.exe" } else { "java" };
        let path_java = craft_anything_platform::which::which_in(exe, craft_anything_platform::which::search_path()).into_iter().next();
        Self { java_home: std::env::var_os("JAVA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from), path_java, roots }
    }
}

/// The JDK whose home is `home`, read from its `release` file; None for a folder that isn't one.
pub fn jdk_at(home: &Path) -> Option<Jdk> {
    let exe = if cfg!(windows) { "java.exe" } else { "java" };
    if !home.join("bin").join(exe).is_file() {
        return None;
    }
    let text = crate::tools::artifact_tools::read_capped(&home.join("release"), 64 * 1024).ok()?;
    let major = major_version(&String::from_utf8_lossy(&text))?;
    Some(Jdk { home: home.to_path_buf(), major })
}

/// The feature version in a `release` file's `JAVA_VERSION="…"` line.
fn major_version(release: &str) -> Option<u32> {
    let value = release.lines().find_map(|l| l.trim().strip_prefix("JAVA_VERSION="))?.trim().trim_matches('"');
    let mut parts = value.split(['.', '_', '-', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    // "1.8.0_392" is Java 8.
    if first == 1 { parts.next()?.parse().ok() } else { Some(first) }
}

/// Every JDK under the roots, each root searched to its depth. A found JDK isn't searched inside.
pub fn find_all(roots: &[(PathBuf, usize)]) -> Vec<Jdk> {
    let mut found: Vec<Jdk> = Vec::new();
    for (root, depth) in roots {
        let mut budget = MAX_FOLDERS_PER_ROOT;
        search(root, *depth + 1, &mut budget, &mut found);
    }
    found
}

fn search(dir: &Path, depth: usize, budget: &mut usize, found: &mut Vec<Jdk>) {
    if depth == 0 || *budget == 0 {
        return;
    }
    *budget = budget.saturating_sub(1);
    if let Some(jdk) = jdk_at(dir) {
        if !found.iter().any(|j| j.home == jdk.home) {
            found.push(jdk);
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut subs: Vec<PathBuf> = entries.flatten().take(512).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    subs.sort();
    for sub in subs {
        search(&sub, depth - 1, budget, found);
    }
}

/// The home of the JDK the `java` program at `java` belongs to (`<home>/bin/java`).
fn home_of(java: &Path) -> Option<PathBuf> {
    let home = java.parent()?.parent()?;
    if jdk_at(home).is_some() {
        return Some(home.to_path_buf());
    }
    // A link such as /usr/bin/java points into the JDK.
    let real = std::fs::canonicalize(java).ok()?;
    let home = real.parent()?.parent()?.to_path_buf();
    jdk_at(&home).map(|j| j.home)
}

/// The JDK to build with: the environment's own (`JAVA_HOME`, then `java` on PATH) when it is in
/// `range`, else the best one found by searching, else an error that says which versions work.
pub fn choose(range: Range, sources: &Sources) -> Result<Jdk, ToolError> {
    let own = sources.java_home.iter().cloned().chain(sources.path_java.as_deref().and_then(home_of)).filter_map(|h| jdk_at(&h));
    let mut seen: Vec<Jdk> = own.collect();
    if let Some(jdk) = seen.iter().find(|j| range.allows(j.major)) {
        return Ok(jdk.clone());
    }
    let searched = find_all(&sources.roots);
    let mut fits: Vec<&Jdk> = searched.iter().filter(|j| range.allows(j.major)).collect();
    // The preferred version first, then the newest; the same home folder order breaks ties.
    fits.sort_by_key(|j| (j.major != range.prefer, std::cmp::Reverse(j.major), j.home.clone()));
    if let Some(jdk) = fits.first() {
        return Ok((*jdk).clone());
    }
    seen.extend(searched);
    let mut majors: Vec<u32> = seen.iter().map(|j| j.major).collect();
    majors.sort_unstable();
    majors.dedup();
    let found = if majors.is_empty() { "No JDK was found on this computer.".to_string() } else { format!("Only JDK {} was found.", join(&majors)) };
    Err(ToolError::new(ErrorCode::EngineMissing, format!("The build needs a JDK it works with. {found}"))
        .with_remediation(format!(
            "Install {} (for example Eclipse Temurin 21), or open Android Studio once, whose bundled JDK is used, then try again. A JDK that is too new makes Gradle stop with \"Unsupported class file major version\".",
            range.words()
        ))
        .with_details(serde_json::json!({ "min": range.min, "max": range.max, "found": majors })))
}

fn join(majors: &[u32]) -> String {
    majors.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder shaped like a JDK that says it is `version`.
    fn fake(parent: &Path, name: &str, version: &str) -> PathBuf {
        let home = parent.join(name);
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }), "").unwrap();
        std::fs::write(home.join("release"), format!("IMPLEMENTOR=\"Fake\"\nJAVA_VERSION=\"{version}\"\n")).unwrap();
        home
    }

    const GRADLE: Range = Range { min: 17, max: 25, prefer: 21 };

    #[test]
    fn the_table_parses_and_has_gradle() {
        assert_eq!(range("gradle"), Some(GRADLE));
        assert_eq!(range("no-such-tool"), None);
        assert!(toml::from_str::<Table>(include_str!("jdk.toml")).is_ok());
    }

    #[test]
    fn versions_are_read_from_the_release_file() {
        for (text, major) in [
            ("JAVA_VERSION=\"21.0.5\"", Some(21)),
            ("JAVA_VERSION=\"1.8.0_392\"", Some(8)),
            ("JAVA_VERSION=\"27\"", Some(27)),
            ("JAVA_VERSION=\"17-ea\"", Some(17)),
            ("X=1", None),
            ("JAVA_VERSION=\"\"", None),
        ] {
            assert_eq!(major_version(text), major, "{text}");
        }
    }

    #[test]
    fn a_folder_without_a_release_file_or_java_is_not_a_jdk() {
        let d = tempfile::tempdir().unwrap();
        let home = fake(d.path(), "jdk", "21.0.1");
        assert_eq!(jdk_at(&home).map(|j| j.major), Some(21));
        std::fs::remove_file(home.join("release")).unwrap();
        assert_eq!(jdk_at(&home), None);
        std::fs::create_dir_all(d.path().join("empty/bin")).unwrap();
        std::fs::write(d.path().join("empty/release"), "JAVA_VERSION=\"21\"").unwrap();
        assert_eq!(jdk_at(&d.path().join("empty")), None);
    }

    /// JAVA_HOME names a JDK too new for Gradle and PATH's java is too old: the one inside Android
    /// Studio (three levels down) is chosen, not the newest nor either of those.
    #[test]
    fn a_compatible_jdk_is_searched_for_when_the_environment_has_none() {
        let d = tempfile::tempdir().unwrap();
        let (new, old) = (fake(d.path(), "jdk-27", "27.0.1"), fake(d.path(), "jdk-11", "11.0.2"));
        let studio = fake(&d.path().join("Android/Android Studio"), "jbr", "21.0.8");
        let sources =
            Sources { java_home: Some(new), path_java: Some(old.join("bin/java")), roots: vec![(d.path().join("missing"), 3), (d.path().join("Android"), 2)] };
        assert_eq!(choose(GRADLE, &sources).unwrap(), Jdk { home: studio.clone(), major: 21 });
        // One level too shallow doesn't reach it.
        let shallow = Sources { roots: vec![(d.path().join("Android"), 1)], ..sources.clone() };
        let err = choose(GRADLE, &shallow).unwrap_err();
        assert!(err.message.contains("27") && err.message.contains("11"), "{}", err.message);
        // JAVA_HOME in range is used as is.
        let ok = fake(d.path(), "jdk-24", "24");
        assert_eq!(choose(GRADLE, &Sources { java_home: Some(ok.clone()), ..sources }).unwrap().home, ok);
    }

    #[test]
    fn the_preferred_version_wins_then_the_newest_in_range() {
        let d = tempfile::tempdir().unwrap();
        for (name, v) in [("a", "17.0.1"), ("b", "21.0.2"), ("c", "25.0.1"), ("d", "27.0.1")] {
            fake(d.path(), name, v);
        }
        let roots = vec![(d.path().to_path_buf(), 1)];
        let sources = Sources { roots: roots.clone(), ..Default::default() };
        assert_eq!(choose(GRADLE, &sources).unwrap().major, 21);
        assert_eq!(choose(Range { prefer: 20, ..GRADLE }, &sources).unwrap().major, 25);
        assert_eq!(choose(Range { min: 26, max: 27, prefer: 26 }, &sources).unwrap().major, 27);
    }

    #[test]
    fn a_java_on_path_is_used_when_it_fits() {
        let d = tempfile::tempdir().unwrap();
        let home = fake(d.path(), "jdk-17", "17.0.9");
        let sources = Sources { path_java: Some(home.join("bin/java")), ..Default::default() };
        assert_eq!(choose(GRADLE, &sources).unwrap().home, home);
    }

    #[test]
    fn no_jdk_is_an_error_that_says_which_versions_work() {
        let d = tempfile::tempdir().unwrap();
        let err = choose(GRADLE, &Sources { roots: vec![(d.path().to_path_buf(), 2)], ..Default::default() }).unwrap_err();
        assert_eq!(err.code, ErrorCode::EngineMissing);
        assert!(err.message.contains("No JDK"), "{}", err.message);
        let help = err.remediation.clone().unwrap_or_default();
        assert!(help.contains("JDK 17 to 25") && help.contains("21"), "{help}");
    }

    #[test]
    fn a_search_is_bounded_and_does_not_enter_a_jdk() {
        let d = tempfile::tempdir().unwrap();
        let home = fake(d.path(), "jdk-21", "21");
        fake(&home, "nested", "22");
        let found = find_all(&[(d.path().to_path_buf(), 2), (d.path().to_path_buf(), 2)]);
        assert_eq!(found.len(), 1, "{found:?}");
    }
}
