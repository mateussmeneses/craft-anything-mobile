//! Files Android's runtime (ART) makes from an app's DEX code when it installs or optimises it:
//! `.vdex` (the DEX files, or their checksums, plus verification data), `.odex`/`.oat` (an ELF
//! holding machine code compiled from the DEX, with an `oat\n` header in its data) and `.art`
//! (a boot or app image: objects laid out for the runtime to map).
//!
//! What is read:
//! - VDEX 027 (Android 12 and later): its section list, how many DEX files it covers and whether
//!   their bytecode is inside it. VDEX 019 and 021: the fixed header (DEX file count, the sizes of
//!   the verification data, boot class path checksums and class loader context) and the DEX
//!   section's header, when there is one.
//! - OAT: the header fields, by version, for the versions in [`OAT_LAYOUTS`] (instruction set,
//!   its feature bits, the DEX file count, the offsets of the runtime's entry stubs), the
//!   key-value store dex2oat wrote (compiler filter, class path, command line…), and, where the
//!   version's record layout is known, the list of DEX files the code was compiled from (where
//!   each was, its checksum). For other versions only the settings found by key are read.
//! - `.art`: only that it is one, and its version.
//!
//! What isn't: the compiled machine code is not turned back into Java or Kotlin; per-class tables,
//! lookup tables and `.bss` mappings are only given as the offsets the DEX list records.
//!
//! The layouts here were written from the format's facts (field order and sizes) as Android's
//! published runtime sources state them for each release tag, and checked where a real file was
//! at hand; each table entry says which.

use serde::Serialize;

/// Most DEX files listed from one file.
const MAX_DEX: usize = 4096;
/// How far into a file the OAT header is looked for.
const OAT_SEARCH: usize = 64 << 20;
/// Longest key-value text kept.
const MAX_VALUE: usize = 4096;
/// Most key-value pairs kept from an OAT header.
const MAX_SETTINGS: usize = 256;
/// Longest DEX location read from an OAT DEX record (a path).
const MAX_LOCATION: usize = 4096;
/// Most warnings kept from one file.
const MAX_WARNINGS: usize = 64;

/// A VDEX section, as version 027 lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    /// checksums, dex, verifier_deps, type_lookup_table, or unknown(n)
    pub kind: String,
    pub offset: u32,
    pub size: u32,
}

/// The fixed header of a VDEX file before version 027 (019, 021).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VdexOldHeader {
    /// `002` when a DEX section follows the checksums, `000` when none does.
    pub dex_section_version: String,
    pub dex_file_count: u32,
    /// Bytes of verification data.
    pub verifier_deps_size: u32,
    /// Bytes of boot class path checksums (021 only).
    pub boot_class_path_checksums_size: Option<u32>,
    /// Bytes of class loader context text (021 only).
    pub class_loader_context_size: Option<u32>,
    /// The DEX section's own header, when there is a DEX section.
    pub dex_section: Option<VdexDexSection>,
}

/// The header of an old VDEX's DEX section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VdexDexSection {
    pub dex_size: u32,
    pub shared_data_size: u32,
    /// Bytes of quickening data (rewritten bytecode), which Android 12 dropped.
    pub quickening_info_size: u32,
}

/// One header value of an OAT file that has no field of its own here (mostly stub offsets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeaderValue {
    pub name: String,
    pub value: i64,
}

/// An OAT header's fields, read with the layout of its version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OatHeader {
    pub checksum: u32,
    /// `arm`, `arm64`, `thumb2`, `riscv64`, `x86`, `x86_64`, `mips`, `mips64`, `none` or `unknown(n)`.
    pub instruction_set: String,
    /// Feature bits of the instruction set (their meaning depends on it).
    pub instruction_set_features: u32,
    pub dex_file_count: u32,
    /// Where the DEX list starts, from the header's start.
    pub dex_list_offset: u32,
    pub key_value_size: u32,
    /// The other fields, in file order.
    pub other_fields: Vec<HeaderValue>,
    /// How this version's layout was checked.
    pub layout_checked: String,
}

/// One DEX file an OAT file was compiled from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OatDex {
    /// Where the DEX was (an APK path, `!classes2.dex` for a second one).
    pub location: String,
    /// The DEX file's checksum (its header's Adler-32).
    pub checksum: u32,
    /// The DEX format version copied into the record (`039`…), for versions that copy it.
    pub dex_version: Option<String>,
    /// The DEX file's SHA-1 signature in hex, for versions that copy it.
    pub sha1: Option<String>,
    /// Where the DEX bytecode is, from the OAT header's start; 0 when it isn't in this file.
    pub dex_offset: u32,
    pub class_table_offset: u32,
    pub lookup_table_offset: u32,
    pub layout_sections_offset: u32,
    /// Offsets of the `.bss` mappings, in the version's order; 0 is none.
    pub bss_offsets: Vec<u32>,
}

/// What an ART file is and says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtFile {
    Vdex {
        version: String,
        /// Read only for version 027 (Android 12 and later).
        sections: Vec<Section>,
        /// Read only for versions 019 and 021.
        old_header: Option<VdexOldHeader>,
        /// DEX files it covers (one checksum each), when known.
        dex_files: Option<usize>,
        /// DEX files whose bytecode is inside it (found by their header).
        embedded_dex: usize,
        warnings: Vec<String>,
    },
    Oat {
        version: String,
        /// The `oat\n` header's offset in the file.
        header_offset: usize,
        /// The header's fields, when this version's layout is known.
        header: Option<OatHeader>,
        /// Settings dex2oat recorded, by key (`compiler-filter`, `dex2oat-cmdline`, `classpath`…).
        settings: Vec<(String, String)>,
        /// The DEX files it was compiled from, when this version's record layout is known.
        dex_list: Vec<OatDex>,
        /// DEX file headers found anywhere in the file; not proof that whole bytecode is there.
        embedded_dex: usize,
        warnings: Vec<String>,
    },
    Image {
        version: String,
    },
}

/// One field of an OAT header after its magic and version (each four bytes, little-endian).
#[derive(Debug, Clone, Copy)]
enum Field {
    Checksum,
    InstructionSet,
    IsaFeatures,
    DexCount,
    DexListOffset,
    KeyValueSize,
    /// An unsigned value kept by name (an offset).
    Offset(&'static str),
    /// A signed value kept by name.
    Signed(&'static str),
}

/// How one version's DEX records are laid out after the location text.
#[derive(Debug, Clone, Copy)]
struct DexRecord {
    /// The record copies the DEX file's magic (8 bytes) before the checksum and its SHA-1
    /// (20 bytes) after it.
    copies_dex_header: bool,
    /// How many `.bss` mapping offsets follow the four table offsets.
    bss_mappings: usize,
}

/// One OAT header layout and the versions it was checked for.
struct OatLayout {
    versions: &'static [&'static str],
    /// How the layout was checked for those versions.
    checked: &'static str,
    fields: &'static [Field],
    /// The instruction set's numbering, by value.
    isa: &'static [&'static str],
    records: Option<DexRecord>,
}

const ISA_WITH_MIPS: &[&str] = &["none", "arm", "arm64", "thumb2", "x86", "x86_64", "mips", "mips64"];
const ISA_PLAIN: &[&str] = &["none", "arm", "arm64", "thumb2", "x86", "x86_64"];
const ISA_WITH_RISCV: &[&str] = &["none", "arm", "arm64", "thumb2", "riscv64", "x86", "x86_64"];

use Field::{Checksum, DexCount, DexListOffset, InstructionSet, IsaFeatures, KeyValueSize, Offset, Signed};

/// OAT header layouts, by exact version. A version that isn't listed has its header left unread:
/// layouts change between releases, and offsets are never guessed.
const OAT_LAYOUTS: &[OatLayout] = &[
    OatLayout {
        versions: &["138"],
        checked: "documentation only (Android 9 runtime sources)",
        fields: &[
            Checksum,
            InstructionSet,
            IsaFeatures,
            DexCount,
            DexListOffset,
            Offset("code_offset"),
            Offset("interpreter_to_interpreter_stub"),
            Offset("interpreter_to_compiled_stub"),
            Offset("jni_lookup_stub"),
            Offset("generic_jni_stub"),
            Offset("imt_conflict_stub"),
            Offset("resolution_stub"),
            Offset("to_interpreter_stub"),
            Signed("image_patch_delta"),
            Offset("image_oat_checksum"),
            Offset("image_oat_data_begin"),
            KeyValueSize,
        ],
        isa: ISA_WITH_MIPS,
        records: None,
    },
    OatLayout {
        versions: &["170"],
        checked: "documentation only (Android 10 runtime sources)",
        fields: &[
            Checksum,
            InstructionSet,
            IsaFeatures,
            DexCount,
            DexListOffset,
            Offset("code_offset"),
            Offset("jni_lookup_stub"),
            Offset("generic_jni_stub"),
            Offset("imt_conflict_stub"),
            Offset("resolution_stub"),
            Offset("to_interpreter_stub"),
            KeyValueSize,
        ],
        isa: ISA_WITH_MIPS,
        records: None,
    },
    OatLayout {
        versions: &["183"],
        checked: "documentation only (Android 11 runtime sources)",
        fields: &[
            Checksum,
            InstructionSet,
            IsaFeatures,
            DexCount,
            DexListOffset,
            Offset("code_offset"),
            Offset("jni_lookup_stub"),
            Offset("jni_critical_lookup_stub"),
            Offset("generic_jni_stub"),
            Offset("imt_conflict_stub"),
            Offset("resolution_stub"),
            Offset("to_interpreter_stub"),
            KeyValueSize,
        ],
        isa: ISA_PLAIN,
        records: None,
    },
    OatLayout {
        versions: &["195"],
        checked: "documentation only (Android 12 runtime sources)",
        fields: &[
            Checksum,
            InstructionSet,
            IsaFeatures,
            DexCount,
            DexListOffset,
            Offset("code_offset"),
            Offset("jni_lookup_stub"),
            Offset("jni_critical_lookup_stub"),
            Offset("generic_jni_stub"),
            Offset("imt_conflict_stub"),
            Offset("resolution_stub"),
            Offset("to_interpreter_stub"),
            Offset("nterp_stub"),
            KeyValueSize,
        ],
        isa: ISA_PLAIN,
        records: Some(DexRecord { copies_dex_header: false, bss_mappings: 5 }),
    },
    OatLayout {
        versions: &["225"],
        checked: "documentation only (Android 13 runtime sources)",
        fields: BCP_FIELDS,
        isa: ISA_PLAIN,
        records: Some(DexRecord { copies_dex_header: false, bss_mappings: 5 }),
    },
    OatLayout {
        versions: &["230"],
        checked: "documentation only (Android 14 runtime sources)",
        fields: BCP_FIELDS,
        isa: ISA_WITH_RISCV,
        records: Some(DexRecord { copies_dex_header: false, bss_mappings: 5 }),
    },
    OatLayout {
        versions: &["244"],
        checked: "documentation only (Android 15 runtime sources)",
        fields: BCP_FIELDS,
        isa: ISA_WITH_RISCV,
        records: Some(DexRecord { copies_dex_header: true, bss_mappings: 6 }),
    },
    OatLayout {
        versions: &["259"],
        checked: "documentation only (Android 16 runtime sources)",
        fields: BASE_FIELDS,
        isa: ISA_WITH_RISCV,
        records: Some(DexRecord { copies_dex_header: true, bss_mappings: 6 }),
    },
    OatLayout {
        // Checked on a real Android 17 x86_64 emulator's `.odex` (instruction set 6 = x86_64, one
        // DEX record whose checksum matches its VDEX, key-value store ending where the header's
        // size says), with the record's field order from Android 16's sources.
        versions: &["275"],
        checked: "a real file (Android 17 x86_64 emulator) and the Android 16 layout",
        fields: BASE_FIELDS,
        isa: ISA_WITH_RISCV,
        records: Some(DexRecord { copies_dex_header: true, bss_mappings: 6 }),
    },
];

/// Android 13 to 15: a boot class path `.bss` offset after the DEX list's.
const BCP_FIELDS: &[Field] = &[
    Checksum,
    InstructionSet,
    IsaFeatures,
    DexCount,
    DexListOffset,
    Offset("boot_class_path_bss_offset"),
    Offset("code_offset"),
    Offset("jni_lookup_stub"),
    Offset("jni_critical_lookup_stub"),
    Offset("generic_jni_stub"),
    Offset("imt_conflict_stub"),
    Offset("resolution_stub"),
    Offset("to_interpreter_stub"),
    Offset("nterp_stub"),
    KeyValueSize,
];

/// Android 16 and 17: also the offset of the OAT data this one extends.
const BASE_FIELDS: &[Field] = &[
    Checksum,
    InstructionSet,
    IsaFeatures,
    DexCount,
    DexListOffset,
    Offset("boot_class_path_bss_offset"),
    Offset("base_offset"),
    Offset("code_offset"),
    Offset("jni_lookup_stub"),
    Offset("jni_critical_lookup_stub"),
    Offset("generic_jni_stub"),
    Offset("imt_conflict_stub"),
    Offset("resolution_stub"),
    Offset("to_interpreter_stub"),
    Offset("nterp_stub"),
    KeyValueSize,
];

/// OAT versions whose header this reader lays out.
pub fn known_oat_versions() -> Vec<&'static str> {
    OAT_LAYOUTS.iter().flat_map(|l| l.versions.iter().copied()).collect()
}

/// Whether the bytes start like a VDEX file.
pub fn is_vdex(data: &[u8]) -> bool {
    data.starts_with(b"vdex")
}

fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let b: [u8; 4] = data.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// `[magic]NNN\0`: a three-digit version after a four-byte magic.
fn versioned_magic(w: &[u8], magic: &[u8; 4]) -> bool {
    w.starts_with(magic) && w.get(4..7).is_some_and(|v| v.iter().all(u8::is_ascii_digit)) && w.get(7) == Some(&0)
}

/// DEX headers (`dex\n0NN\0`) inside `data`.
fn count_dex(data: &[u8]) -> usize {
    data.windows(8).filter(|w| versioned_magic(w, b"dex\n")).take(MAX_DEX).count()
}

fn version_text(b: &[u8]) -> String {
    b.iter().take_while(|c| **c != 0).map(|c| if c.is_ascii_graphic() { *c as char } else { '?' }).collect()
}

fn warn(warnings: &mut Vec<String>, w: String) {
    if warnings.len() < MAX_WARNINGS {
        warnings.push(w);
    }
}

/// Read a VDEX file.
pub fn parse_vdex(data: &[u8]) -> Option<ArtFile> {
    if !is_vdex(data) {
        return None;
    }
    let version = version_text(data.get(4..8)?);
    let mut sections = Vec::new();
    let mut warnings = Vec::new();
    let mut dex_files = None;
    let mut embedded = 0usize;
    let mut old_header = None;
    if version == "027" {
        let n = u32_at(data, 8).unwrap_or(0) as usize;
        if n > 16 {
            warnings.push(format!("{n} sections listed; read the first 16"));
        }
        for i in 0..n.min(16) {
            let at = 12usize.saturating_add(i.saturating_mul(12));
            let (Some(kind), Some(offset), Some(size)) = (u32_at(data, at), u32_at(data, at + 4), u32_at(data, at + 8)) else {
                warnings.push("the section list ends early".into());
                break;
            };
            let name = match kind {
                0 => "checksums".to_string(),
                1 => "dex".to_string(),
                2 => "verifier_deps".to_string(),
                3 => "type_lookup_table".to_string(),
                k => format!("unknown({k})"),
            };
            let body = (offset as usize).checked_add(size as usize).and_then(|end| data.get(offset as usize..end));
            if body.is_none() && size > 0 {
                warnings.push(format!("the {name} section runs past the end of the file"));
            }
            match (kind, body) {
                (0, Some(_)) => dex_files = Some((size / 4) as usize),
                (1, Some(b)) => embedded = count_dex(b),
                _ => {}
            }
            sections.push(Section { kind: name, offset, size });
        }
    } else if let Some(header_size) = old_vdex_header_size(&version) {
        match parse_old_vdex(data, &version, header_size, &mut warnings) {
            Some((h, e)) => {
                dex_files = Some(h.dex_file_count as usize);
                embedded = e;
                old_header = Some(h);
            }
            None => {
                warnings.push(format!("the VDEX {version} header ends early; only DEX headers were counted"));
                embedded = count_dex(data);
            }
        }
    } else {
        warnings.push(format!("VDEX version {version} isn't one whose layout is read here (019, 021, 027); only DEX headers were counted"));
        embedded = count_dex(data);
    }
    Some(ArtFile::Vdex { version, sections, old_header, dex_files, embedded_dex: embedded, warnings })
}

/// The fixed header's size for VDEX versions before 027. Both from the runtime's sources
/// (documentation only, no real file): 019 is Android 9's, 021 Android 10's and 11's, which
/// added the boot class path checksums' and class loader context's sizes.
fn old_vdex_header_size(version: &str) -> Option<usize> {
    match version {
        "019" => Some(20),
        "021" => Some(28),
        _ => None,
    }
}

/// An old VDEX: magic, version, DEX section version (4 bytes each), then u32 fields; a checksum
/// per DEX file follows, then the DEX section's header when the DEX section version is `002`.
fn parse_old_vdex(data: &[u8], version: &str, header_size: usize, warnings: &mut Vec<String>) -> Option<(VdexOldHeader, usize)> {
    let dex_section_version = version_text(data.get(8..12)?);
    let dex_file_count = u32_at(data, 12)?;
    let verifier_deps_size = u32_at(data, 16)?;
    let (boot_class_path_checksums_size, class_loader_context_size) =
        if version == "021" { (Some(u32_at(data, 20)?), Some(u32_at(data, 24)?)) } else { (None, None) };
    let checksums_end = (dex_file_count as usize).checked_mul(4).and_then(|n| n.checked_add(header_size));
    let mut embedded = 0;
    let mut dex_section = None;
    match checksums_end {
        Some(end) if end <= data.len() => match dex_section_version.as_str() {
            "002" => match (u32_at(data, end), u32_at(data, end.saturating_add(4)), u32_at(data, end.saturating_add(8))) {
                (Some(dex_size), Some(shared_data_size), Some(quickening_info_size)) => {
                    let start = end.saturating_add(12);
                    let stop = start.checked_add(dex_size as usize);
                    match stop.and_then(|s| data.get(start..s)) {
                        Some(b) => embedded = count_dex(b),
                        None => {
                            warn(warnings, "the DEX section runs past the end of the file".into());
                            embedded = count_dex(data.get(start..).unwrap_or_default());
                        }
                    }
                    dex_section = Some(VdexDexSection { dex_size, shared_data_size, quickening_info_size });
                }
                _ => warn(warnings, "the DEX section's header ends early".into()),
            },
            "000" => {}
            other => {
                warn(warnings, format!("DEX section version {other} isn't one read here (000, 002); only DEX headers were counted"));
                embedded = count_dex(data);
            }
        },
        _ => warn(warnings, format!("{dex_file_count} DEX checksums are listed but the file ends before them")),
    }
    let header =
        VdexOldHeader { dex_section_version, dex_file_count, verifier_deps_size, boot_class_path_checksums_size, class_loader_context_size, dex_section };
    Some((header, embedded))
}

/// Read an ODEX/OAT file (an ELF): find its `oat\n` header and read it by version.
pub fn parse_oat(data: &[u8]) -> Option<ArtFile> {
    if !data.starts_with(b"\x7fELF") {
        return None;
    }
    let search = data.get(..OAT_SEARCH.min(data.len()))?;
    let header_offset = search.windows(8).position(|w| versioned_magic(w, b"oat\n"))?;
    let version = version_text(data.get(header_offset.checked_add(4)?..header_offset.checked_add(8)?)?);
    let oat = data.get(header_offset..)?;
    let mut warnings = Vec::new();
    let mut header = None;
    let mut dex_list = Vec::new();
    let settings = match OAT_LAYOUTS.iter().find(|l| l.versions.contains(&version.as_str())) {
        Some(layout) => match read_header(oat, layout) {
            Some((h, kv_start)) => {
                let settings = read_key_values(oat, kv_start, h.key_value_size, &mut warnings);
                if let Some(rec) = layout.records {
                    dex_list = read_dex_list(oat, &h, rec, &mut warnings);
                }
                header = Some(h);
                settings
            }
            None => {
                warn(&mut warnings, format!("the OAT {version} header ends before its fields do; settings were found by key"));
                settings_by_key(oat)
            }
        },
        None => settings_by_key(oat),
    };
    Some(ArtFile::Oat { version, header_offset, header, settings, dex_list, embedded_dex: count_dex(data), warnings })
}

/// The header's fields and where the key-value store starts (both from the header's start).
fn read_header(oat: &[u8], layout: &OatLayout) -> Option<(OatHeader, usize)> {
    let mut h = OatHeader {
        checksum: 0,
        instruction_set: String::new(),
        instruction_set_features: 0,
        dex_file_count: 0,
        dex_list_offset: 0,
        key_value_size: 0,
        other_fields: Vec::new(),
        layout_checked: layout.checked.to_string(),
    };
    let mut at = 8usize;
    for field in layout.fields {
        let v = u32_at(oat, at)?;
        at = at.checked_add(4)?;
        match field {
            Checksum => h.checksum = v,
            InstructionSet => {
                h.instruction_set = layout.isa.get(v as usize).map_or_else(|| format!("unknown({v})"), |s| (*s).to_string());
            }
            IsaFeatures => h.instruction_set_features = v,
            DexCount => h.dex_file_count = v,
            DexListOffset => h.dex_list_offset = v,
            KeyValueSize => h.key_value_size = v,
            Offset(name) => h.other_fields.push(HeaderValue { name: (*name).to_string(), value: i64::from(v) }),
            Signed(name) => h.other_fields.push(HeaderValue { name: (*name).to_string(), value: i64::from(v as i32) }),
        }
    }
    Some((h, at))
}

/// The key-value store: NUL-ended key, NUL-ended value, repeated. Some values are padded with
/// extra NULs to a fixed size, so NULs between a value and the next key are skipped.
fn read_key_values(oat: &[u8], start: usize, size: u32, warnings: &mut Vec<String>) -> Vec<(String, String)> {
    let end = start.checked_add(size as usize);
    let store = match end.and_then(|e| oat.get(start..e)) {
        Some(s) => s,
        None => {
            warn(warnings, format!("the key-value store ({size} bytes) runs past the end of the file; read what is there"));
            oat.get(start..).unwrap_or_default()
        }
    };
    let mut out = Vec::new();
    let mut rest = store;
    loop {
        let skip = rest.iter().take_while(|c| **c == 0).count();
        rest = rest.get(skip..).unwrap_or_default();
        if rest.is_empty() {
            break;
        }
        if out.len() >= MAX_SETTINGS {
            warn(warnings, format!("more than {MAX_SETTINGS} settings; read the first {MAX_SETTINGS}"));
            break;
        }
        let Some(k_end) = rest.iter().position(|c| *c == 0) else {
            warn(warnings, "the key-value store ends inside a key".into());
            break;
        };
        let key = text(rest.get(..k_end).unwrap_or_default(), warnings, "a key");
        rest = rest.get(k_end.saturating_add(1)..).unwrap_or_default();
        let v_end = rest.iter().position(|c| *c == 0).unwrap_or(rest.len());
        let value = text(rest.get(..v_end).unwrap_or_default(), warnings, &key);
        rest = rest.get(v_end.saturating_add(1)..).unwrap_or_default();
        out.push((key, value));
    }
    out
}

fn text(b: &[u8], warnings: &mut Vec<String>, what: &str) -> String {
    if b.len() > MAX_VALUE {
        warn(warnings, format!("the value of {what} is {} bytes; kept the first {MAX_VALUE}", b.len()));
    }
    String::from_utf8_lossy(b.get(..b.len().min(MAX_VALUE)).unwrap_or_default()).into_owned()
}

/// Settings found by searching for known keys, for versions whose header isn't laid out here.
fn settings_by_key(oat: &[u8]) -> Vec<(String, String)> {
    let window = oat.get(..oat.len().min(64 * 1024)).unwrap_or_default();
    let mut settings = Vec::new();
    for key in ["compiler-filter", "dex2oat-cmdline", "classpath", "concurrent-copying", "debuggable", "native-debuggable", "pic", "dex2oat-host"] {
        let pat = [key.as_bytes(), b"\0"].concat();
        if let Some(at) = window.windows(pat.len()).position(|w| w == pat.as_slice()) {
            let rest = window.get(at.saturating_add(pat.len())..).unwrap_or_default();
            let value: String = String::from_utf8_lossy(rest.split(|c| *c == 0).next().unwrap_or_default()).chars().take(MAX_VALUE).collect();
            settings.push((key.to_string(), value));
        }
    }
    settings
}

/// The DEX records, one after another from the header's DEX list offset.
fn read_dex_list(oat: &[u8], h: &OatHeader, rec: DexRecord, warnings: &mut Vec<String>) -> Vec<OatDex> {
    let count = h.dex_file_count as usize;
    if count > MAX_DEX {
        warn(warnings, format!("{count} DEX files are listed; read at most {MAX_DEX}"));
    }
    let mut out = Vec::new();
    let mut at = h.dex_list_offset as usize;
    for i in 0..count.min(MAX_DEX) {
        match read_dex_record(oat, at, rec) {
            Ok((d, next)) => {
                out.push(d);
                at = next;
            }
            Err(why) => {
                warn(warnings, format!("the DEX list stops at record {} of {count}: {why}", i.saturating_add(1)));
                break;
            }
        }
    }
    out
}

fn read_dex_record(oat: &[u8], at: usize, rec: DexRecord) -> Result<(OatDex, usize), String> {
    let short = || "the file ends inside it".to_string();
    let len = u32_at(oat, at).ok_or_else(short)? as usize;
    if len > MAX_LOCATION {
        return Err(format!("its location is said to be {len} bytes long"));
    }
    let mut p = at.checked_add(4).ok_or_else(short)?;
    let loc_end = p.checked_add(len).ok_or_else(short)?;
    let location = String::from_utf8_lossy(oat.get(p..loc_end).ok_or_else(short)?).into_owned();
    p = loc_end;
    let mut dex_version = None;
    if rec.copies_dex_header {
        let magic = oat.get(p..p.checked_add(8).ok_or_else(short)?).ok_or_else(short)?;
        dex_version = Some(if versioned_magic(magic, b"dex\n") { version_text(magic.get(4..8).unwrap_or_default()) } else { "not a DEX magic".to_string() });
        p = p.checked_add(8).ok_or_else(short)?;
    }
    let checksum = u32_at(oat, p).ok_or_else(short)?;
    p = p.checked_add(4).ok_or_else(short)?;
    let mut sha1 = None;
    if rec.copies_dex_header {
        let s = oat.get(p..p.checked_add(20).ok_or_else(short)?).ok_or_else(short)?;
        sha1 = Some(s.iter().map(|b| format!("{b:02x}")).collect());
        p = p.checked_add(20).ok_or_else(short)?;
    }
    let mut word = || -> Result<u32, String> {
        let v = u32_at(oat, p).ok_or_else(short)?;
        p = p.checked_add(4).ok_or_else(short)?;
        Ok(v)
    };
    let dex_offset = word()?;
    let class_table_offset = word()?;
    let lookup_table_offset = word()?;
    let layout_sections_offset = word()?;
    let bss_offsets = (0..rec.bss_mappings).map(|_| word()).collect::<Result<Vec<_>, _>>()?;
    let d = OatDex { location, checksum, dex_version, sha1, dex_offset, class_table_offset, lookup_table_offset, layout_sections_offset, bss_offsets };
    Ok((d, p))
}

/// Read an ART image (`.art`): only its magic and version.
pub fn parse_image(data: &[u8]) -> Option<ArtFile> {
    let head = data.get(..8)?;
    versioned_magic(head, b"art\n").then(|| ArtFile::Image { version: version_text(head.get(4..8).unwrap_or_default()) })
}

/// Read any of the three: VDEX, OAT (ELF) or ART image.
pub fn parse_any(data: &[u8]) -> Option<ArtFile> {
    parse_vdex(data).or_else(|| parse_oat(data)).or_else(|| parse_image(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_testkit::android::{OatDexSpec, oat_file, vdex_old};

    fn vdex027(sections: &[(u32, &[u8])]) -> Vec<u8> {
        let mut head = b"vdex027\0".to_vec();
        head.extend((sections.len() as u32).to_le_bytes());
        let mut off = 12 + 12 * sections.len();
        let mut body = Vec::new();
        for (kind, b) in sections {
            head.extend(kind.to_le_bytes());
            head.extend((off as u32).to_le_bytes());
            head.extend((b.len() as u32).to_le_bytes());
            body.extend_from_slice(b);
            off += b.len();
        }
        [head, body].concat()
    }

    #[test]
    fn a_vdex_says_how_many_dex_files_it_covers_and_holds() {
        let dex = [b"dex\n039\0".as_slice(), &[0; 8], b"dex\n039\0"].concat();
        let v = parse_vdex(&vdex027(&[(0, &[1, 0, 0, 0, 2, 0, 0, 0]), (1, &dex), (2, b"deps")])).unwrap();
        let ArtFile::Vdex { version, sections, dex_files, embedded_dex, warnings, .. } = v else { panic!() };
        assert_eq!((version.as_str(), sections.len(), dex_files, embedded_dex), ("027", 3, Some(2), 2));
        assert!(warnings.is_empty(), "{warnings:?}");
        // Android 12+ usually keeps the bytecode in the APK: an empty dex section.
        let ArtFile::Vdex { embedded_dex, .. } = parse_vdex(&vdex027(&[(0, &[0; 4]), (1, b"")])).unwrap() else { panic!() };
        assert_eq!(embedded_dex, 0);
    }

    #[test]
    fn hostile_or_unknown_vdex_files_are_bounded() {
        let mut bad = b"vdex027\0".to_vec();
        bad.extend(1000u32.to_le_bytes());
        bad.extend([1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        let ArtFile::Vdex { warnings, .. } = parse_vdex(&bad).unwrap() else { panic!() };
        assert!(warnings.len() >= 2, "{warnings:?}");
        let ArtFile::Vdex { warnings, .. } = parse_vdex(b"vdex006\0junk").unwrap() else { panic!() };
        assert!(warnings[0].contains("006"));
        assert!(parse_vdex(b"vde").is_none() && parse_vdex(b"").is_none());
    }

    #[test]
    fn an_old_vdex_header_is_read_by_version() {
        let dex = [b"dex\n035\0".as_slice(), &[0; 24]].concat();
        let ArtFile::Vdex { old_header, dex_files, embedded_dex, warnings, .. } = parse_vdex(&vdex_old("021", "002", &[7, 8], 40, Some(&dex))).unwrap() else {
            panic!()
        };
        let h = old_header.unwrap();
        assert_eq!((dex_files, embedded_dex, h.verifier_deps_size, h.boot_class_path_checksums_size), (Some(2), 1, 40, Some(0)));
        assert_eq!(h.dex_section.unwrap().dex_size, 32);
        assert!(warnings.is_empty(), "{warnings:?}");
        // 019 has no boot class path or class loader sizes; "000" means no DEX section.
        let ArtFile::Vdex { old_header, embedded_dex, .. } = parse_vdex(&vdex_old("019", "000", &[7], 0, None)).unwrap() else { panic!() };
        let h = old_header.unwrap();
        assert_eq!((h.boot_class_path_checksums_size, h.dex_section, embedded_dex), (None, None, 0));
    }

    #[test]
    fn hostile_old_vdex_files_warn_and_never_panic() {
        // A DEX count far larger than the file.
        let mut v = vdex_old("021", "002", &[], 0, None);
        v.splice(12..16, u32::MAX.to_le_bytes());
        let ArtFile::Vdex { warnings, dex_files, .. } = parse_vdex(&v).unwrap() else { panic!() };
        assert_eq!(dex_files, Some(u32::MAX as usize));
        assert!(warnings.iter().any(|w| w.contains("ends before")), "{warnings:?}");
        // A DEX section bigger than the file, and a truncated header.
        let mut v = vdex_old("021", "002", &[1], 0, Some(b"dex\n035\0"));
        v.splice(32..36, u32::MAX.to_le_bytes());
        let ArtFile::Vdex { warnings, .. } = parse_vdex(&v).unwrap() else { panic!() };
        assert!(warnings.iter().any(|w| w.contains("past the end")), "{warnings:?}");
        let ArtFile::Vdex { old_header, warnings, .. } = parse_vdex(b"vdex021\x00002\0\x01").unwrap() else { panic!() };
        assert!(old_header.is_none() && warnings[0].contains("ends early"), "{warnings:?}");
    }

    fn one_dex() -> OatDexSpec {
        OatDexSpec { location: "/data/app/x/base.apk".into(), checksum: 0x4d56_7e4b, dex_offset: 0 }
    }

    #[test]
    fn an_oat_header_is_read_by_version() {
        let elf = oat_file(
            "275",
            6,
            &[one_dex(), OatDexSpec { location: "/data/app/x/base.apk!classes2.dex".into(), ..one_dex() }],
            &[("compiler-filter", "verify"), ("classpath", "PCL[]")],
            64,
        );
        let ArtFile::Oat { version, header, settings, dex_list, warnings, header_offset, .. } = parse_oat(&elf).unwrap() else { panic!() };
        assert_eq!((version.as_str(), header_offset), ("275", 64));
        assert!(warnings.is_empty(), "{warnings:?}");
        let h = header.unwrap();
        assert_eq!((h.instruction_set.as_str(), h.dex_file_count), ("x86_64", 2));
        assert!(h.layout_checked.contains("real file"));
        assert!(h.other_fields.iter().any(|f| f.name == "base_offset"));
        assert!(settings.contains(&("compiler-filter".into(), "verify".into())));
        assert_eq!(dex_list.len(), 2);
        assert_eq!((dex_list[1].location.as_str(), dex_list[1].checksum), ("/data/app/x/base.apk!classes2.dex", 0x4d56_7e4b));
        assert_eq!((dex_list[0].dex_version.as_deref(), dex_list[0].bss_offsets.len()), (Some("039"), 6));
        // An older layout: no copy of the DEX header, five .bss mappings, a different ISA numbering.
        let ArtFile::Oat { header, dex_list, warnings, .. } = parse_oat(&oat_file("225", 5, &[one_dex()], &[("compiler-filter", "speed")], 128)).unwrap()
        else {
            panic!()
        };
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(header.unwrap().instruction_set, "x86_64");
        assert_eq!((dex_list[0].dex_version.as_ref(), dex_list[0].bss_offsets.len()), (None, 5));
        // A header-only layout (no DEX list read) and a padded value.
        let ArtFile::Oat { header, dex_list, settings, .. } =
            parse_oat(&oat_file("170", 6, &[one_dex()], &[("apex-versions", "/\0\0\0\0"), ("pic", "false")], 0)).unwrap()
        else {
            panic!()
        };
        assert_eq!((header.unwrap().instruction_set.as_str(), dex_list.len()), ("mips", 0));
        assert!(settings.contains(&("pic".into(), "false".into())), "{settings:?}");
    }

    #[test]
    fn an_oat_version_outside_the_table_is_not_laid_out() {
        let mut elf = b"\x7fELF".to_vec();
        elf.extend([0; 60]);
        elf.extend(b"oat\n238\0");
        elf.extend([0; 40]);
        elf.extend(b"classpath\0PCL[]\0compiler-filter\0speed-profile\0");
        let ArtFile::Oat { version, settings, header, dex_list, header_offset, .. } = parse_oat(&elf).unwrap() else { panic!() };
        assert_eq!((version.as_str(), header_offset, header, dex_list.len()), ("238", 64, None, 0));
        assert!(settings.contains(&("compiler-filter".into(), "speed-profile".into())));
        assert!(settings.contains(&("classpath".into(), "PCL[]".into())));
        assert!(parse_oat(b"\x7fELF no header").is_none());
        assert!(parse_oat(b"oat\n238\0").is_none());
        assert!(!known_oat_versions().contains(&"238"));
    }

    #[test]
    fn hostile_oat_files_warn_and_never_panic() {
        let base = oat_file("275", 6, &[one_dex()], &[("compiler-filter", "verify")], 0);
        let at = |field: usize| 8 + 4 * field; // from the header start (offset 0 here, after the ELF magic)
        let h0 = base.windows(4).position(|w| w == b"oat\n").unwrap();
        // A truncated header.
        let ArtFile::Oat { header, warnings, .. } = parse_oat(&base[..h0 + 20]).unwrap() else { panic!() };
        assert!(header.is_none() && warnings[0].contains("ends before"), "{warnings:?}");
        // A DEX count far larger than the file.
        let mut v = base.clone();
        v.splice(h0 + at(3)..h0 + at(3) + 4, u32::MAX.to_le_bytes());
        let ArtFile::Oat { dex_list, warnings, .. } = parse_oat(&v).unwrap() else { panic!() };
        assert_eq!(dex_list.len(), 1);
        assert!(warnings.iter().any(|w| w.contains("read at most")) && warnings.iter().any(|w| w.contains("stops at record 2")), "{warnings:?}");
        // A DEX list offset that wraps, and one past the end.
        for off in [u32::MAX, u32::MAX - 2, 1 << 30] {
            let mut v = base.clone();
            v.splice(h0 + at(4)..h0 + at(4) + 4, off.to_le_bytes());
            let ArtFile::Oat { dex_list, warnings, .. } = parse_oat(&v).unwrap() else { panic!() };
            assert!(dex_list.is_empty() && warnings.iter().any(|w| w.contains("stops at record 1")), "{warnings:?}");
        }
        // A location length past the end, and one absurdly long.
        let list = u32::from_le_bytes(base[h0 + at(4)..h0 + at(4) + 4].try_into().unwrap()) as usize;
        for len in [5000u32, u32::MAX, 900] {
            let mut v = base.clone();
            v.truncate(h0 + list + 600);
            v.splice(h0 + list..h0 + list + 4, len.to_le_bytes());
            let ArtFile::Oat { dex_list, warnings, .. } = parse_oat(&v).unwrap() else { panic!() };
            assert!(dex_list.is_empty() && !warnings.is_empty(), "{warnings:?}");
        }
        // A key-value size past the end.
        let mut v = base.clone();
        v.splice(h0 + at(15)..h0 + at(15) + 4, u32::MAX.to_le_bytes());
        let ArtFile::Oat { settings, warnings, .. } = parse_oat(&v).unwrap() else { panic!() };
        assert!(settings.contains(&("compiler-filter".into(), "verify".into())));
        assert!(warnings.iter().any(|w| w.contains("key-value store")), "{warnings:?}");
        // An instruction set number outside the table.
        let mut v = base.clone();
        v.splice(h0 + at(1)..h0 + at(1) + 4, 99u32.to_le_bytes());
        let ArtFile::Oat { header, .. } = parse_oat(&v).unwrap() else { panic!() };
        assert_eq!(header.unwrap().instruction_set, "unknown(99)");
    }

    #[test]
    fn an_art_image_is_named_and_others_are_not() {
        assert_eq!(parse_image(b"art\n108\0rest"), Some(ArtFile::Image { version: "108".into() }));
        assert_eq!(parse_any(b"art\n108\0"), Some(ArtFile::Image { version: "108".into() }));
        assert!(parse_image(b"art\n").is_none() && parse_image(b"art\nabc\0").is_none() && parse_any(b"hello world").is_none());
    }
}
