//! The Android SDK on this computer: where it is and what is in it.
//!
//! Everything here reads folders and small text files; no SDK program is run. An SDK is a folder
//! with `platform-tools/`, `build-tools/<version>/`, `platforms/android-<level>/`, `ndk/<version>/`,
//! `cmdline-tools/<version>/`, `emulator/` and `system-images/<platform>/<tag>/<abi>/`; each package
//! says its revision in `source.properties`. Virtual devices live elsewhere, as `<name>.ini` files
//! beside `<name>.avd/config.ini` folders.
//!
//! Folder and file contents are not trusted: listings are capped ([`MAX_ENTRIES`]), files are read
//! up to [`MAX_FILE`] bytes, and anything unreadable is left out rather than failing the whole read.

use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Most entries read from any one folder.
pub const MAX_ENTRIES: usize = 512;
/// Most bytes read from a `source.properties` or `.ini` file.
pub const MAX_FILE: u64 = 64 * 1024;
/// Most lines looked at in such a file.
const MAX_LINES: usize = 2_000;

/// Where to look for an SDK and for virtual devices. Built by [`Places::here`] from this computer,
/// or by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// A folder the caller named; looked at first and reported even when it isn't an SDK.
    pub given: Option<PathBuf>,
    /// The adb in use (`<sdk>/platform-tools/adb`): its SDK is the one the adb tools talk through.
    pub adb: Option<PathBuf>,
    /// `ANDROID_HOME`.
    pub android_home: Option<PathBuf>,
    /// `ANDROID_SDK_ROOT` (the older name).
    pub android_sdk_root: Option<PathBuf>,
    /// The usual install folders for this system.
    pub usual: Vec<PathBuf>,
    /// Folders that may hold virtual devices, most specific first.
    pub avd_homes: Vec<PathBuf>,
}

impl Places {
    /// The environment variables and usual folders of this computer.
    pub fn here() -> Places {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = env("HOME").or_else(|| env("USERPROFILE"));
        let mut usual = Vec::new();
        if let Some(local) = env("LOCALAPPDATA") {
            usual.push(local.join("Android").join("Sdk"));
        }
        if let Some(home) = &home {
            usual.push(home.join("Library").join("Android").join("sdk"));
            usual.push(home.join("Android").join("Sdk"));
        }
        let mut avd_homes = Vec::new();
        avd_homes.extend(env("ANDROID_AVD_HOME"));
        avd_homes.extend(env("ANDROID_USER_HOME").map(|p| p.join("avd")));
        avd_homes.extend(home.map(|p| p.join(".android").join("avd")));
        Places { given: None, adb: None, android_home: env("ANDROID_HOME"), android_sdk_root: env("ANDROID_SDK_ROOT"), usual, avd_homes }
    }

    /// The SDK folders to try, in order, each with how it was named.
    pub fn candidates(&self) -> Vec<(PathBuf, RootSource)> {
        let mut out: Vec<(PathBuf, RootSource)> = Vec::new();
        let mut add = |p: Option<PathBuf>, source: RootSource| {
            if let Some(p) = p
                && !out.iter().any(|(q, _)| *q == p)
            {
                out.push((p, source));
            }
        };
        add(self.given.clone(), RootSource::Given);
        add(self.adb.as_deref().and_then(sdk_of_adb), RootSource::Adb);
        add(self.android_home.clone(), RootSource::AndroidHome);
        add(self.android_sdk_root.clone(), RootSource::AndroidSdkRoot);
        for p in &self.usual {
            add(Some(p.clone()), RootSource::UsualFolder);
        }
        out
    }
}

/// How an SDK folder was named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootSource {
    Given,
    Adb,
    AndroidHome,
    AndroidSdkRoot,
    UsualFolder,
}

/// One installed package: its folder name and the revision its `source.properties` states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Package {
    /// The folder's name: a version (`36.1.0`), a platform (`android-36`), `latest`…
    pub name: String,
    /// `Pkg.Revision`, when the package says one.
    pub revision: Option<String>,
    pub path: PathBuf,
}

/// A system image an emulator can boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemImage {
    /// `android-37.0`
    pub platform: String,
    /// `google_apis_playstore`, `default`…
    pub tag: String,
    /// `x86_64`, `arm64-v8a`…
    pub abi: String,
    pub revision: Option<String>,
    pub path: PathBuf,
}

/// What an SDK folder holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sdk {
    pub root: PathBuf,
    pub source: RootSource,
    pub platform_tools: Option<Package>,
    pub emulator: Option<Package>,
    pub build_tools: Vec<Package>,
    pub platforms: Vec<Package>,
    pub ndks: Vec<Package>,
    pub cmdline_tools: Vec<Package>,
    pub system_images: Vec<SystemImage>,
    /// A folder had more than [`MAX_ENTRIES`] entries, so a list above may be short.
    pub truncated: bool,
}

/// A virtual device, as its `.ini` and `config.ini` describe it. A field the files don't state is
/// `None`, never a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Avd {
    pub name: String,
    /// The `.avd` folder its `.ini` points at.
    pub path: Option<PathBuf>,
    /// The `.avd` folder exists and has a `config.ini`.
    pub usable: bool,
    /// `android-37.0`
    pub target: Option<String>,
    pub abi: Option<String>,
    pub tag: Option<String>,
    pub device: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub density: Option<u32>,
    pub ram_mb: Option<u32>,
    /// The system image folder, relative to the SDK.
    pub system_image: Option<String>,
}

/// Virtual devices found in one folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Avds {
    pub home: PathBuf,
    pub devices: Vec<Avd>,
    pub truncated: bool,
}

/// The SDK an adb belongs to: `<sdk>/platform-tools/adb`. `None` for an adb installed any other way.
pub fn sdk_of_adb(adb: &Path) -> Option<PathBuf> {
    let tools = adb.parent()?;
    let named = tools.file_name()?.to_str()?.eq_ignore_ascii_case("platform-tools");
    named.then(|| tools.parent().map(Path::to_path_buf)).flatten()
}

/// Whether a folder looks like an SDK: it has at least one of the package folders.
pub fn is_sdk(root: &Path) -> bool {
    ["platform-tools", "build-tools", "platforms", "cmdline-tools"].iter().any(|d| root.join(d).is_dir())
}

/// The first candidate that is an SDK, read.
pub fn find(places: &Places) -> Option<Sdk> {
    places.candidates().into_iter().find(|(p, _)| is_sdk(p)).map(|(p, source)| read(&p, source))
}

/// Read what an SDK folder holds. Missing parts are empty, not errors.
pub fn read(root: &Path, source: RootSource) -> Sdk {
    let mut truncated = false;
    let single = |name: &str| {
        let path = root.join(name);
        path.is_dir().then(|| Package { name: name.to_string(), revision: revision(&path), path })
    };
    let mut packages = |name: &str| {
        let (dirs, cut) = subdirs(&root.join(name));
        truncated |= cut;
        let mut list: Vec<Package> = dirs.into_iter().map(|(name, path)| Package { name, revision: revision(&path), path }).collect();
        list.sort_by(|a, b| version_key(&a.name).cmp(&version_key(&b.name)).then_with(|| a.name.cmp(&b.name)));
        list
    };
    let build_tools = packages("build-tools");
    let platforms = packages("platforms");
    let ndks = packages("ndk");
    let cmdline_tools = packages("cmdline-tools");
    let mut system_images = Vec::new();
    let (platform_dirs, cut) = subdirs(&root.join("system-images"));
    truncated |= cut;
    for (platform, platform_path) in platform_dirs {
        let (tags, cut) = subdirs(&platform_path);
        truncated |= cut;
        for (tag, tag_path) in tags {
            let (abis, cut) = subdirs(&tag_path);
            truncated |= cut;
            for (abi, path) in abis {
                if system_images.len() >= MAX_ENTRIES {
                    truncated = true;
                    break;
                }
                system_images.push(SystemImage { platform: platform.clone(), tag: tag.clone(), abi, revision: revision(&path), path });
            }
        }
    }
    system_images.sort_by(|a, b| {
        version_key(&a.platform).cmp(&version_key(&b.platform)).then_with(|| (&a.platform, &a.tag, &a.abi).cmp(&(&b.platform, &b.tag, &b.abi)))
    });
    Sdk {
        root: root.to_path_buf(),
        source,
        platform_tools: single("platform-tools"),
        emulator: single("emulator"),
        build_tools,
        platforms,
        ndks,
        cmdline_tools,
        system_images,
        truncated,
    }
}

/// The virtual devices in the first folder of `homes` that exists.
pub fn find_avds(homes: &[PathBuf]) -> Option<Avds> {
    homes.iter().find(|h| h.is_dir()).map(|h| read_avds(h))
}

/// Read the virtual devices a folder lists: one `<name>.ini` each, pointing at its `.avd` folder.
pub fn read_avds(home: &Path) -> Avds {
    let mut devices = Vec::new();
    let mut truncated = false;
    let Ok(entries) = std::fs::read_dir(home) else {
        return Avds { home: home.to_path_buf(), devices, truncated };
    };
    for (seen, entry) in entries.flatten().enumerate() {
        if seen >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let path = entry.path();
        let is_ini = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("ini"));
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).filter(|_| is_ini && path.is_file()) else {
            continue;
        };
        devices.push(read_avd(home, name, &path));
    }
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    Avds { home: home.to_path_buf(), devices, truncated }
}

fn read_avd(home: &Path, name: &str, ini: &Path) -> Avd {
    let pointer = properties(ini);
    let get = |props: &[(String, String)], key: &str| props.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).filter(|v| !v.is_empty());
    // `path` is absolute; `path.rel` is relative to the folder above the avd home. A device copied
    // from another computer keeps a stale `path`, so the folder beside the .ini is tried last.
    let folder = [
        get(&pointer, "path").map(PathBuf::from),
        get(&pointer, "path.rel").and_then(|rel| home.parent().map(|up| up.join(rel))),
        Some(home.join(format!("{name}.avd"))),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_dir());
    let config_file = folder.as_ref().map(|f| f.join("config.ini")).filter(|c| c.is_file());
    let config = config_file.as_deref().map(properties).unwrap_or_default();
    let number = |key: &str| get(&config, key).and_then(|v| v.trim_end_matches(['M', 'm']).parse::<u32>().ok());
    let system_image = get(&config, "image.sysdir.1").map(|v| v.replace('\\', "/").trim_end_matches('/').to_string());
    Avd {
        name: name.to_string(),
        usable: config_file.is_some(),
        path: folder,
        target: get(&pointer, "target").or_else(|| system_image.as_deref().and_then(|s| s.split('/').nth(1)).map(str::to_string)),
        abi: get(&config, "abi.type"),
        tag: get(&config, "tag.id"),
        device: get(&config, "hw.device.name"),
        width: number("hw.lcd.width"),
        height: number("hw.lcd.height"),
        density: number("hw.lcd.density"),
        ram_mb: number("hw.ramSize"),
        system_image,
    }
}

/// `Pkg.Revision` from a package's `source.properties`.
fn revision(package: &Path) -> Option<String> {
    properties(&package.join("source.properties")).into_iter().find(|(k, _)| k == "Pkg.Revision").map(|(_, v)| v).filter(|v| !v.is_empty())
}

/// The `key=value` lines of a small properties or `.ini` file; comments, sections and anything
/// past the caps are left out. An unreadable file has none.
fn properties(file: &Path) -> Vec<(String, String)> {
    let Ok(f) = std::fs::File::open(file) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if f.take(MAX_FILE).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&bytes)
        .lines()
        .take(MAX_LINES)
        .filter_map(|line| {
            let line = line.trim().trim_start_matches('\u{feff}');
            if line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// The sub-folders of a folder as (name, path), and whether the listing was cut at the cap.
fn subdirs(dir: &Path) -> (Vec<(String, PathBuf)>, bool) {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (out, false);
    };
    for (seen, entry) in entries.flatten().enumerate() {
        if seen >= MAX_ENTRIES {
            return (out, true);
        }
        let path = entry.path();
        if let Some(name) = entry.file_name().to_str()
            && path.is_dir()
        {
            out.push((name.to_string(), path));
        }
    }
    (out, false)
}

/// The numbers in a name, for ordering versions: `android-36.1` → [36, 1], `9.0.0` → [9, 0, 0],
/// so 9 sorts before 36. A name with no number sorts first.
fn version_key(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).take(8).map(|s| s.parse::<u64>().unwrap_or(u64::MAX)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn package(root: &Path, rel: &str, revision: Option<&str>) {
        let dir = root.join(rel);
        fs::create_dir_all(&dir).unwrap();
        if let Some(r) = revision {
            fs::write(dir.join("source.properties"), format!("Pkg.Desc = something\nPkg.Revision={r}\n")).unwrap();
        }
    }

    fn sample_sdk() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let r = t.path();
        package(r, "platform-tools", Some("37.0.0"));
        package(r, "emulator", Some("36.4.9"));
        package(r, "build-tools/9.0.0", Some("9.0.0"));
        package(r, "build-tools/36.1.0", Some("36.1.0"));
        package(r, "build-tools/34.0.0", None);
        package(r, "platforms/android-36.1", Some("1"));
        package(r, "platforms/android-34", Some("3"));
        package(r, "ndk/28.2.13676358", Some("28.2.13676358"));
        package(r, "cmdline-tools/latest", Some("19.0"));
        package(r, "system-images/android-37.0/google_apis_playstore/x86_64", Some("2"));
        package(r, "system-images/android-34/default/arm64-v8a", None);
        fs::write(r.join("build-tools").join("notes.txt"), "not a version").unwrap();
        t
    }

    #[test]
    fn an_sdk_is_read_with_versions_in_order_and_revisions_as_stated() {
        let t = sample_sdk();
        let sdk = read(t.path(), RootSource::Given);
        assert_eq!(sdk.platform_tools.as_ref().and_then(|p| p.revision.as_deref()), Some("37.0.0"));
        assert_eq!(sdk.emulator.as_ref().and_then(|p| p.revision.as_deref()), Some("36.4.9"));
        let names = |l: &[Package]| l.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&sdk.build_tools), ["9.0.0", "34.0.0", "36.1.0"], "numeric order, and a file is not a version");
        assert_eq!(sdk.build_tools.iter().find(|p| p.name == "34.0.0").and_then(|p| p.revision.clone()), None, "no source.properties, no revision");
        assert_eq!(names(&sdk.platforms), ["android-34", "android-36.1"]);
        assert_eq!(names(&sdk.ndks), ["28.2.13676358"]);
        assert_eq!(names(&sdk.cmdline_tools), ["latest"]);
        let images: Vec<_> = sdk.system_images.iter().map(|i| (i.platform.as_str(), i.tag.as_str(), i.abi.as_str())).collect();
        assert_eq!(images, [("android-34", "default", "arm64-v8a"), ("android-37.0", "google_apis_playstore", "x86_64")]);
        assert!(!sdk.truncated);
    }

    #[test]
    fn an_empty_or_missing_folder_is_no_sdk_and_reads_as_nothing() {
        let t = tempfile::tempdir().unwrap();
        assert!(!is_sdk(t.path()));
        assert!(!is_sdk(&t.path().join("absent")));
        let sdk = read(&t.path().join("absent"), RootSource::Given);
        assert!(sdk.platform_tools.is_none() && sdk.build_tools.is_empty() && sdk.system_images.is_empty());
    }

    #[test]
    fn the_first_candidate_that_is_an_sdk_wins_and_says_how_it_was_named() {
        let t = sample_sdk();
        let empty = tempfile::tempdir().unwrap();
        let places = Places {
            given: Some(empty.path().to_path_buf()),
            android_home: Some(empty.path().join("absent")),
            android_sdk_root: Some(t.path().to_path_buf()),
            ..Default::default()
        };
        let sdk = find(&places).unwrap();
        assert_eq!((sdk.root.as_path(), sdk.source), (t.path(), RootSource::AndroidSdkRoot));
        assert!(find(&Places { given: Some(empty.path().to_path_buf()), ..Default::default() }).is_none());
        assert!(find(&Places::default()).is_none(), "no place to look finds nothing");
    }

    #[test]
    fn an_adb_names_its_sdk_only_from_platform_tools() {
        let t = sample_sdk();
        let adb = t.path().join("platform-tools").join("adb.exe");
        assert_eq!(sdk_of_adb(&adb).as_deref(), Some(t.path()));
        assert_eq!(sdk_of_adb(Path::new("/usr/bin/adb")), None);
        assert_eq!(sdk_of_adb(Path::new("adb")), None);
        let sdk = find(&Places { adb: Some(adb), ..Default::default() }).unwrap();
        assert_eq!(sdk.source, RootSource::Adb);
    }

    #[test]
    fn the_same_folder_named_twice_is_tried_once() {
        let p = PathBuf::from("/sdk");
        let places = Places { android_home: Some(p.clone()), android_sdk_root: Some(p.clone()), usual: vec![p.clone()], ..Default::default() };
        assert_eq!(places.candidates(), [(p, RootSource::AndroidHome)]);
    }

    fn avd(home: &Path, name: &str, pointer: &str, config: Option<&str>) {
        fs::create_dir_all(home).unwrap();
        fs::write(home.join(format!("{name}.ini")), pointer).unwrap();
        if let Some(c) = config {
            let dir = home.join(format!("{name}.avd"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("config.ini"), c).unwrap();
        }
    }

    #[test]
    fn virtual_devices_are_read_from_their_ini_and_config() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join(".android").join("avd");
        let config = "abi.type = x86_64\ntag.id=google_apis_playstore\nhw.device.name=medium_phone\nhw.lcd.width=1080\nhw.lcd.height = 2400\nhw.lcd.density=420\nhw.ramSize=2048M\nimage.sysdir.1=system-images\\android-37.0\\google_apis_playstore\\x86_64\\\n";
        // A stale absolute path from another computer: the folder beside the .ini is used.
        avd(
            &home,
            "Medium_Phone",
            "avd.ini.encoding=UTF-8\npath=/somewhere/else/Medium_Phone.avd\npath.rel=avd/Medium_Phone.avd\ntarget=android-37.0\n",
            Some(config),
        );
        avd(&home, "Broken", "path.rel=avd/Broken.avd\n", None);
        fs::write(home.join("notes.txt"), "x").unwrap();
        let found = find_avds(&[t.path().join("absent"), home.clone()]).unwrap();
        assert_eq!(found.home, home);
        assert_eq!(found.devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Broken", "Medium_Phone"]);
        let phone = found.devices.iter().find(|d| d.name == "Medium_Phone").unwrap();
        assert!(phone.usable);
        assert_eq!(phone.path.as_deref(), Some(home.join("Medium_Phone.avd").as_path()));
        assert_eq!(
            (phone.target.as_deref(), phone.abi.as_deref(), phone.tag.as_deref()),
            (Some("android-37.0"), Some("x86_64"), Some("google_apis_playstore"))
        );
        assert_eq!((phone.width, phone.height, phone.density, phone.ram_mb), (Some(1080), Some(2400), Some(420), Some(2048)));
        assert_eq!(phone.device.as_deref(), Some("medium_phone"));
        assert_eq!(phone.system_image.as_deref(), Some("system-images/android-37.0/google_apis_playstore/x86_64"));
        let broken = found.devices.iter().find(|d| d.name == "Broken").unwrap();
        assert!(!broken.usable && broken.path.is_none() && broken.abi.is_none() && broken.width.is_none(), "{broken:?}");
    }

    #[test]
    fn a_target_missing_from_the_ini_is_taken_from_the_system_image_folder() {
        let t = tempfile::tempdir().unwrap();
        avd(t.path(), "P", "path.rel=avd/P.avd\n", Some("image.sysdir.1=system-images/android-34/default/arm64-v8a/\n"));
        let d = read_avds(t.path()).devices.pop().unwrap();
        assert_eq!(d.target.as_deref(), Some("android-34"));
    }

    #[test]
    fn hostile_files_are_read_within_the_caps_and_never_fail() {
        let t = tempfile::tempdir().unwrap();
        // Not UTF-8, no `=`, a huge value, a number that overflows, and a file past the size cap.
        let mut config = vec![0xff, 0xfe, b'\n'];
        config.extend_from_slice(b"hw.lcd.width=99999999999999999999\nhw.lcd.height\n=\nabi.type=");
        config.extend(std::iter::repeat_n(b'a', 1024));
        config.extend_from_slice(b"\n");
        config.extend(std::iter::repeat_n(b'#', MAX_FILE as usize));
        config.extend_from_slice(b"\nhw.ramSize=4096\n");
        fs::create_dir_all(t.path().join("X.avd")).unwrap();
        fs::write(t.path().join("X.avd").join("config.ini"), &config).unwrap();
        fs::write(t.path().join("X.ini"), [0xffu8, 0x00, b'=']).unwrap();
        let d = read_avds(t.path()).devices.pop().unwrap();
        assert!(d.usable);
        assert_eq!(d.width, None, "an overflowing number is not a width");
        assert_eq!(d.abi.as_ref().map(String::len), Some(1024));
        assert_eq!(d.ram_mb, None, "past the size cap is not read");
        assert!(version_key("99999999999999999999999.1").first() == Some(&u64::MAX));
    }

    #[test]
    fn a_folder_with_too_many_entries_is_cut_and_says_so() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..MAX_ENTRIES + 3 {
            fs::create_dir_all(t.path().join("build-tools").join(format!("1.0.{i}"))).unwrap();
        }
        let sdk = read(t.path(), RootSource::Given);
        assert!(sdk.truncated);
        assert_eq!(sdk.build_tools.len(), MAX_ENTRIES);
    }
}
