//! The controls on an Android screen, read from a `uiautomator dump`.
//!
//! The dump is XML with one element kind: `<node …>` inside `<hierarchy rotation="…">`, each node
//! carrying its class, text, resource id, state flags and `bounds="[left,top][right,bottom]"`.
//! This reader handles exactly that shape. It is written for hostile input (the text comes from
//! whatever app is on screen): it never recurses, keeps at most [`MAX_NODES`] nodes and
//! [`MAX_DEPTH`] levels, cuts long values at [`MAX_VALUE`] bytes, and says when it stopped early.
//!
//! A password field's text is never kept: the node says `password: true` and `text` stays empty,
//! whatever the dump held.

use serde::Serialize;

/// Most nodes kept from one screen.
pub const MAX_NODES: usize = 5_000;
/// Deepest nesting followed; deeper nodes are counted as cut, not kept.
pub const MAX_DEPTH: usize = 128;
/// Most bytes kept of one text, description, id or class.
pub const MAX_VALUE: usize = 2_048;

/// A rectangle on the screen, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    /// Its middle, where a tap lands; `None` for a rectangle with no area.
    pub fn center(&self) -> Option<(i32, i32)> {
        let (w, h) = (self.right.checked_sub(self.left)?, self.bottom.checked_sub(self.top)?);
        (w > 0 && h > 0).then(|| (self.left.saturating_add(w / 2), self.top.saturating_add(h / 2)))
    }
}

/// One control on the screen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct UiNode {
    /// Its place in [`UiTree::nodes`].
    pub id: usize,
    /// The node it sits in; `None` for a window's root.
    pub parent: Option<usize>,
    pub depth: usize,
    pub class: String,
    pub package: String,
    pub resource_id: String,
    /// What it shows. Always empty for a password field.
    pub text: String,
    pub content_desc: String,
    pub hint: String,
    pub bounds: Option<Bounds>,
    pub clickable: bool,
    pub long_clickable: bool,
    pub checkable: bool,
    pub checked: bool,
    pub enabled: bool,
    pub focusable: bool,
    pub focused: bool,
    pub scrollable: bool,
    pub selected: bool,
    pub password: bool,
    /// A value above was longer than [`MAX_VALUE`] and was cut.
    pub cut: bool,
    /// For a control a person can act on: its own text or description, or else the texts of the
    /// controls inside it (a list row's title and summary), joined with " · ". Empty otherwise.
    pub label: String,
    /// For a control a person can't act on: the nearest one around it that they can (where a tap
    /// on this text lands). `None` when nothing around it is interactive.
    pub acts_through: Option<usize>,
}

impl UiNode {
    /// Something a person can act on: tap, hold, tick, scroll or type into.
    pub fn interactive(&self) -> bool {
        self.clickable || self.long_clickable || self.checkable || self.scrollable || self.focusable
    }
}

/// A screen's controls, parents before children, in the order the dump lists them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct UiTree {
    /// Quarter turns of the screen: 0 to 3.
    pub rotation: Option<u8>,
    pub nodes: Vec<UiNode>,
    /// Nodes left out because of [`MAX_NODES`] or [`MAX_DEPTH`].
    pub dropped: usize,
    /// The text ended inside an element, or held markup this reader doesn't know: what was read
    /// before that point is kept.
    pub incomplete: bool,
}

/// Why a dump could not be read at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiError {
    /// There is no `<hierarchy` element: the device printed something else (an error line).
    NotADump,
}

/// Read a dump. Text after the closing `</hierarchy>` (uiautomator's own status line) is ignored.
pub fn parse(text: &str) -> Result<UiTree, UiError> {
    let start = text.find("<hierarchy").ok_or(UiError::NotADump)?;
    let mut rest = text.get(start..).unwrap_or_default();
    let mut tree = UiTree::default();
    // The open nodes: `Some(id)` for one that was kept, `None` for one that was dropped.
    let mut open: Vec<Option<usize>> = Vec::new();
    let mut seen_root = false;
    loop {
        let Some(lt) = rest.find('<') else {
            tree.incomplete = true;
            break;
        };
        rest = rest.get(lt..).unwrap_or_default();
        let Some(gt) = tag_end(rest) else {
            tree.incomplete = true;
            break;
        };
        let tag = rest.get(1..gt).unwrap_or_default();
        rest = rest.get(gt.saturating_add(1)..).unwrap_or_default();
        if let Some(name) = tag.strip_prefix('/') {
            match name.trim() {
                "node" => {
                    if open.pop().is_none() {
                        tree.incomplete = true;
                        break;
                    }
                }
                "hierarchy" => {
                    tree.incomplete |= !open.is_empty();
                    break;
                }
                _ => {
                    tree.incomplete = true;
                    break;
                }
            }
            continue;
        }
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let (body, self_closing) = match tag.strip_suffix('/') {
            Some(b) => (b, true),
            None => (tag, false),
        };
        let (name, attrs) = body.split_once(|c: char| c.is_ascii_whitespace()).unwrap_or((body, ""));
        match name {
            "hierarchy" if !seen_root => {
                seen_root = true;
                for (key, value) in Attrs::new(attrs) {
                    if key == "rotation" {
                        tree.rotation = value.trim().parse::<u8>().ok().filter(|r| *r < 4);
                    }
                }
                if self_closing {
                    break;
                }
            }
            "node" if seen_root => {
                let parent = open.last().copied().flatten();
                let depth = open.len();
                // A node under a dropped one is dropped too: it would have no place in the tree.
                let under_dropped = open.last().is_some_and(Option::is_none);
                let kept = if tree.nodes.len() >= MAX_NODES || depth >= MAX_DEPTH || under_dropped {
                    tree.dropped = tree.dropped.saturating_add(1);
                    None
                } else {
                    let id = tree.nodes.len();
                    tree.nodes.push(node(id, parent, depth, attrs));
                    Some(id)
                };
                if !self_closing {
                    open.push(kept);
                }
            }
            _ => {
                tree.incomplete = true;
                break;
            }
        }
    }
    link_labels(&mut tree.nodes);
    if !seen_root {
        return Err(UiError::NotADump);
    }
    Ok(tree)
}

/// Most bytes in one control's gathered [`UiNode::label`].
pub const MAX_LABEL: usize = 300;

/// Fill [`UiNode::acts_through`] and [`UiNode::label`]. Parents come before children, so one pass
/// in order finds every node's nearest interactive ancestor.
fn link_labels(nodes: &mut [UiNode]) {
    for i in 0..nodes.len() {
        let through = nodes.get(i).and_then(|n| n.parent).and_then(|p| {
            let parent = nodes.get(p)?;
            if parent.interactive() { Some(p) } else { parent.acts_through }
        });
        if let Some(n) = nodes.get_mut(i)
            && !n.interactive()
        {
            n.acts_through = through;
        }
    }
    for i in 0..nodes.len() {
        let Some(n) = nodes.get(i) else { continue };
        let own = if n.text.is_empty() { n.content_desc.clone() } else { n.text.clone() };
        if own.is_empty() {
            continue;
        }
        let owner = if n.interactive() { Some(i) } else { n.acts_through };
        if let Some(owner) = owner.and_then(|o| nodes.get_mut(o))
            && owner.label.len() < MAX_LABEL
        {
            if !owner.label.is_empty() {
                owner.label.push_str(" · ");
            }
            owner.label.push_str(&own);
            if owner.label.len() > MAX_LABEL {
                let mut end = MAX_LABEL;
                while end > 0 && !owner.label.is_char_boundary(end) {
                    end -= 1;
                }
                owner.label.truncate(end);
            }
        }
    }
}

/// Where a tag that starts at `s[0] == '<'` ends: the first `>` outside a quoted value.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in s.bytes().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
    }
    None
}

/// The `name="value"` pairs of a tag, values unescaped. Stops at the first thing that isn't one.
struct Attrs<'a> {
    rest: &'a str,
}

impl<'a> Attrs<'a> {
    fn new(rest: &'a str) -> Self {
        Attrs { rest }
    }
}

impl<'a> Iterator for Attrs<'a> {
    type Item = (&'a str, String);

    fn next(&mut self) -> Option<Self::Item> {
        let s = self.rest.trim_start();
        let eq = s.find('=')?;
        let key = s.get(..eq)?.trim();
        let after = s.get(eq.saturating_add(1)..)?.trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = after.get(1..)?;
        let close = value.find(quote)?;
        self.rest = value.get(close.saturating_add(1)..)?;
        Some((key, unescape(value.get(..close)?)))
    }
}

/// Turn XML's character references back into characters; an unknown one is kept as written.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(rest.get(..amp).unwrap_or_default());
        let tail = rest.get(amp..).unwrap_or_default();
        // A reference is short: `&#x10FFFF;` is the longest that means anything.
        let semi = tail.char_indices().take(12).find(|(_, c)| *c == ';').map(|(i, _)| i);
        let replaced = semi.and_then(|semi| {
            let name = tail.get(1..semi)?;
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                _ => {
                    let code = match name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => name.strip_prefix('#')?.parse::<u32>().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, semi))
        });
        match replaced {
            Some((c, semi)) => {
                out.push(c);
                rest = tail.get(semi.saturating_add(1)..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = tail.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// `s` cut to [`MAX_VALUE`] bytes at a character boundary, and whether it was cut.
fn capped(s: String) -> (String, bool) {
    if s.len() <= MAX_VALUE {
        return (s, false);
    }
    let mut end = MAX_VALUE;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s.get(..end).unwrap_or_default().to_string(), true)
}

fn node(id: usize, parent: Option<usize>, depth: usize, attrs: &str) -> UiNode {
    let mut n = UiNode { id, parent, depth, ..UiNode::default() };
    let mut text = String::new();
    for (key, value) in Attrs::new(attrs).take(64) {
        let flag = value == "true";
        let mut keep = |slot: &mut String, value: String| {
            let (v, cut) = capped(value);
            *slot = v;
            n.cut |= cut;
        };
        match key {
            "class" => keep(&mut n.class, value),
            "package" => keep(&mut n.package, value),
            "resource-id" => keep(&mut n.resource_id, value),
            "content-desc" => keep(&mut n.content_desc, value),
            "hint" => keep(&mut n.hint, value),
            "text" => keep(&mut text, value),
            "bounds" => n.bounds = parse_bounds(&value),
            "clickable" => n.clickable = flag,
            "long-clickable" => n.long_clickable = flag,
            "checkable" => n.checkable = flag,
            "checked" => n.checked = flag,
            "enabled" => n.enabled = flag,
            "focusable" => n.focusable = flag,
            "focused" => n.focused = flag,
            "scrollable" => n.scrollable = flag,
            "selected" => n.selected = flag,
            "password" => n.password = flag,
            _ => {}
        }
    }
    // Attributes come in any order, so the text is dropped only once `password` is known.
    if !n.password {
        n.text = text;
    }
    n
}

/// `[left,top][right,bottom]`.
pub fn parse_bounds(s: &str) -> Option<Bounds> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (a, b) = inner.split_once("][")?;
    let pair = |p: &str| {
        let (x, y) = p.split_once(',')?;
        Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?))
    };
    let ((left, top), (right, bottom)) = (pair(a)?, pair(b)?);
    Some(Bounds { left, top, right, bottom })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>";

    fn n(attrs: &str, children: &str) -> String {
        format!("<node {attrs}>{children}</node>")
    }

    #[test]
    fn a_dump_is_read_parents_first_with_the_status_line_ignored() {
        let leaf = r#"<node index="1" text="Wi&#45;Fi &amp; &quot;more&quot;" resource-id="android:id/title" class="android.widget.TextView" package="com.android.settings" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" long-clickable="false" password="false" selected="false" bounds="[63,400][500,460]" drawing-order="2" hint="" />"#;
        let root = n(r#"index="0" text="" class="android.widget.FrameLayout" package="com.android.settings" enabled="true" bounds="[0,0][1080,2400]""#, leaf);
        let dump = format!("{HEAD}<hierarchy rotation=\"1\">{root}</hierarchy>UI hierchary dumped to: /dev/tty");
        let t = parse(&dump).unwrap();
        assert_eq!((t.rotation, t.nodes.len(), t.dropped, t.incomplete), (Some(1), 2, 0, false));
        let (frame, title) = (&t.nodes[0], &t.nodes[1]);
        assert_eq!((frame.parent, frame.depth, frame.class.as_str()), (None, 0, "android.widget.FrameLayout"));
        assert_eq!(frame.bounds, Some(Bounds { left: 0, top: 0, right: 1080, bottom: 2400 }));
        assert!(!frame.interactive());
        assert_eq!((title.parent, title.depth), (Some(0), 1));
        assert_eq!(title.text, "Wi-Fi & \"more\"");
        assert_eq!(title.resource_id, "android:id/title");
        assert!(title.clickable && title.focusable && title.enabled && title.interactive());
        assert_eq!(title.bounds.and_then(|b| b.center()), Some((281, 430)));
    }

    #[test]
    fn a_password_fields_text_is_never_kept_whatever_the_attribute_order() {
        for attrs in [r#"text="hunter2" password="true" class="android.widget.EditText""#, r#"password="true" class="android.widget.EditText" text="hunter2""#]
        {
            let dump = format!("<hierarchy rotation=\"0\">{}</hierarchy>", n(attrs, ""));
            let t = parse(&dump).unwrap();
            assert!(t.nodes[0].password);
            assert_eq!(t.nodes[0].text, "");
            assert!(!serde_json::to_string(&t).unwrap().contains("hunter2"));
        }
        let plain = parse(&format!("<hierarchy>{}</hierarchy>", n(r#"text="hello" password="false""#, ""))).unwrap();
        assert_eq!(plain.nodes[0].text, "hello");
    }

    #[test]
    fn siblings_and_nesting_keep_their_parents() {
        let dump = format!(
            "<hierarchy rotation=\"0\">{}{}</hierarchy>",
            n("class=\"A\"", &format!("{}{}", n("class=\"B\"", &n("class=\"C\"", "")), n("class=\"D\"", ""))),
            n("class=\"E\"", "")
        );
        let t = parse(&dump).unwrap();
        let shape: Vec<(&str, Option<usize>, usize)> = t.nodes.iter().map(|x| (x.class.as_str(), x.parent, x.depth)).collect();
        assert_eq!(shape, [("A", None, 0), ("B", Some(0), 1), ("C", Some(1), 2), ("D", Some(0), 1), ("E", None, 0)]);
    }

    #[test]
    fn something_that_is_not_a_dump_is_an_error_not_an_empty_screen() {
        for text in ["", "ERROR: could not get idle state.", "<html><body/></html>", "<node class=\"A\"/>"] {
            assert_eq!(parse(text), Err(UiError::NotADump), "{text:?}");
        }
    }

    #[test]
    fn a_dump_cut_short_keeps_what_was_read_and_says_so() {
        let whole = format!("<hierarchy rotation=\"0\">{}</hierarchy>", n("class=\"A\"", &n("class=\"B\" text=\"x\"", "")));
        for cut in [whole.len() - 3, whole.len() - 14, whole.find("class=\"B\"").unwrap() + 4, 30] {
            let t = parse(&whole[..cut]).unwrap();
            assert!(t.incomplete, "cut at {cut}");
            assert!(t.nodes.len() <= 2);
        }
        let t = parse("<hierarchy rotation=\"0\"><node class=\"A\"></node></node></hierarchy>").unwrap();
        assert!(t.incomplete && t.nodes.len() == 1, "a close with nothing open");
        let t = parse("<hierarchy><node class=\"A\"/><other/></hierarchy>").unwrap();
        assert!(t.incomplete && t.nodes.len() == 1, "markup it doesn't know");
    }

    #[test]
    fn a_closing_bracket_inside_a_value_does_not_end_the_tag() {
        let t = parse("<hierarchy><node text=\"a > b\" content-desc='c \" d' class=\"A\" /></hierarchy>").unwrap();
        assert_eq!((t.nodes[0].text.as_str(), t.nodes[0].content_desc.as_str(), t.nodes[0].class.as_str()), ("a > b", "c \" d", "A"));
        assert!(!t.incomplete);
    }

    #[test]
    fn too_many_nodes_or_levels_are_counted_not_kept() {
        let many = format!("<hierarchy>{}</hierarchy>", "<node class=\"A\" />".repeat(MAX_NODES + 7));
        let t = parse(&many).unwrap();
        assert_eq!((t.nodes.len(), t.dropped, t.incomplete), (MAX_NODES, 7, false));

        let deep = format!("<hierarchy>{}{}</hierarchy>", "<node class=\"A\">".repeat(MAX_DEPTH + 5), "</node>".repeat(MAX_DEPTH + 5));
        let t = parse(&deep).unwrap();
        assert_eq!((t.nodes.len(), t.dropped, t.incomplete), (MAX_DEPTH, 5, false));
        assert_eq!(t.nodes.last().map(|x| x.depth), Some(MAX_DEPTH - 1));

        // A node after a dropped subtree goes back to its real parent.
        let mixed = format!(
            "<hierarchy><node class=\"R\">{}{}<node class=\"Z\" /></node></hierarchy>",
            "<node class=\"A\">".repeat(MAX_DEPTH),
            "</node>".repeat(MAX_DEPTH)
        );
        let t = parse(&mixed).unwrap();
        let z = t.nodes.iter().find(|x| x.class == "Z").unwrap();
        assert_eq!((z.parent, z.depth), (Some(0), 1));
        assert_eq!(t.dropped, 1);
    }

    #[test]
    fn long_values_are_cut_at_a_character_boundary_and_flagged() {
        let long = "é".repeat(MAX_VALUE);
        let t = parse(&format!("<hierarchy><node text=\"{long}\" class=\"A\" /></hierarchy>")).unwrap();
        assert!(t.nodes[0].cut);
        assert_eq!(t.nodes[0].text.len(), MAX_VALUE);
        assert!(t.nodes[0].text.chars().all(|c| c == 'é'));
    }

    #[test]
    fn character_references_are_read_and_unknown_ones_left_alone() {
        assert_eq!(unescape("a&lt;b&gt;c&apos;&#233;&#xE9;&#x1F600;"), "a<b>c'éé😀");
        for odd in ["&", "&;", "&nope;", "&#;", "&#xZZ;", "&#1114112;", "&#55296;", "a & b", "&amp", "&#99999999999999999999;"] {
            assert_eq!(unescape(odd), odd, "{odd:?}");
        }
        assert_eq!(unescape("&amp;amp;"), "&amp;");
    }

    #[test]
    fn a_rows_texts_label_the_control_that_takes_the_tap() {
        let row = n(
            "class=\"Row\" clickable=\"true\"",
            &n("class=\"Box\"", &format!("{}{}", n("class=\"T\" text=\"Network &amp; internet\"", ""), n("class=\"T\" text=\"Wi-Fi, hotspot\"", ""))),
        );
        let dump = format!("<hierarchy>{}</hierarchy>", n("class=\"Root\"", &format!("{row}{}", n("class=\"T\" text=\"Loose\"", ""))));
        let t = parse(&dump).unwrap();
        let by = |c: &str, text: &str| t.nodes.iter().find(|x| x.class == c && x.text == text).unwrap();
        let row = by("Row", "");
        assert_eq!(row.label, "Network & internet · Wi-Fi, hotspot");
        assert_eq!(row.acts_through, None, "an interactive control acts for itself");
        assert_eq!(by("T", "Wi-Fi, hotspot").acts_through, Some(row.id));
        assert_eq!(by("Box", "").acts_through, Some(row.id));
        let loose = by("T", "Loose");
        assert_eq!((loose.acts_through, loose.label.as_str()), (None, ""), "nothing around it can be tapped");
        assert_eq!(by("Root", "").label, "", "a control nobody can act on gathers no label");
    }

    #[test]
    fn a_label_has_a_cap_and_never_holds_a_password() {
        let kids: String = (0..100).map(|i| n(&format!("class=\"T\" text=\"élément {i}\""), "")).collect();
        let secret = n("class=\"E\" text=\"hunter2\" password=\"true\"", "");
        let t = parse(&format!("<hierarchy>{}</hierarchy>", n("class=\"Row\" clickable=\"true\"", &format!("{secret}{kids}")))).unwrap();
        assert!(t.nodes[0].label.len() <= MAX_LABEL, "{}", t.nodes[0].label.len());
        assert!(t.nodes[0].label.starts_with("élément 0"));
        assert!(!serde_json::to_string(&t).unwrap().contains("hunter2"));
    }

    #[test]
    fn bounds_are_read_strictly() {
        assert_eq!(parse_bounds("[0,66][1080,2400]"), Some(Bounds { left: 0, top: 66, right: 1080, bottom: 2400 }));
        assert_eq!(parse_bounds("[-5,0][10,10]").map(|b| b.left), Some(-5));
        for bad in ["", "[]", "[0,0]", "[0,0][1,]", "[a,b][c,d]", "0,0,1,1", "[0,0][1,1][2,2]", "[99999999999,0][1,1]"] {
            assert_eq!(parse_bounds(bad), None, "{bad:?}");
        }
        // A rectangle turned inside out (seen on real screens) has no middle to tap.
        assert_eq!(parse_bounds("[189,2391][1038,2337]").and_then(|b| b.center()), None);
        assert_eq!(Bounds { left: i32::MIN, top: 0, right: i32::MAX, bottom: 1 }.center(), None, "no overflow");
    }

    #[test]
    fn attributes_that_break_off_leave_the_node_with_what_came_before() {
        let t = parse("<hierarchy><node class=\"A\" text=\"ok\" clickable=true bounds=\"[0,0][1,1]\" /></hierarchy>").unwrap();
        assert_eq!((t.nodes[0].class.as_str(), t.nodes[0].text.as_str(), t.nodes[0].clickable, t.nodes[0].bounds), ("A", "ok", false, None));
    }
}
