//! Which Android device Play and the Android checks use, and the default device this computer
//! keeps for them.
//!
//! [`choose`] (Play) and [`choose_for_check`] (the builder's checks) are pure functions over what
//! adb and the SDK saw ([`Seen`]), so every client gets the same answer from the same facts. The
//! default lives in `<config_dir>/android-devices.json`, owned by this module: a serial or an AVD
//! name only means something on one computer, so it is never kept in a project (`craft.json`
//! travels with the folder).

use craft_anything_engines::adb::Device;
use craft_anything_engines::emulator;
use craft_anything_evidence::{ErrorCode, ToolError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The devices file's name in the config folder.
pub const FILE: &str = "android-devices.json";
/// The devices file's current version.
pub const DEVICES_VERSION: u32 = 1;
/// The oldest version this build reads.
const FIRST_VERSION: u32 = 1;
/// Steps that bring an older file forward (none yet: version 1 is the first).
const STEPS: &[crate::versioned::Step<Value>] = &[];
/// Largest devices file read (it holds one small record).
const MAX_FILE: u64 = 64 * 1024;
/// How many unreadable files are kept beside it.
const BROKEN_KEPT: u32 = 16;

/// A device as a person names it: an adb serial (a phone, `R58M…`, or `host:port`) or a virtual
/// device's (AVD's) name. An emulator is kept by name, since `emulator-5554` changes from boot to boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRef {
    Serial(String),
    Avd(String),
}

impl DeviceRef {
    /// The text it names.
    pub fn text(&self) -> &str {
        match self {
            DeviceRef::Serial(s) | DeviceRef::Avd(s) => s,
        }
    }

    /// Read what a person typed, by what it matches in `seen`: a listed serial, else a known or
    /// running AVD, else an emulator serial or a phone serial. Refused when it can be neither a
    /// serial nor an AVD name, so nothing odd reaches adb.
    pub fn parse(text: &str, seen: &Seen) -> Result<DeviceRef, ToolError> {
        let serial_ok = craft_anything_engines::adb::validate_serial(text).is_ok();
        let avd_ok = emulator::validate_avd_name(text).is_ok();
        if !serial_ok && !avd_ok {
            return Err(ToolError::invalid(format!("{text:?} is neither a device serial nor a virtual device name."))
                .with_remediation("Use a serial adb_devices_find lists or a name adb_emulators_list shows."));
        }
        let text = text.to_string();
        Ok(if seen.devices.iter().any(|d| d.serial == text) || (serial_ok && emulator::is_emulator_serial(&text)) {
            DeviceRef::Serial(text)
        } else if avd_ok && (seen.avds.contains(&text) || seen.running_avds.iter().any(|(_, a)| *a == text)) {
            DeviceRef::Avd(text)
        } else if serial_ok {
            DeviceRef::Serial(text)
        } else {
            DeviceRef::Avd(text)
        })
    }
}

/// What adb and the SDK saw: every attached device (any state), the emulators running and the
/// virtual device each runs, and the virtual devices this computer has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    pub devices: Vec<Device>,
    /// (serial, AVD name) of each running emulator whose console said its name.
    pub running_avds: Vec<(String, String)>,
    /// The virtual devices that can be started, in the order the SDK lists them.
    pub avds: Vec<String>,
}

impl Seen {
    fn avd_of(&self, serial: &str) -> Option<String> {
        self.running_avds.iter().find(|(s, _)| s == serial).map(|(_, a)| a.clone())
    }

    fn online(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|d| d.is_online())
    }

    fn candidate(&self, d: &Device) -> Candidate {
        Candidate { serial: d.serial.clone(), model: d.model.clone(), emulator: emulator::is_emulator_serial(&d.serial), avd: self.avd_of(&d.serial) }
    }
}

/// Why a device was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// The caller named it.
    Asked,
    /// It is this computer's default device.
    Default,
    /// It was the only one online.
    OnlyOne,
    /// Nothing was online, so a virtual device is started.
    Started,
    /// A check, with several online and no default: the first emulator.
    FirstEmulator,
    /// A check, with several online, no default and no emulator: the first one.
    FirstOnline,
}

/// One device a person can pick from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    pub serial: String,
    pub model: Option<String>,
    pub emulator: bool,
    /// The virtual device it runs, for an emulator whose console said.
    pub avd: Option<String>,
}

/// Why no device can be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NoDevice {
    /// Nothing is online and this computer has no virtual device to start.
    NoAvd,
    /// The device asked for isn't connected or known; `connected` lists what is.
    AskedMissing { name: String, connected: Vec<String> },
    /// The only device hasn't authorised this computer.
    Unauthorized { serial: String },
    /// The device is attached but not ready (offline, booting, recovery…).
    NotReady { serial: String, state: String },
}

impl NoDevice {
    /// The error a tool returns for it, with a remediation the person can act on.
    pub fn error(&self) -> ToolError {
        let details = serde_json::to_value(self).unwrap_or(Value::Null);
        match self {
            NoDevice::NoAvd => ToolError::new(ErrorCode::NotFound, "No phone or emulator is connected, and this computer has no virtual device to start.")
                .with_remediation("Make one in Android Studio's Device Manager, or connect a phone with USB debugging on."),
            NoDevice::AskedMissing { name, connected } => {
                let have = if connected.is_empty() { "Nothing is connected.".to_string() } else { format!("Connected: {}.", connected.join(", ")) };
                ToolError::new(ErrorCode::NotFound, format!("No device or virtual device named {name} was found. {have}"))
                    .with_remediation("Use a serial adb_devices_find lists or a name adb_emulators_list shows.")
            }
            NoDevice::Unauthorized { serial } => ToolError::new(ErrorCode::NoPermission, format!("Device {serial} has not authorised this computer."))
                .with_remediation("On the phone, accept the \"Allow USB debugging\" prompt, then try again."),
            NoDevice::NotReady { serial, state } => ToolError::new(ErrorCode::NotFound, format!("Device {serial} is {state}, not ready."))
                .with_remediation("Wait for it to come online (unlock it / reconnect it), then try again."),
        }
        .with_details(details)
    }
}

/// What to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Use the online device `serial` (`avd` when it is an emulator whose name is known).
    Use { serial: String, avd: Option<String>, how: How },
    /// Nothing usable is online: start this virtual device.
    Start { avd: String, how: How },
    /// Several are online and nothing says which: the person picks.
    Choose { candidates: Vec<Candidate> },
    /// No device can be used.
    None { why: NoDevice },
}

/// A [`Choice`], and whether the default device was set but couldn't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    pub choice: Choice,
    pub default_missing: bool,
}

/// Resolve a named device against what was seen: `Some` when it settles the choice.
fn resolve(seen: &Seen, r: &DeviceRef, how: How) -> Result<Choice, NoDevice> {
    let text = r.text();
    // A name may be either: a serial adb lists wins, then a running or known AVD.
    let by_serial = seen.devices.iter().find(|d| d.serial == text);
    let running_as = seen.running_avds.iter().find(|(_, a)| a == text).map(|(s, _)| s.clone());
    let device = match (r, by_serial, &running_as) {
        (DeviceRef::Serial(_), Some(d), _) => Some(d),
        (_, _, Some(serial)) => seen.devices.iter().find(|d| d.serial == *serial),
        (DeviceRef::Avd(_), Some(d), None) => Some(d),
        _ => None,
    };
    if let Some(d) = device {
        return match d.state.as_str() {
            "device" => Ok(Choice::Use { serial: d.serial.clone(), avd: seen.avd_of(&d.serial), how }),
            "unauthorized" => Err(NoDevice::Unauthorized { serial: d.serial.clone() }),
            state => Err(NoDevice::NotReady { serial: d.serial.clone(), state: state.to_string() }),
        };
    }
    if seen.avds.iter().any(|a| a == text) {
        return Ok(Choice::Start { avd: text.to_string(), how });
    }
    Err(NoDevice::AskedMissing { name: text.to_string(), connected: seen.devices.iter().map(|d| d.serial.clone()).collect() })
}

/// When nothing is online: start the first virtual device, else say why there is none.
fn nothing_online(seen: &Seen) -> Choice {
    if let Some(avd) = seen.avds.first() {
        return Choice::Start { avd: avd.clone(), how: How::Started };
    }
    let why = if let Some(d) = seen.devices.iter().find(|d| d.state == "unauthorized") {
        NoDevice::Unauthorized { serial: d.serial.clone() }
    } else if let Some(d) = seen.devices.first() {
        NoDevice::NotReady { serial: d.serial.clone(), state: d.state.clone() }
    } else {
        NoDevice::NoAvd
    };
    Choice::None { why }
}

/// Play's rule: the asked device, then the default, then the only one online; none online starts
/// the first virtual device; several online with nothing named is [`Choice::Choose`]. A default
/// that can't be used is passed over (`default_missing`); an asked one that can't is the answer.
/// An unauthorised or offline device is never picked.
pub fn choose(seen: &Seen, asked: Option<&DeviceRef>, default: Option<&DeviceRef>) -> Chosen {
    if let Some(r) = asked {
        let choice = resolve(seen, r, How::Asked).unwrap_or_else(|why| Choice::None { why });
        return Chosen { choice, default_missing: false };
    }
    let mut default_missing = false;
    if let Some(r) = default {
        match resolve(seen, r, How::Default) {
            Ok(choice) => return Chosen { choice, default_missing: false },
            Err(_) => default_missing = true,
        }
    }
    let online: Vec<&Device> = seen.online().collect();
    let choice = match online.as_slice() {
        [] => nothing_online(seen),
        [one] => Choice::Use { serial: one.serial.clone(), avd: seen.avd_of(&one.serial), how: How::OnlyOne },
        many => Choice::Choose { candidates: many.iter().map(|d| seen.candidate(d)).collect() },
    };
    Chosen { choice, default_missing }
}

/// A check's rule: it never stops a build to ask. Play's rule with nothing asked, and several
/// online means the first emulator (a build doesn't install on the person's phone when an
/// emulator is there), else the first online.
pub fn choose_for_check(seen: &Seen, default: Option<&DeviceRef>) -> Chosen {
    let mut chosen = choose(seen, None, default);
    if let Choice::Choose { candidates } = &chosen.choice {
        let pick = candidates.iter().find(|c| c.emulator).map(|c| (c, How::FirstEmulator)).or_else(|| candidates.first().map(|c| (c, How::FirstOnline)));
        if let Some((c, how)) = pick {
            chosen.choice = Choice::Use { serial: c.serial.clone(), avd: c.avd.clone(), how };
        }
    }
    chosen
}

// ------------------------------------------------------------------------------------------------
// The devices file

/// What `android-devices.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DevicesFile {
    pub version: u32,
    /// The device Play and the checks use when none is named.
    #[serde(default)]
    pub default: Option<DeviceRef>,
}

/// The JSON schema of `android-devices.json` (`docs/schemas/android-devices.schema.json`).
pub fn devices_schema() -> String {
    let schema = schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<DevicesFile>();
    serde_json::to_string_pretty(&schema).map(|s| s + "\n").unwrap_or_default()
}

/// How the file on disk could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// Absent, or read and brought to the current version.
    Current,
    /// Written by a newer build: read as far as its fields are known, never written over.
    Newer(u32),
    /// Unreadable (not JSON, not a devices file, too large): no default is read from it, and a
    /// save first moves it aside.
    Broken(String),
}

/// The file's contents, and how it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub file: DevicesFile,
    pub state: FileState,
}

/// Where the devices file lives for a config folder.
pub fn path_in(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE)
}

/// Read the devices file. A missing file is no default; an unreadable one is reported, not
/// treated as empty.
pub fn load(path: &Path) -> Loaded {
    let empty = DevicesFile { version: DEVICES_VERSION, default: None };
    let bytes = match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Loaded { file: empty, state: FileState::Current },
        Err(e) => return Loaded { file: empty, state: FileState::Broken(format!("it can't be read: {e}")) },
        Ok(m) if m.len() > MAX_FILE => return Loaded { file: empty, state: FileState::Broken(format!("it is larger than {MAX_FILE} bytes")) },
        Ok(_) => match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return Loaded { file: empty, state: FileState::Broken(format!("it can't be read: {e}")) },
        },
    };
    let mut doc: Value = match serde_json::from_slice(&bytes) {
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => return Loaded { file: empty, state: FileState::Broken("it isn't a JSON object".into()) },
        Err(e) => return Loaded { file: empty, state: FileState::Broken(format!("it isn't valid JSON: {e}")) },
    };
    let Some(version) = doc.get("version").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()) else {
        return Loaded { file: empty, state: FileState::Broken("it has no version".into()) };
    };
    if version > DEVICES_VERSION {
        // Read as far as its fields are known; keep it as it is.
        let default = doc.get("default").cloned().and_then(|d| serde_json::from_value(d).ok());
        return Loaded { file: DevicesFile { version, default }, state: FileState::Newer(version) };
    }
    if let Err(e) = crate::versioned::upgrade(&mut doc, version, FIRST_VERSION, STEPS) {
        return Loaded { file: empty, state: FileState::Broken(e) };
    }
    match serde_json::from_value::<DevicesFile>(doc) {
        Ok(mut file) => {
            file.version = DEVICES_VERSION;
            Loaded { file, state: FileState::Current }
        }
        Err(e) => Loaded { file: empty, state: FileState::Broken(format!("its fields can't be read: {e}")) },
    }
}

/// Move an unreadable devices file aside (`android-devices.json.broken`, then `.broken.1`…).
fn set_aside(path: &Path) -> Result<PathBuf, ToolError> {
    for n in 0..BROKEN_KEPT {
        let to = path.with_extension(if n == 0 { "json.broken".to_string() } else { format!("json.broken.{n}") });
        match std::fs::symlink_metadata(&to) {
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ToolError::io(to.display(), e)),
        }
        std::fs::rename(path, &to).map_err(|e| ToolError::io(path.display(), e))?;
        return Ok(to);
    }
    Err(ToolError::new(ErrorCode::FileError, format!("{BROKEN_KEPT} unreadable copies of {} are already kept; remove some.", path.display())))
}

/// What [`save_default`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// Where an unreadable earlier file was moved, if one was.
    pub set_aside: Option<PathBuf>,
}

/// Set (or with `None` clear) this computer's default device. A file from a newer build is never
/// written over; an unreadable one is moved aside first, never overwritten.
pub fn save_default(path: &Path, default: Option<DeviceRef>) -> Result<Saved, ToolError> {
    let loaded = load(path);
    let mut aside = None;
    match &loaded.state {
        FileState::Current => {}
        FileState::Newer(v) => {
            return Err(ToolError::new(ErrorCode::NotSupported, format!("{} was written by a newer Craft Anything (version {v}).", path.display()))
                .with_remediation("Update Craft Anything to change the default device."));
        }
        FileState::Broken(_) => aside = Some(set_aside(path)?),
    }
    let file = DevicesFile { version: DEVICES_VERSION, default };
    let mut bytes = serde_json::to_vec_pretty(&file).map_err(|e| ToolError::new(ErrorCode::Internal, format!("the devices file couldn't be written: {e}")))?;
    bytes.push(b'\n');
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ToolError::io(dir.display(), e))?;
    }
    craft_anything_platform::fs::write_atomic(path, &bytes)?;
    Ok(Saved { set_aside: aside })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(serial: &str, state: &str, model: &str) -> Device {
        Device { serial: serial.into(), state: state.into(), product: None, model: Some(model.into()), device: None, transport_id: None }
    }
    fn serial(s: &str) -> DeviceRef {
        DeviceRef::Serial(s.into())
    }
    fn avd(s: &str) -> DeviceRef {
        DeviceRef::Avd(s.into())
    }
    fn seen(devices: Vec<Device>, running: &[(&str, &str)], avds: &[&str]) -> Seen {
        Seen {
            devices,
            running_avds: running.iter().map(|(s, a)| (s.to_string(), a.to_string())).collect(),
            avds: avds.iter().map(|a| a.to_string()).collect(),
        }
    }
    fn uses(c: &Chosen) -> Option<(&str, How)> {
        match &c.choice {
            Choice::Use { serial, how, .. } => Some((serial.as_str(), *how)),
            _ => None,
        }
    }
    fn starts(c: &Chosen) -> Option<(&str, How)> {
        match &c.choice {
            Choice::Start { avd, how } => Some((avd.as_str(), *how)),
            _ => None,
        }
    }
    fn none(c: &Chosen) -> Option<&NoDevice> {
        match &c.choice {
            Choice::None { why } => Some(why),
            _ => None,
        }
    }

    const PHONE: &str = "R58M12345";
    const EMU: &str = "emulator-5554";

    #[test]
    fn zero_online_starts_the_first_virtual_device_or_says_there_is_none() {
        let s = seen(vec![], &[], &["Medium_Phone", "Tablet"]);
        assert_eq!(starts(&choose(&s, None, None)), Some(("Medium_Phone", How::Started)));
        assert_eq!(starts(&choose_for_check(&s, None)), Some(("Medium_Phone", How::Started)));
        let s = seen(vec![], &[], &[]);
        assert_eq!(none(&choose(&s, None, None)), Some(&NoDevice::NoAvd));
        assert_eq!(NoDevice::NoAvd.error().code, ErrorCode::NotFound);
        assert!(NoDevice::NoAvd.error().remediation.unwrap_or_default().contains("Device Manager"));
    }

    #[test]
    fn one_online_is_used_whatever_it_is() {
        for (d, is_emu) in [(dev(PHONE, "device", "Pixel_8"), false), (dev(EMU, "device", "sdk_gphone"), true)] {
            let s = seen(vec![d.clone(), dev("other", "offline", "x")], if is_emu { &[(EMU, "Medium_Phone")] } else { &[] }, &["Medium_Phone"]);
            let c = choose(&s, None, None);
            assert_eq!(uses(&c), Some((d.serial.as_str(), How::OnlyOne)), "{d:?}");
            if let Choice::Use { avd: a, .. } = &c.choice {
                assert_eq!(a.as_deref(), is_emu.then_some("Medium_Phone"));
            }
            // Play may use a lone phone; so may a check (it is the only device).
            assert_eq!(uses(&choose_for_check(&s, None)).map(|u| u.0), Some(d.serial.as_str()));
        }
    }

    #[test]
    fn several_online_asks_for_play_and_prefers_an_emulator_in_a_check() {
        let s = seen(vec![dev(PHONE, "device", "Pixel_8"), dev(EMU, "device", "sdk"), dev("emulator-5556", "device", "sdk")], &[(EMU, "Medium_Phone")], &[]);
        let Choice::Choose { candidates } = choose(&s, None, None).choice else { panic!("several: the person picks") };
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0], Candidate { serial: PHONE.into(), model: Some("Pixel_8".into()), emulator: false, avd: None });
        assert_eq!(candidates[1].avd.as_deref(), Some("Medium_Phone"));
        assert!(candidates[1].emulator);
        assert_eq!(uses(&choose_for_check(&s, None)), Some((EMU, How::FirstEmulator)), "a check passes over the person's phone");
        // Two phones and no emulator: the first online.
        let s = seen(vec![dev("offline1", "offline", "x"), dev(PHONE, "device", "a"), dev("R2", "device", "b")], &[], &["Medium_Phone"]);
        assert_eq!(uses(&choose_for_check(&s, None)), Some((PHONE, How::FirstOnline)));
    }

    #[test]
    fn a_default_that_is_online_wins_and_one_that_is_missing_is_passed_over() {
        let s = seen(vec![dev(PHONE, "device", "a"), dev(EMU, "device", "b")], &[(EMU, "Medium_Phone")], &["Medium_Phone", "Tablet"]);
        for check in [false, true] {
            let pick = |d: &DeviceRef| if check { choose_for_check(&s, Some(d)) } else { choose(&s, None, Some(d)) };
            assert_eq!(uses(&pick(&serial(PHONE))), Some((PHONE, How::Default)), "check={check}");
            // An emulator kept by name is found under whatever serial it booted as.
            assert_eq!(uses(&pick(&avd("Medium_Phone"))), Some((EMU, How::Default)));
            // A default AVD that isn't running is started.
            assert_eq!(starts(&pick(&avd("Tablet"))), Some(("Tablet", How::Default)));
            let missing = pick(&serial("GONE"));
            assert!(missing.default_missing, "check={check}");
        }
        // Play: missing default and several online still asks; a check picks the emulator.
        assert!(matches!(choose(&s, None, Some(&serial("GONE"))).choice, Choice::Choose { .. }));
        assert_eq!(uses(&choose_for_check(&s, Some(&serial("GONE")))), Some((EMU, How::FirstEmulator)));
        // A default that is offline or unauthorised is passed over too.
        let s = seen(vec![dev(PHONE, "unauthorized", "a"), dev(EMU, "device", "b")], &[], &[]);
        let c = choose(&s, None, Some(&serial(PHONE)));
        assert_eq!((uses(&c), c.default_missing), (Some((EMU, How::OnlyOne)), true));
        // The same emulator under a new serial after a reboot.
        let s = seen(vec![dev("emulator-5556", "device", "b")], &[("emulator-5556", "Medium_Phone")], &["Medium_Phone"]);
        assert_eq!(uses(&choose(&s, None, Some(&avd("Medium_Phone")))), Some(("emulator-5556", How::Default)));
    }

    #[test]
    fn an_asked_device_is_used_started_or_is_the_error() {
        let s = seen(
            vec![dev(PHONE, "device", "a"), dev(EMU, "device", "b"), dev("R3", "unauthorized", "c"), dev("R4", "offline", "d")],
            &[(EMU, "Medium_Phone")],
            &["Medium_Phone", "Tablet"],
        );
        assert_eq!(uses(&choose(&s, Some(&serial(EMU)), Some(&serial(PHONE)))), Some((EMU, How::Asked)), "asked beats the default");
        assert_eq!(uses(&choose(&s, Some(&avd("Medium_Phone")), None)), Some((EMU, How::Asked)));
        assert_eq!(starts(&choose(&s, Some(&avd("Tablet")), None)), Some(("Tablet", How::Asked)));
        assert_eq!(none(&choose(&s, Some(&serial("R3")), None)), Some(&NoDevice::Unauthorized { serial: "R3".into() }));
        assert_eq!(none(&choose(&s, Some(&serial("R4")), None)), Some(&NoDevice::NotReady { serial: "R4".into(), state: "offline".into() }));
        let Some(NoDevice::AskedMissing { name, connected }) = none(&choose(&s, Some(&serial("NOPE")), None)).cloned() else { panic!() };
        assert_eq!((name.as_str(), connected.len()), ("NOPE", 4));
        // A serial stored as an AVD name (or the other way round) still matches.
        assert_eq!(uses(&choose(&s, Some(&avd(PHONE)), None)), Some((PHONE, How::Asked)));
        assert_eq!(starts(&choose(&s, Some(&serial("Tablet")), None)), Some(("Tablet", How::Asked)));
    }

    #[test]
    fn an_unauthorised_or_offline_device_is_never_picked() {
        let s = seen(vec![dev(PHONE, "unauthorized", "a")], &[], &[]);
        let c = choose(&s, None, None);
        assert_eq!(none(&c), Some(&NoDevice::Unauthorized { serial: PHONE.into() }));
        let e = none(&c).map(NoDevice::error).unwrap();
        assert_eq!(e.code, ErrorCode::NoPermission);
        assert_eq!(e.details.as_ref().and_then(|d| d["reason"].as_str()), Some("unauthorized"));
        let s = seen(vec![dev(PHONE, "offline", "a")], &[], &[]);
        assert!(matches!(none(&choose(&s, None, None)), Some(NoDevice::NotReady { .. })));
        // With a virtual device to start, it is started instead.
        let s = seen(vec![dev(PHONE, "unauthorized", "a")], &[], &["Medium_Phone"]);
        assert_eq!(starts(&choose_for_check(&s, None)), Some(("Medium_Phone", How::Started)));
    }

    #[test]
    fn what_a_person_types_is_read_by_what_it_matches() {
        let s = seen(vec![dev(PHONE, "device", "a")], &[(EMU, "Medium_Phone")], &["Tablet"]);
        assert_eq!(DeviceRef::parse(PHONE, &s).unwrap(), serial(PHONE));
        assert_eq!(DeviceRef::parse(EMU, &s).unwrap(), serial(EMU));
        assert_eq!(DeviceRef::parse("Medium_Phone", &s).unwrap(), avd("Medium_Phone"));
        assert_eq!(DeviceRef::parse("Tablet", &s).unwrap(), avd("Tablet"));
        assert_eq!(DeviceRef::parse("192.168.0.5:5555", &s).unwrap(), serial("192.168.0.5:5555"));
        for bad in ["", "a b", "a;rm", "-no-window x"] {
            assert_eq!(DeviceRef::parse(bad, &s).unwrap_err().code, ErrorCode::BadInput, "{bad:?}");
        }
    }

    #[test]
    fn the_devices_file_round_trips_and_clears() {
        let d = tempfile::tempdir().unwrap();
        let path = path_in(d.path());
        assert_eq!(load(&path), Loaded { file: DevicesFile { version: 1, default: None }, state: FileState::Current }, "absent: no default");
        save_default(&path, Some(avd("Medium_Phone"))).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""version": 1"#) && text.contains(r#""avd": "Medium_Phone""#), "{text}");
        assert_eq!(load(&path).file.default, Some(avd("Medium_Phone")));
        save_default(&path, Some(serial(PHONE))).unwrap();
        assert_eq!(load(&path).file.default, Some(serial(PHONE)));
        save_default(&path, None).unwrap();
        assert_eq!(load(&path), Loaded { file: DevicesFile { version: 1, default: None }, state: FileState::Current });
    }

    #[test]
    fn a_newer_file_is_read_and_never_written_over() {
        let d = tempfile::tempdir().unwrap();
        let path = path_in(d.path());
        let newer = r#"{"version": 9, "default": {"serial": "R58M12345"}, "profiles": [1]}"#;
        std::fs::write(&path, newer).unwrap();
        let l = load(&path);
        assert_eq!((l.state, l.file.default), (FileState::Newer(9), Some(serial(PHONE))));
        let err = save_default(&path, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotSupported);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer, "kept as it was");
    }

    #[test]
    fn a_broken_file_gives_no_default_and_is_set_aside_before_a_save() {
        for broken in
            [&b"not json"[..], b"[1]", br#"{"default": {"avd": "X"}}"#, br#"{"version": 1, "default": {"what": 1}}"#, br#"{"version": 0}"#, &[0xff, 0xfe]]
        {
            let d = tempfile::tempdir().unwrap();
            let path = path_in(d.path());
            std::fs::write(&path, broken).unwrap();
            let l = load(&path);
            assert!(matches!(l.state, FileState::Broken(_)), "{:?}", String::from_utf8_lossy(broken));
            assert_eq!(l.file.default, None);
            let saved = save_default(&path, Some(avd("Medium_Phone"))).unwrap();
            let aside = saved.set_aside.unwrap();
            assert_eq!(std::fs::read(&aside).unwrap(), broken, "every byte kept");
            assert_eq!(load(&path).file.default, Some(avd("Medium_Phone")));
        }
        // A second broken file goes beside the first, never over it.
        let d = tempfile::tempdir().unwrap();
        let path = path_in(d.path());
        for n in 0..2 {
            std::fs::write(&path, format!("broken {n}")).unwrap();
            save_default(&path, None).unwrap();
        }
        assert_eq!(std::fs::read_to_string(path.with_extension("json.broken")).unwrap(), "broken 0");
        assert_eq!(std::fs::read_to_string(path.with_extension("json.broken.1")).unwrap(), "broken 1");
    }
}
