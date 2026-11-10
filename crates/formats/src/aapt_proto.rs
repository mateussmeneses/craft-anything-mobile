//! Android XML compiled to protocol buffers, as an Android App Bundle (`.aab`) keeps each module's
//! `manifest/AndroidManifest.xml`: an `XmlNode` message (an element with namespaces, attributes and
//! child nodes, or a text node). Read into the same [`Document`] binary XML gives, so
//! [`axml::manifest`](crate::axml::manifest) reads both.
//!
//! Only the fields a manifest needs are read; unknown fields are skipped by wire type. The walk
//! keeps its own stack (no recursion), with limits on nodes and depth.

use crate::FormatError;
use crate::axml::{Attribute, Document, Node, ResValue};

/// Most elements and texts read.
pub const MAX_NODES: usize = 200_000;
/// Deepest nesting read.
pub const MAX_DEPTH: usize = 256;
/// Longest string kept from a field.
const MAX_STRING: usize = 64 * 1024;

/// One field of a message: its number and value.
enum Field<'a> {
    Varint(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
    Other,
}

fn varint(data: &[u8], pos: &mut usize) -> Result<u64, FormatError> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *data.get(*pos).ok_or_else(|| FormatError::Truncated("varint".into()))?;
        *pos = pos.saturating_add(1);
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(FormatError::Malformed("varint longer than 10 bytes".into()))
}

/// The fields of one message, in order.
fn fields(data: &[u8]) -> Result<Vec<(u64, Field<'_>)>, FormatError> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let key = varint(data, &mut pos)?;
        let (num, wire) = (key >> 3, key & 7);
        let f = match wire {
            0 => Field::Varint(varint(data, &mut pos)?),
            1 => {
                pos = pos.checked_add(8).filter(|p| *p <= data.len()).ok_or_else(|| FormatError::Truncated("fixed64".into()))?;
                Field::Other
            }
            2 => {
                let len = usize::try_from(varint(data, &mut pos)?).map_err(|_| FormatError::Malformed("length".into()))?;
                let end = pos.checked_add(len).filter(|e| *e <= data.len()).ok_or_else(|| FormatError::Truncated("length-delimited field".into()))?;
                let b = data.get(pos..end).ok_or_else(|| FormatError::Truncated("field".into()))?;
                pos = end;
                Field::Bytes(b)
            }
            5 => {
                let end = pos.checked_add(4).filter(|e| *e <= data.len()).ok_or_else(|| FormatError::Truncated("fixed32".into()))?;
                let b: [u8; 4] = data.get(pos..end).and_then(|b| b.try_into().ok()).ok_or_else(|| FormatError::Truncated("fixed32".into()))?;
                pos = end;
                Field::Fixed32(u32::from_le_bytes(b))
            }
            w => return Err(FormatError::Malformed(format!("wire type {w}"))),
        };
        out.push((num, f));
    }
    Ok(out)
}

fn text(b: &[u8]) -> String {
    let b = b.get(..MAX_STRING.min(b.len())).unwrap_or_default();
    String::from_utf8_lossy(b).into_owned()
}

fn string_field(fs: &[(u64, Field<'_>)], num: u64) -> Option<String> {
    fs.iter().find_map(|(n, f)| match f {
        Field::Bytes(b) if *n == num => Some(text(b)),
        _ => None,
    })
}

fn message<'a>(fs: &[(u64, Field<'a>)], num: u64) -> Option<&'a [u8]> {
    fs.iter().find_map(|(n, f)| match f {
        Field::Bytes(b) if *n == num => Some(*b),
        _ => None,
    })
}

/// A compiled value (`Item`): its text and the binary-XML type it matches.
fn item(data: &[u8]) -> Result<(String, ResValue), FormatError> {
    let fs = fields(data)?;
    let none = ResValue { data_type: 0, data: 0 };
    // Reference { type = 1; id = 2; name = 3 }
    if let Some(r) = message(&fs, 1) {
        let rf = fields(r)?;
        let id = rf.iter().find_map(|(n, f)| if let (2, Field::Varint(v)) = (n, f) { u32::try_from(*v).ok() } else { None }).unwrap_or(0);
        let shown = string_field(&rf, 3).map(|n| format!("@{n}")).unwrap_or_else(|| format!("@{id:#010x}"));
        return Ok((shown, ResValue { data_type: 0x01, data: id }));
    }
    // String { value = 1 } and RawString { value = 1 }
    for num in [2, 3] {
        if let Some(s) = message(&fs, num) {
            return Ok((string_field(&fields(s)?, 1).unwrap_or_default(), ResValue { data_type: 0x03, data: 0 }));
        }
    }
    // Primitive
    if let Some(p) = message(&fs, 7) {
        // A primitive holds one value: its first field.
        if let Some((n, f)) = fields(p)?.into_iter().next() {
            let v = match f {
                Field::Varint(v) => v as u32,
                Field::Fixed32(v) => v,
                _ => 0,
            };
            return Ok(match n {
                1 => ("@null".into(), ResValue { data_type: 0x00, data: 0 }),
                2 => ("@empty".into(), ResValue { data_type: 0x00, data: 1 }),
                3 => (f32::from_bits(v).to_string(), ResValue { data_type: 0x04, data: v }),
                6 => ((v as i32).to_string(), ResValue { data_type: 0x10, data: v }),
                7 => (format!("{v:#x}"), ResValue { data_type: 0x11, data: v }),
                8 => ((if v != 0 { "true" } else { "false" }).into(), ResValue { data_type: 0x12, data: u32::from(v != 0) }),
                9..=12 => (format!("#{v:08x}"), ResValue { data_type: 0x1c, data: v }),
                _ => (format!("{v:#x}"), none),
            });
        }
    }
    Ok((String::new(), none))
}

/// An attribute (`XmlAttribute { namespace_uri = 1; name = 2; value = 3; resource_id = 5; compiled_item = 6 }`).
fn attribute(data: &[u8]) -> Result<Attribute, FormatError> {
    let fs = fields(data)?;
    let namespace = string_field(&fs, 1).filter(|s| !s.is_empty());
    let name = string_field(&fs, 2).unwrap_or_default();
    let raw_value = string_field(&fs, 3).unwrap_or_default();
    let resource_id = fs.iter().find_map(|(n, f)| if let (5, Field::Varint(v)) = (n, f) { Some(format!("{v:#010x}")) } else { None });
    let (compiled, raw) = match message(&fs, 6) {
        Some(i) => item(i)?,
        None => (String::new(), ResValue { data_type: 0x03, data: 0 }),
    };
    // The source text, when kept, is what the author wrote; otherwise the compiled value.
    let value = if raw_value.is_empty() { compiled } else { raw_value };
    let mut a = Attribute { namespace, name, resource_id, value, value_type: "string", raw };
    a.value_type = a.raw.type_name();
    Ok(a)
}

/// Read a module's manifest (or any XML) compiled to protocol buffers.
pub fn parse(data: &[u8]) -> Result<Document, FormatError> {
    let mut doc = Document::default();
    // Each frame: the element's name and the child nodes left to read.
    let mut stack: Vec<(String, std::vec::IntoIter<&[u8]>)> = Vec::new();
    let mut pending: Option<&[u8]> = Some(data);
    let mut count = 0usize;
    loop {
        let node = match pending.take() {
            Some(n) => n,
            None => {
                let Some((_, children)) = stack.last_mut() else { break };
                match children.next() {
                    Some(c) => c,
                    None => {
                        if let Some((name, _)) = stack.pop() {
                            doc.nodes.push(Node::End { depth: stack.len(), name });
                        }
                        continue;
                    }
                }
            }
        };
        count = count.saturating_add(1);
        if count > MAX_NODES {
            doc.truncated = true;
            doc.warnings.push(format!("stopped after {MAX_NODES} nodes"));
            break;
        }
        let fs = fields(node)?;
        let depth = stack.len();
        // XmlNode { element = 1; text = 2 }
        if let Some(t) = string_field(&fs, 2) {
            doc.nodes.push(Node::Text { depth, text: t });
            continue;
        }
        let Some(el) = message(&fs, 1) else { continue };
        if depth >= MAX_DEPTH {
            doc.truncated = true;
            doc.warnings.push(format!("elements deeper than {MAX_DEPTH} levels were left out"));
            continue;
        }
        // XmlElement { namespace_declaration = 1; namespace_uri = 2; name = 3; attribute = 4; child = 5 }
        let ef = fields(el)?;
        let mut attributes = Vec::new();
        let mut children = Vec::new();
        for (n, f) in &ef {
            match (n, f) {
                (1, Field::Bytes(b)) => {
                    // XmlNamespace { prefix = 1; uri = 2 }
                    let nf = fields(b)?;
                    let (prefix, uri) = (string_field(&nf, 1).unwrap_or_default(), string_field(&nf, 2).unwrap_or_default());
                    if doc.namespaces.len() < 256 {
                        doc.namespaces.push((prefix.clone(), uri.clone()));
                    }
                    doc.nodes.push(Node::Namespace { prefix, uri });
                }
                (4, Field::Bytes(b)) => attributes.push(attribute(b)?),
                (5, Field::Bytes(b)) => children.push(*b),
                _ => {}
            }
        }
        let name = string_field(&ef, 3).unwrap_or_default();
        let namespace = string_field(&ef, 2).filter(|s| !s.is_empty());
        doc.nodes.push(Node::Start { depth, name: name.clone(), namespace, line: 0, attributes });
        stack.push((name, children.into_iter()));
    }
    // Close what a stop left open, so readers see whole elements.
    while let Some((name, _)) = stack.pop() {
        doc.nodes.push(Node::End { depth: stack.len(), name });
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::axml::{ANDROID_NS, manifest};

    fn key(num: u64, wire: u64) -> Vec<u8> {
        var(num << 3 | wire)
    }
    fn var(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }
    fn bytes(num: u64, b: &[u8]) -> Vec<u8> {
        [key(num, 2), var(b.len() as u64), b.to_vec()].concat()
    }
    fn s(num: u64, t: &str) -> Vec<u8> {
        bytes(num, t.as_bytes())
    }
    fn attr(ns: &str, name: &str, value: &str, item: Option<Vec<u8>>) -> Vec<u8> {
        let mut a = [s(1, ns), s(2, name), s(3, value)].concat();
        if let Some(i) = item {
            a.extend(bytes(6, &i));
        }
        a
    }
    fn int_item(v: u64) -> Vec<u8> {
        bytes(7, &[key(6, 0), var(v)].concat())
    }
    fn bool_item(v: bool) -> Vec<u8> {
        bytes(7, &[key(8, 0), var(u64::from(v))].concat())
    }
    fn element(name: &str, attrs: &[Vec<u8>], children: &[Vec<u8>]) -> Vec<u8> {
        let mut e = s(3, name);
        for a in attrs {
            e.extend(bytes(4, a));
        }
        for c in children {
            e.extend(bytes(5, c));
        }
        bytes(1, &e)
    }

    #[test]
    fn a_bundle_manifest_reads_like_a_binary_one() {
        let activity = element(
            "activity",
            &[attr(ANDROID_NS, "name", ".Main", None), attr(ANDROID_NS, "exported", "", Some(bool_item(true)))],
            &[element("intent-filter", &[], &[element("action", &[attr(ANDROID_NS, "name", "android.intent.action.MAIN", None)], &[])])],
        );
        let app = element("application", &[attr(ANDROID_NS, "debuggable", "", Some(bool_item(false)))], &[activity]);
        let sdk = element(
            "uses-sdk",
            &[attr(ANDROID_NS, "minSdkVersion", "", Some(int_item(24))), attr(ANDROID_NS, "targetSdkVersion", "34", Some(int_item(34)))],
            &[],
        );
        let perm = element("uses-permission", &[attr(ANDROID_NS, "name", "android.permission.INTERNET", None)], &[]);
        let root = element(
            "manifest",
            &[
                attr("", "package", "com.example.app", None),
                attr(ANDROID_NS, "versionCode", "", Some(int_item(7))),
                attr("", "split", "config.arm64_v8a", None),
                attr(ANDROID_NS, "compileSdkVersion", "35", None),
            ],
            &[sdk, perm, app],
        );
        let doc = parse(&root).unwrap();
        let m = manifest(&doc);
        assert_eq!(m.package.as_deref(), Some("com.example.app"));
        assert_eq!((m.version_code.as_deref(), m.min_sdk.as_deref(), m.target_sdk.as_deref()), (Some("7"), Some("24"), Some("34")));
        assert_eq!(m.uses_permissions, vec!["android.permission.INTERNET".to_string()]);
        assert_eq!(m.compile_sdk.as_deref(), Some("35"));
        assert_eq!(m.application.debuggable, Some(false));
        assert_eq!(m.components.len(), 1);
        assert_eq!(m.components[0].name, "com.example.app.Main");
        assert!(m.components[0].exported);
        assert_eq!(m.components[0].intent_filters[0].actions, vec!["android.intent.action.MAIN".to_string()]);
        assert!(!doc.truncated);
    }

    #[test]
    fn hostile_input_is_refused_or_bounded_never_a_crash() {
        for bad in [&[0x0a, 0xff][..], &[0x0a, 0x05, 0x01], &[0xff; 11], &[0x0b], &[0x0d, 0x01]] {
            let _ = parse(bad);
        }
        // Nesting far deeper than the limit: the walk stops descending, never overflows.
        let mut node = element("x", &[], &[]);
        for _ in 0..(MAX_DEPTH + 20) {
            node = element("x", &[], &[node]);
        }
        let doc = parse(&node).unwrap();
        assert!(doc.truncated);
        let starts = doc.nodes.iter().filter(|n| matches!(n, Node::Start { .. })).count();
        let ends = doc.nodes.iter().filter(|n| matches!(n, Node::End { .. })).count();
        assert_eq!((starts, ends), (MAX_DEPTH, MAX_DEPTH));
    }
}
