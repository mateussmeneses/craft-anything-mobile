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

/// A `Reference { type = 1; id = 2; name = 3 }`: how it is shown (`@name` or `@0x…`) and its id.
fn reference(data: &[u8]) -> Result<(String, u32), FormatError> {
    let rf = fields(data)?;
    let id = varint_field(&rf, 2).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    let shown = string_field(&rf, 3).map(|n| format!("@{n}")).unwrap_or_else(|| format!("@{id:#010x}"));
    Ok((shown, id))
}

/// A compiled value (`Item`): its text and the binary-XML type it matches.
fn item(data: &[u8]) -> Result<(String, ResValue), FormatError> {
    let fs = fields(data)?;
    let none = ResValue { data_type: 0, data: 0 };
    // Reference { type = 1; id = 2; name = 3 }
    if let Some(r) = message(&fs, 1) {
        let (shown, id) = reference(r)?;
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

fn varint_field(fs: &[(u64, Field<'_>)], num: u64) -> Option<u64> {
    fs.iter().find_map(|(n, f)| if let (true, Field::Varint(v)) = (*n == num, f) { Some(*v) } else { None })
}

/// The id inside a wrapper message (`PackageId`, `TypeId`, `EntryId`: `{ id = 1 }`).
fn wrapped_id(fs: &[(u64, Field<'_>)], num: u64) -> Result<u32, FormatError> {
    match message(fs, num) {
        Some(b) => Ok(varint_field(&fields(b)?, 1).and_then(|v| u32::try_from(v).ok()).unwrap_or(0)),
        None => Ok(0),
    }
}

/// A BCP-47 locale (`es-MX`) in the qualifier form the binary table uses (`es-rMX`).
fn qualifier_locale(l: &str) -> String {
    match l.split_once('-') {
        Some((lang, region)) if region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic()) => format!("{lang}-r{}", region.to_ascii_uppercase()),
        Some((lang, rest)) => format!("b+{lang}+{}", rest.replace('-', "+")),
        None => l.to_string(),
    }
}

/// Most items kept for one style, array or plural.
const MAX_BAG: usize = 4096;
/// Most compound-value items kept across the whole table.
const MAX_BAG_TOTAL: usize = 500_000;

/// The qualifier string (`night-xhdpi-v21`, or `default`) and the locale of a `Configuration`
/// message, in the form [`arsc::config_qualifiers`](crate::arsc::config_qualifiers) gives for a
/// binary table's configuration, in the same order. Unset fields add nothing.
pub fn config_qualifiers(data: &[u8]) -> Result<(String, String), FormatError> {
    let fs = fields(data)?;
    let num = |n: u64| varint_field(&fs, n).unwrap_or(0);
    // An enum field: the name of its value, or nothing when unset or unknown.
    let pick = |n: u64, names: &[&str]| -> Option<String> { names.get(usize::try_from(num(n)).ok()?).filter(|s| !s.is_empty()).map(|s| (*s).to_string()) };
    let mut q: Vec<String> = Vec::new();
    if num(1) != 0 {
        q.push(format!("mcc{}", num(1)));
    }
    if num(2) != 0 {
        q.push(format!("mnc{}", num(2)));
    }
    let locale = string_field(&fs, 3).map(|l| qualifier_locale(&l)).unwrap_or_default();
    if !locale.is_empty() {
        q.push(locale.clone());
    }
    q.extend(pick(4, &["", "ldltr", "ldrtl"]));
    for (n, suffix, prefix) in [(9, "dp", "sw"), (7, "dp", "w"), (8, "dp", "h")] {
        if num(n) != 0 {
            q.push(format!("{prefix}{}{suffix}", num(n)));
        }
    }
    q.extend(pick(10, &["", "small", "normal", "large", "xlarge"]));
    q.extend(pick(11, &["", "long", "notlong"]));
    q.extend(pick(12, &["", "round", "notround"]));
    q.extend(pick(13, &["", "widecg", "nowidecg"]));
    q.extend(pick(14, &["", "highdr", "lowdr"]));
    q.extend(pick(15, &["", "port", "land", "square"]));
    q.extend(pick(16, &["", "", "desk", "car", "television", "appliance", "watch", "vrheadset"]));
    q.extend(pick(17, &["", "night", "notnight"]));
    if num(18) != 0 {
        q.push(crate::arsc::density_qualifier(u32::try_from(num(18)).unwrap_or(0)));
    }
    q.extend(pick(19, &["", "notouch", "stylus", "finger"]));
    q.extend(pick(20, &["", "keysexposed", "keyshidden", "keyssoft"]));
    q.extend(pick(21, &["", "nokeys", "qwerty", "12key"]));
    q.extend(pick(22, &["", "navexposed", "navhidden"]));
    q.extend(pick(23, &["", "nonav", "dpad", "trackball", "wheel"]));
    if num(5) != 0 && num(6) != 0 {
        q.push(format!("{}x{}", num(5), num(6)));
    }
    if num(24) != 0 {
        q.push(format!("v{}", num(24)));
    }
    let s = if q.is_empty() { "default".to_string() } else { q.join("-") };
    Ok((s, locale))
}

/// A compound value's parent (a style's) and its items.
type Bag = (Option<String>, Vec<crate::arsc::BagItem>);

/// The items of a style, array or plural (`CompoundValue { style = 2; array = 4; plural = 5 }`)
/// as the binary table lists them: the style's parent, and each item with its name id, text and
/// type. `None` for any other compound value (attribute and styleable definitions), which has no
/// items to list here. `budget` counts the items still allowed across the table.
fn compound_items(compound: &[u8], budget: &mut usize) -> Result<Option<Bag>, FormatError> {
    use crate::arsc::BagItem;
    let cf = fields(compound)?;
    let mut parent = None;
    let mut bag = Vec::new();
    let mut push = |name: String, it: Option<&[u8]>, budget: &mut usize| -> Result<(), FormatError> {
        if bag.len() >= MAX_BAG || *budget == 0 {
            return Ok(());
        }
        *budget -= 1;
        let (value, raw) = match it {
            Some(i) => item(i)?,
            None => (String::new(), ResValue { data_type: 0, data: 0 }),
        };
        bag.push(BagItem { name, value, value_type: raw.type_name() });
        Ok(())
    };
    if let Some(style) = message(&cf, 2) {
        // Style { parent = 1 (Reference); parent_source = 2; entry = 3 }
        // Entry { source = 1; comment = 2; key = 3 (Reference); item = 4 }
        let sf = fields(style)?;
        parent =
            message(&sf, 1).map(reference).transpose()?.map(|(shown, id)| if id != 0 { format!("@{id:#010x}") } else { shown }).filter(|p| p != "@0x00000000");
        for (n, f) in &sf {
            let (3, Field::Bytes(eb)) = (n, f) else { continue };
            let ef = fields(eb)?;
            let name = match message(&ef, 3).map(reference).transpose()? {
                Some((shown, id)) => {
                    if id != 0 {
                        format!("@{id:#010x}")
                    } else {
                        shown
                    }
                }
                None => format!("@{:#010x}", 0),
            };
            push(name, message(&ef, 4), budget)?;
        }
    } else if let Some(array) = message(&cf, 4) {
        // Array { element = 1 }; Element { source = 1; comment = 2; item = 3 }. The binary table
        // gives array items no name.
        for (n, f) in fields(array)? {
            let (1, Field::Bytes(eb)) = (n, f) else { continue };
            let ef = fields(eb)?;
            push(format!("@{:#010x}", 0), message(&ef, 3), budget)?;
        }
    } else if let Some(plural) = message(&cf, 5) {
        // Plural { entry = 1 }; Entry { source = 1; comment = 2; arity = 3; item = 4 }. The binary
        // table names each item by its quantity attribute (zero 0x01000005 … many 0x01000009,
        // other 0x01000004).
        for (n, f) in fields(plural)? {
            let (1, Field::Bytes(eb)) = (n, f) else { continue };
            let ef = fields(eb)?;
            let id: u32 = match varint_field(&ef, 3).unwrap_or(0) {
                0 => 0x0100_0005,
                1 => 0x0100_0006,
                2 => 0x0100_0007,
                3 => 0x0100_0008,
                4 => 0x0100_0009,
                _ => 0x0100_0004,
            };
            push(format!("@{id:#010x}"), message(&ef, 4), budget)?;
        }
    } else {
        return Ok(None);
    }
    Ok(Some((parent, bag)))
}

/// Read a bundle module's resource table (`resources.pb`, a `ResourceTable` message) into the
/// same [`Table`](crate::arsc::Table) the binary `resources.arsc` gives. Each value's
/// configuration is read as the qualifier string the binary table uses (locale, density, night
/// mode, screen size, SDK version…). Styles, arrays and plurals list their items as a binary
/// table's bags do; attribute and styleable definitions are listed without items.
pub fn parse_table(data: &[u8], max_entries: usize) -> Result<crate::arsc::Table, FormatError> {
    use crate::arsc::{Entry, Package, Table, TypeInfo};
    let mut t = Table::default();
    let mut bag_budget = MAX_BAG_TOTAL;
    let mut unlisted_compound = false;
    // ResourceTable { source_pool = 1; package = 2 }
    for (n, f) in fields(data)? {
        let (2, Field::Bytes(pb)) = (n, f) else { continue };
        // Package { package_id = 1; package_name = 2; type = 3 }
        let pf = fields(pb)?;
        let pid = wrapped_id(&pf, 1)?;
        let pname = string_field(&pf, 2).unwrap_or_default();
        let mut types = Vec::new();
        for (tn, tf) in &pf {
            let (3, Field::Bytes(tb)) = (tn, tf) else { continue };
            // Type { type_id = 1; name = 2; entry = 3 }
            let tff = fields(tb)?;
            let tid = wrapped_id(&tff, 1)?;
            let tname = string_field(&tff, 2).unwrap_or_default();
            let mut count = 0u32;
            let mut configs: Vec<String> = Vec::new();
            for (en, ef) in &tff {
                let (3, Field::Bytes(eb)) = (en, ef) else { continue };
                count = count.saturating_add(1);
                // Entry { entry_id = 1; name = 2; config_value = 6 }
                let eff = fields(eb)?;
                let eid = wrapped_id(&eff, 1)?;
                let key = string_field(&eff, 2).unwrap_or_default();
                for (cn, cf) in &eff {
                    let (6, Field::Bytes(cb)) = (cn, cf) else { continue };
                    if t.entries.len() >= max_entries {
                        t.truncated = true;
                        break;
                    }
                    // ConfigValue { config = 1 (Configuration); value = 2 }
                    let cvf = fields(cb)?;
                    let (config, locale) = match message(&cvf, 1) {
                        Some(c) => config_qualifiers(c)?,
                        None => ("default".to_string(), String::new()),
                    };
                    if !configs.contains(&config) && configs.len() < 256 {
                        configs.push(config.clone());
                    }
                    // Value { item = 4; compound_value = 5 }
                    let mut parent = None;
                    let mut bag = Vec::new();
                    let (value, value_type, raw) = match message(&cvf, 2).map(fields).transpose()? {
                        Some(vf) => match (message(&vf, 4), message(&vf, 5)) {
                            (Some(i), _) => {
                                let (v, r) = item(i)?;
                                (Some(v), Some(r.type_name()), Some(r))
                            }
                            (None, Some(c)) => match compound_items(c, &mut bag_budget)? {
                                // A style, array or plural: its items, like a binary table's bag.
                                Some((p, items)) => {
                                    parent = p;
                                    bag = items;
                                    (None, None, None)
                                }
                                None => {
                                    unlisted_compound = true;
                                    (None, Some("compound"), None)
                                }
                            },
                            _ => (None, None, None),
                        },
                        None => (None, None, None),
                    };
                    t.entries.push(Entry {
                        id: format!("{:#010x}", (pid & 0xff) << 24 | (tid & 0xff) << 16 | (eid & 0xffff)),
                        package: pname.clone(),
                        type_name: tname.clone(),
                        key: key.clone(),
                        config,
                        locale,
                        value,
                        value_type,
                        parent,
                        bag,
                        raw,
                    });
                }
            }
            types.push(TypeInfo { id: u8::try_from(tid).unwrap_or(0), name: tname, entry_count: count, configs });
        }
        let keys = types.iter().map(|t| t.entry_count as usize).sum();
        t.packages.push(Package { id: pid, name: pname, types, keys, libraries: Vec::new() });
    }
    if unlisted_compound {
        t.warnings.push("read from a bundle's resources.pb: attribute and styleable definitions are listed without their items".into());
    }
    if bag_budget == 0 {
        t.truncated = true;
        t.warnings.push("read from a bundle's resources.pb: the items of some styles, arrays or plurals were left out (item limit reached)".into());
    }
    Ok(t)
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
    fn a_bundle_resource_table_reads_like_a_binary_one() {
        let id = |v: u64| [key(1, 0), var(v)].concat();
        let string_item = |t: &str| bytes(4, &bytes(2, &s(1, t)));
        let config_value = |locale: &str, value: Vec<u8>| bytes(6, &[bytes(1, &s(3, locale)), bytes(2, &value)].concat());
        let entry = |eid: u64, name: &str, values: Vec<Vec<u8>>| bytes(3, &[bytes(1, &id(eid)), s(2, name), values.concat()].concat());
        let strings = bytes(
            3,
            &[
                bytes(1, &id(3)),
                s(2, "string"),
                entry(0, "app_name", vec![config_value("", string_item("Notes")), config_value("pt-BR", string_item("Notas"))]),
                entry(1, "title", vec![config_value("fr", bytes(5, b""))]),
            ]
            .concat(),
        );
        let package = bytes(2, &[bytes(1, &id(0x7f)), s(2, "com.example.app"), strings].concat());
        let t = parse_table(&package, 100).unwrap();
        assert_eq!(t.packages[0].types[0].name, "string");
        assert_eq!(t.packages[0].types[0].entry_count, 2);
        let e: Vec<(&str, &str, &str, Option<&str>)> =
            t.entries.iter().map(|e| (e.id.as_str(), e.key.as_str(), e.locale.as_str(), e.value.as_deref())).collect();
        assert_eq!(
            e,
            vec![("0x7f030000", "app_name", "", Some("Notes")), ("0x7f030000", "app_name", "pt-rBR", Some("Notas")), ("0x7f030001", "title", "fr", None)]
        );
        assert_eq!(t.entries[2].value_type, Some("compound"));
        let capped = parse_table(&package, 1).unwrap();
        assert!(capped.truncated && capped.entries.len() == 1);
        assert_eq!(qualifier_locale("zh-Hant-TW"), "b+zh+Hant+TW");
    }

    fn vf(num: u64, v: u64) -> Vec<u8> {
        [key(num, 0), var(v)].concat()
    }

    #[test]
    fn a_configuration_gives_the_qualifier_a_binary_table_would() {
        assert_eq!(config_qualifiers(&[]).unwrap(), ("default".into(), String::new()));
        let cfg = [s(3, "pt-BR"), vf(17, 1), vf(18, 320), vf(24, 21)].concat();
        assert_eq!(config_qualifiers(&cfg).unwrap(), ("pt-rBR-night-xhdpi-v21".into(), "pt-rBR".into()));
        // Screen size, orientation, layout direction and mcc/mnc, in the binary order.
        let cfg = [vf(1, 310), vf(2, 4), vf(4, 2), vf(9, 600), vf(7, 700), vf(10, 3), vf(15, 2), vf(16, 4), vf(18, 0xfffe), vf(5, 800), vf(6, 480)].concat();
        assert_eq!(config_qualifiers(&cfg).unwrap().0, "mcc310-mnc4-ldrtl-sw600dp-w700dp-large-land-television-anydpi-800x480");
        // Unknown enum values and unset fields add nothing.
        assert_eq!(config_qualifiers(&[vf(15, 99), vf(17, 0), vf(18, 0)].concat()).unwrap().0, "default");
        assert!(config_qualifiers(&[0x1a, 0xff]).is_err());
    }

    /// A package with a string in two configurations and a style, an array, a plural and a
    /// styleable, built the way aapt2 writes a bundle's table.
    fn compound_table(plural_items: usize) -> Vec<u8> {
        let id = |v: u64| [key(1, 0), var(v)].concat();
        let str_item = |t: &str| bytes(2, &s(1, t));
        let refmsg = |rid: u64| bytes(1, &vf(2, rid));
        let config_value = |cfg: Vec<u8>, value: Vec<u8>| bytes(6, &[bytes(1, &cfg), bytes(2, &value)].concat());
        let entry = |eid: u64, name: &str, values: Vec<Vec<u8>>| bytes(3, &[bytes(1, &id(eid)), s(2, name), values.concat()].concat());
        let ty = |tid: u64, name: &str, entries: Vec<Vec<u8>>| bytes(3, &[bytes(1, &id(tid)), s(2, name), entries.concat()].concat());
        let compound = |kind: u64, body: Vec<u8>| bytes(5, &bytes(kind, &body));
        // Style { parent = 1; entry = 3 { key = 3; item = 4 (a reference) } }
        let style = compound(2, [bytes(1, &vf(2, 0x7f0d_0001)), bytes(3, &[bytes(3, &vf(2, 0x0101_0098)), bytes(4, &refmsg(0x7f06_0000))].concat())].concat());
        // Array { element = 1 { item = 3 } }
        let array = compound(4, [bytes(1, &bytes(3, &str_item("a"))), bytes(1, &bytes(3, &str_item("b")))].concat());
        // Plural { entry = 1 { arity = 3 (ONE = 1, OTHER = 5); item = 4 } }
        let mut entries = vec![bytes(1, &[vf(3, 1), bytes(4, &str_item("1 file"))].concat()), bytes(1, &[vf(3, 5), bytes(4, &str_item("n files"))].concat())];
        entries.extend((2..plural_items).map(|_| bytes(1, &[vf(3, 5), bytes(4, &str_item("x"))].concat())));
        let plural = compound(5, entries.concat());
        let styleable = compound(3, Vec::new());
        let string_value = |t: &str| bytes(4, &str_item(t));
        let night_hdpi = [vf(17, 1), vf(18, 240)].concat();
        bytes(
            2,
            &[
                bytes(1, &id(0x7f)),
                s(2, "com.example.app"),
                ty(3, "string", vec![entry(0, "title", vec![config_value(vec![], string_value("Day")), config_value(night_hdpi, string_value("Night"))])]),
                ty(4, "style", vec![entry(0, "Theme", vec![config_value(vec![], style)])]),
                ty(5, "array", vec![entry(0, "names", vec![config_value(vec![], array)])]),
                ty(6, "plurals", vec![entry(0, "files", vec![config_value(vec![], plural)])]),
                ty(7, "styleable", vec![entry(0, "Thing", vec![config_value(vec![], styleable)])]),
            ]
            .concat(),
        )
    }

    #[test]
    fn a_bundle_tables_other_qualifiers_and_compound_items_are_read() {
        let t = parse_table(&compound_table(2), 100).unwrap();
        // The same key in two configurations stays two entries, told apart by qualifier.
        let title: Vec<(&str, &str, Option<&str>)> =
            t.entries.iter().filter(|e| e.key == "title").map(|e| (e.config.as_str(), e.locale.as_str(), e.value.as_deref())).collect();
        assert_eq!(title, vec![("default", "", Some("Day")), ("night-hdpi", "", Some("Night"))]);
        assert_eq!(t.packages[0].types[0].configs, ["default", "night-hdpi"]);
        let by = |k: &str| t.entries.iter().find(|e| e.key == k).unwrap();
        let theme = by("Theme");
        assert_eq!((theme.parent.as_deref(), theme.value.as_deref(), theme.value_type), (Some("@0x7f0d0001"), None, None));
        assert_eq!((theme.bag[0].name.as_str(), theme.bag[0].value.as_str(), theme.bag[0].value_type), ("@0x01010098", "@0x7f060000", "reference"));
        let names: Vec<(&str, &str)> = by("names").bag.iter().map(|b| (b.name.as_str(), b.value.as_str())).collect();
        assert_eq!(names, vec![("@0x00000000", "a"), ("@0x00000000", "b")]);
        let files: Vec<(&str, &str)> = by("files").bag.iter().map(|b| (b.name.as_str(), b.value.as_str())).collect();
        assert_eq!(files, vec![("@0x01000006", "1 file"), ("@0x01000004", "n files")]);
        // A styleable has no items to list, and says so.
        assert_eq!(by("Thing").value_type, Some("compound"));
        assert!(t.warnings.iter().any(|w| w.contains("styleable")), "{:?}", t.warnings);
    }

    #[test]
    fn compound_items_are_capped() {
        let t = parse_table(&compound_table(MAX_BAG + 50), 100).unwrap();
        let files = t.entries.iter().find(|e| e.key == "files").unwrap();
        assert_eq!(files.bag.len(), MAX_BAG);
        assert!(t.entries.iter().all(|e| e.bag.len() <= MAX_BAG));
    }

    #[test]
    fn hostile_input_is_refused_or_bounded_never_a_crash() {
        // A truncated configuration or compound value is an error, never a panic.
        let good = compound_table(3);
        for cut in 0..good.len() {
            let _ = parse_table(&good[..cut], 100);
        }
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
