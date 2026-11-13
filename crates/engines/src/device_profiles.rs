//! Device profiles: the kinds of Android screen a check can ask an emulator for.
//!
//! The table is data (`device_profiles.toml`, embedded): resolution, density, API level, ABI and
//! memory per profile. [`match_avd`] compares a profile with a virtual device that
//! [`crate::android_sdk`] read from folders; [`rank`] and [`pick`] choose among several. Nothing here
//! starts an emulator or contacts a device: a field the device's files don't state is reported as
//! unknown, never guessed, so a check can tell "does not fit" from "could not tell".

use crate::android_sdk::Avd;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// The table's schema version this code reads.
pub const TABLE_VERSION: u32 = 1;

/// What sort of device a profile stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Phone,
    Tablet,
}

/// One row of the table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    /// Stable id a check names (`phone_medium`).
    pub id: String,
    /// What people see.
    pub title: String,
    pub kind: Kind,
    /// Portrait pixels.
    pub width: u32,
    pub height: u32,
    /// `hw.lcd.density`, dots per inch.
    pub density: u32,
    /// The lowest Android API level the device may run.
    pub min_api: u32,
    /// The CPU types the device may have.
    pub abis: Vec<String>,
    /// The least memory the device may have.
    pub ram_mb: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    version: u32,
    profile: Vec<DeviceProfile>,
}

/// Parse a table. A separate function so a test can fail on the parse error itself.
fn parse(text: &str) -> Result<Vec<DeviceProfile>, String> {
    let table: Table = toml::from_str(text).map_err(|e| e.to_string())?;
    if table.version != TABLE_VERSION {
        return Err(format!("device_profiles.toml is version {}, this build reads {TABLE_VERSION}", table.version));
    }
    Ok(table.profile)
}

/// The built-in profiles, in table order. Empty only if the embedded table is broken, which a
/// test refuses.
pub fn profiles() -> &'static [DeviceProfile] {
    static TABLE: OnceLock<Vec<DeviceProfile>> = OnceLock::new();
    TABLE.get_or_init(|| parse(include_str!("device_profiles.toml")).unwrap_or_default())
}

/// The profile with this id.
pub fn profile(id: &str) -> Option<&'static DeviceProfile> {
    profiles().iter().find(|p| p.id == id)
}

/// How well a virtual device fits a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Every field is stated and fits.
    Matches,
    /// Nothing known differs, but the device's files leave some field out ([`ProfileMatch::unknown`]).
    Unverified,
    /// At least one stated field differs ([`ProfileMatch::differences`]).
    Differs,
}

/// One field that does not fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Difference {
    /// `usable`, `screen`, `density`, `api_level`, `abi` or `ram_mb`.
    pub field: &'static str,
    pub wanted: String,
    pub found: String,
}

/// A profile compared with one virtual device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileMatch {
    pub profile: String,
    /// The virtual device's name.
    pub avd: String,
    pub fit: Fit,
    pub differences: Vec<Difference>,
    /// Fields the device's files don't state, so they were not compared.
    pub unknown: Vec<&'static str>,
}

/// The Android API level a virtual device runs, from its target (`android-37.0` is 37,
/// `android-34` is 34). `None` for a preview name (`android-VanillaIceCream`) or no target.
pub fn api_level(target: &str) -> Option<u32> {
    let rest = target.rsplit(['-', ':']).next().unwrap_or(target);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Compare a profile with a virtual device.
pub fn match_avd(profile: &DeviceProfile, avd: &Avd) -> ProfileMatch {
    let mut differences = Vec::new();
    let mut unknown = Vec::new();
    let mut differ = |field: &'static str, wanted: String, found: String| differences.push(Difference { field, wanted, found });

    if !avd.usable {
        differ("usable", "a device folder with a config.ini".into(), "missing".into());
    }
    match (avd.width, avd.height) {
        // A device may be stored landscape.
        (Some(w), Some(h)) if (w, h) != (profile.width, profile.height) && (h, w) != (profile.width, profile.height) => {
            differ("screen", format!("{}x{}", profile.width, profile.height), format!("{w}x{h}"));
        }
        (Some(_), Some(_)) => {}
        _ => unknown.push("screen"),
    }
    match avd.density {
        Some(d) if d != profile.density => differ("density", profile.density.to_string(), d.to_string()),
        Some(_) => {}
        None => unknown.push("density"),
    }
    match avd.target.as_deref().and_then(api_level) {
        Some(level) if level < profile.min_api => differ("api_level", format!("{} or newer", profile.min_api), level.to_string()),
        Some(_) => {}
        None => unknown.push("api_level"),
    }
    match avd.abi.as_deref() {
        Some(abi) if !profile.abis.iter().any(|a| a == abi) => differ("abi", profile.abis.join(" or "), abi.to_string()),
        Some(_) => {}
        None => unknown.push("abi"),
    }
    match avd.ram_mb {
        Some(ram) if ram < profile.ram_mb => differ("ram_mb", format!("{} or more", profile.ram_mb), ram.to_string()),
        Some(_) => {}
        None => unknown.push("ram_mb"),
    }
    let fit = if !differences.is_empty() {
        Fit::Differs
    } else if !unknown.is_empty() {
        Fit::Unverified
    } else {
        Fit::Matches
    };
    ProfileMatch { profile: profile.id.clone(), avd: avd.name.clone(), fit, differences, unknown }
}

/// Every device compared with the profile, best fit first (then fewest differences, then name).
pub fn rank(profile: &DeviceProfile, avds: &[Avd]) -> Vec<ProfileMatch> {
    let mut all: Vec<ProfileMatch> = avds.iter().map(|a| match_avd(profile, a)).collect();
    all.sort_by(|a, b| a.fit.cmp(&b.fit).then(a.differences.len().cmp(&b.differences.len())).then_with(|| a.avd.cmp(&b.avd)));
    all
}

/// The best device that does not differ from the profile, if any: a full match before one that
/// could not be fully checked.
pub fn pick(profile: &DeviceProfile, avds: &[Avd]) -> Option<ProfileMatch> {
    rank(profile, avds).into_iter().find(|m| m.fit != Fit::Differs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avd(name: &str) -> Avd {
        Avd {
            name: name.into(),
            path: None,
            usable: true,
            target: Some("android-37.0".into()),
            abi: Some("x86_64".into()),
            tag: None,
            device: None,
            width: Some(1080),
            height: Some(2400),
            density: Some(420),
            ram_mb: Some(2048),
            system_image: None,
        }
    }

    #[test]
    fn the_embedded_table_parses_and_is_well_formed() {
        // `unwrap` on the parse itself, so a typo in the table fails here with its own message.
        let parsed = parse(include_str!("device_profiles.toml")).unwrap();
        assert_eq!(parsed, profiles(), "the shipped table must not fall back to empty");
        assert!(parsed.len() >= 4);
        let mut ids = std::collections::HashSet::new();
        for p in &parsed {
            assert!(ids.insert(&p.id), "duplicate id {}", p.id);
            assert!(!p.id.is_empty() && p.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'), "{}", p.id);
            assert!(!p.title.is_empty());
            assert!(p.width > 0 && p.height > p.width, "portrait pixels: {}", p.id);
            assert!((100..=800).contains(&p.density) && p.min_api >= 21 && p.ram_mb >= 512, "{}", p.id);
            assert!(!p.abis.is_empty() && p.abis.iter().all(|a| ["x86_64", "arm64-v8a", "x86", "armeabi-v7a"].contains(&a.as_str())), "{}", p.id);
        }
        assert!(profile("phone_medium").is_some() && profile("nope").is_none());
    }

    #[test]
    fn a_broken_or_future_table_is_an_error_not_an_empty_list() {
        assert!(parse("version = 1\n[[profile]]\nid = \"x\"\nbogus = 1\n").is_err(), "unknown fields are refused");
        assert!(parse("version = 2\nprofile = []\n").unwrap_err().contains("version 2"));
        assert!(parse("not toml [").is_err());
    }

    #[test]
    fn api_levels_are_read_from_targets() {
        assert_eq!(api_level("android-37.0"), Some(37));
        assert_eq!(api_level("android-34"), Some(34));
        assert_eq!(api_level("android-36.1"), Some(36));
        assert_eq!(api_level("Google Inc.:Google APIs:30"), Some(30));
        assert_eq!(api_level("android-VanillaIceCream"), None);
        assert_eq!(api_level(""), None);
        assert_eq!(api_level("android-99999999999999999999"), None, "overflow is unknown, not a level");
    }

    #[test]
    fn a_device_that_fits_matches_and_each_difference_is_named() {
        let p = profile("phone_medium").unwrap();
        let m = match_avd(p, &avd("Good"));
        assert_eq!((m.fit, m.differences.len(), m.unknown.len()), (Fit::Matches, 0, 0));
        let landscape = Avd { width: Some(2400), height: Some(1080), ..avd("Landscape") };
        assert_eq!(match_avd(p, &landscape).fit, Fit::Matches, "a rotated screen is the same screen");
        let bad = Avd {
            height: Some(1920),
            density: Some(320),
            target: Some("android-30".into()),
            abi: Some("armeabi-v7a".into()),
            ram_mb: Some(1024),
            usable: false,
            ..avd("Bad")
        };
        let m = match_avd(p, &bad);
        assert_eq!(m.fit, Fit::Differs);
        let fields: Vec<_> = m.differences.iter().map(|d| d.field).collect();
        assert_eq!(fields, ["usable", "screen", "density", "api_level", "abi", "ram_mb"]);
        assert_eq!(m.differences[1].wanted, "1080x2400");
        assert_eq!(m.differences[1].found, "1080x1920");
    }

    #[test]
    fn a_field_the_files_leave_out_is_unknown_and_never_a_match() {
        let p = profile("phone_medium").unwrap();
        let sparse = Avd { width: None, density: None, target: None, abi: None, ram_mb: None, ..avd("Sparse") };
        let m = match_avd(p, &sparse);
        assert_eq!(m.fit, Fit::Unverified);
        assert_eq!(m.unknown, ["screen", "density", "api_level", "abi", "ram_mb"]);
        assert!(m.differences.is_empty());
        // A known difference beats a gap: it still differs.
        let m = match_avd(p, &Avd { density: Some(160), abi: None, ..avd("Mixed") });
        assert_eq!((m.fit, m.unknown.as_slice()), (Fit::Differs, ["abi"].as_slice()));
        // A preview target can't be compared with a level.
        assert_eq!(match_avd(p, &Avd { target: Some("android-Baklava".into()), ..avd("Preview") }).unknown, ["api_level"]);
    }

    #[test]
    fn devices_read_from_folders_are_matched() {
        let home = tempfile::tempdir().unwrap();
        let make = |name: &str, config: &str| {
            std::fs::write(home.path().join(format!("{name}.ini")), "target=android-37.0\n").unwrap();
            std::fs::create_dir_all(home.path().join(format!("{name}.avd"))).unwrap();
            std::fs::write(home.path().join(format!("{name}.avd")).join("config.ini"), config).unwrap();
        };
        make("Phone", "abi.type=x86_64\nhw.lcd.width=1080\nhw.lcd.height=2400\nhw.lcd.density=420\nhw.ramSize=2048\n");
        make("Old", "abi.type=x86_64\nhw.lcd.width=720\nhw.lcd.height=1280\nhw.lcd.density=320\nhw.ramSize=2048\n");
        let avds = crate::android_sdk::read_avds(home.path()).devices;
        assert_eq!(pick(profile("phone_medium").unwrap(), &avds).map(|m| m.avd), Some("Phone".to_string()));
        assert_eq!(pick(profile("phone_small").unwrap(), &avds).map(|m| m.avd), Some("Old".to_string()));
        assert!(pick(profile("tablet").unwrap(), &avds).is_none());
    }

    #[test]
    fn the_best_device_is_picked_and_none_is_none() {
        let p = profile("phone_medium").unwrap();
        let avds = vec![Avd { density: Some(160), ..avd("A_wrong_density") }, Avd { abi: None, ..avd("B_unverified") }, avd("C_exact"), avd("D_exact")];
        let ranked = rank(p, &avds);
        assert_eq!(ranked.iter().map(|m| m.avd.as_str()).collect::<Vec<_>>(), ["C_exact", "D_exact", "B_unverified", "A_wrong_density"]);
        assert_eq!(pick(p, &avds).map(|m| m.avd), Some("C_exact".to_string()));
        assert_eq!(pick(p, &avds[..2]).map(|m| (m.avd, m.fit)), Some(("B_unverified".to_string(), Fit::Unverified)));
        assert!(pick(p, &avds[..1]).is_none());
        assert!(pick(p, &[]).is_none());
        // A phone does not fit the tablet profile.
        let tablet = profile("tablet").unwrap();
        assert_eq!(match_avd(tablet, &avd("Phone")).fit, Fit::Differs);
    }
}
