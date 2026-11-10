//! Files Android's runtime (ART) makes from an app's DEX code when it installs or optimises it:
//! `.vdex` (the DEX files, or their checksums, plus verification data) and `.odex`/`.oat` (an ELF
//! holding machine code compiled from the DEX, with an `oat\n` header in its data).
//!
//! What is read: the kind, the format version, a VDEX's sections (how many DEX files it covers and
//! whether their bytecode is inside it) and an OAT's compiler filter and settings, found by their
//! keys. What isn't: the compiled machine code is not turned back into Java or Kotlin, and an
//! older VDEX version's layout (before Android 12's `027`) is only named.

use serde::Serialize;

/// Most DEX files counted inside one file.
const MAX_DEX: usize = 4096;
/// How far into a file the OAT header is looked for.
const OAT_SEARCH: usize = 64 << 20;
/// Longest key-value text kept.
const MAX_VALUE: usize = 4096;

/// A VDEX section, as version 027 lists them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    /// checksums, dex, verifier_deps, type_lookup_table, or unknown(n)
    pub kind: String,
    pub offset: u32,
    pub size: u32,
}

/// What an ART file is and says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtFile {
    Vdex {
        version: String,
        /// Read only for version 027 (Android 12 and later).
        sections: Vec<Section>,
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
        /// Settings dex2oat recorded, by key (`compiler-filter`, `dex2oat-cmdline`, `classpath`…).
        settings: Vec<(String, String)>,
        /// DEX file headers found anywhere in the file; not proof that whole bytecode is there.
        embedded_dex: usize,
    },
}

/// Whether the bytes start like a VDEX file.
pub fn is_vdex(data: &[u8]) -> bool {
    data.starts_with(b"vdex")
}

fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let b: [u8; 4] = data.get(off..off.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

/// DEX headers (`dex\n0NN\0`) inside `data`.
fn count_dex(data: &[u8]) -> usize {
    data.windows(8)
        .filter(|w| w.starts_with(b"dex\n") && w.get(4..7).is_some_and(|v| v.iter().all(u8::is_ascii_digit)) && w.get(7) == Some(&0))
        .take(MAX_DEX)
        .count()
}

fn version_text(b: &[u8]) -> String {
    b.iter().take_while(|c| **c != 0).map(|c| if c.is_ascii_graphic() { *c as char } else { '?' }).collect()
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
    } else {
        warnings.push(format!("VDEX version {version} predates the layout read here (027); only DEX headers were counted"));
        embedded = count_dex(data);
    }
    Some(ArtFile::Vdex { version, sections, dex_files, embedded_dex: embedded, warnings })
}

/// Read an ODEX/OAT file (an ELF): find its `oat\n` header and the settings recorded after it.
pub fn parse_oat(data: &[u8]) -> Option<ArtFile> {
    if !data.starts_with(b"\x7fELF") {
        return None;
    }
    let search = data.get(..OAT_SEARCH.min(data.len()))?;
    let header_offset =
        search.windows(8).position(|w| w.starts_with(b"oat\n") && w.get(4..7).is_some_and(|v| v.iter().all(u8::is_ascii_digit)) && w.get(7) == Some(&0))?;
    let version = version_text(data.get(header_offset + 4..header_offset + 8)?);
    // The key-value store is NUL-separated text shortly after the header; read known keys.
    let window = data.get(header_offset..header_offset.saturating_add(64 * 1024).min(data.len()))?;
    let mut settings = Vec::new();
    for key in ["compiler-filter", "dex2oat-cmdline", "classpath", "concurrent-copying", "debuggable", "native-debuggable", "pic", "dex2oat-host"] {
        let pat = [key.as_bytes(), b"\0"].concat();
        if let Some(at) = window.windows(pat.len()).position(|w| w == pat.as_slice()) {
            let rest = window.get(at + pat.len()..).unwrap_or_default();
            let value: String = String::from_utf8_lossy(rest.split(|c| *c == 0).next().unwrap_or_default()).chars().take(MAX_VALUE).collect();
            settings.push((key.to_string(), value));
        }
    }
    Some(ArtFile::Oat { version, header_offset, settings, embedded_dex: count_dex(data) })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ArtFile::Vdex { version, sections, dex_files, embedded_dex, warnings } = v else { panic!() };
        assert_eq!((version.as_str(), sections.len(), dex_files, embedded_dex), ("027", 3, Some(2), 2));
        assert!(warnings.is_empty(), "{warnings:?}");
        // Android 12+ usually keeps the bytecode in the APK: an empty dex section.
        let ArtFile::Vdex { embedded_dex, .. } = parse_vdex(&vdex027(&[(0, &[0; 4]), (1, b"")])).unwrap() else { panic!() };
        assert_eq!(embedded_dex, 0);
    }

    #[test]
    fn hostile_or_old_vdex_files_are_bounded() {
        let mut bad = b"vdex027\0".to_vec();
        bad.extend(1000u32.to_le_bytes());
        bad.extend([1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        let ArtFile::Vdex { warnings, .. } = parse_vdex(&bad).unwrap() else { panic!() };
        assert!(warnings.len() >= 2, "{warnings:?}");
        let ArtFile::Vdex { warnings, .. } = parse_vdex(b"vdex021\0junk").unwrap() else { panic!() };
        assert!(warnings[0].contains("021"));
        assert!(parse_vdex(b"vde").is_none() && parse_vdex(b"").is_none());
    }

    #[test]
    fn an_oat_file_gives_its_version_and_compiler_filter() {
        let mut elf = b"\x7fELF".to_vec();
        elf.extend([0; 60]);
        elf.extend(b"oat\n238\0");
        elf.extend([0; 40]);
        elf.extend(b"classpath\0PCL[]\0compiler-filter\0speed-profile\0");
        let ArtFile::Oat { version, settings, header_offset, .. } = parse_oat(&elf).unwrap() else { panic!() };
        assert_eq!((version.as_str(), header_offset), ("238", 64));
        assert!(settings.contains(&("compiler-filter".into(), "speed-profile".into())));
        assert!(settings.contains(&("classpath".into(), "PCL[]".into())));
        assert!(parse_oat(b"\x7fELF no header").is_none());
        assert!(parse_oat(b"oat\n238\0").is_none());
    }
}
