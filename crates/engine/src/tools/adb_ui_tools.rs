//! The controls on a device's screen (`adb_ui_show`).
//!
//! Asks the device for its own description of the screen (`uiautomator dump`) and returns it as a
//! paged, typed list: each control's class, text, resource id, state and place. It watches from
//! outside, as a person looking at the screen does, so it runs in Clean room; what it returns is
//! the app's own text, so it is marked as carrying target content. What a person typed into a
//! field (a password above all) is never returned.

use super::adb_tools::{adb_for, dev_ev, ensure_online, serial_of};
use super::native_common::spec;
use crate::tool::{Ctx, Effects, Requires, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use craft_anything_engines::android_ui::{self, UiError, UiNode, UiTree};
use craft_anything_evidence::{Coverage, ErrorCode, Limitation, ToolError};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::Duration;

pub const UI_TOOL: &str = "adb_ui_show";

/// Controls returned by one call unless told otherwise.
const DEFAULT_LIMIT: usize = 200;
/// The most one call returns.
const MAX_LIMIT: usize = 1000;
/// Longest text filter accepted.
const MAX_FILTER: usize = 200;
/// How long the device gets to describe its screen.
pub(super) const DUMP_TIMEOUT: Duration = Duration::from_secs(45);

pub fn specs() -> Vec<ToolSpec> {
    vec![spec(
        UI_TOOL,
        "Controls on the screen",
        "Read the controls on a device's screen with `uiautomator dump`: for each, its class, text, content description, resource id, state (clickable, checked, enabled, focused, scrollable, selected), bounds in pixels and its parent, parents before children. A control a person can act on also has a label (its own text, or the texts inside it, as on a list row), and a text that can't be tapped names in acts_through the control a tap on it reaches. Paged with offset and limit (default 200, at most 1000); interactive_only keeps what a person can act on, text_contains keeps controls whose text, description or resource id contains the words (case ignored). Only what is on screen now is listed, and the text in a field a person types into (a password above all, even one shown in clear) is never returned: such a field says text_hidden.",
        "List the buttons, text and fields on the phone's screen right now.",
        Toolset::Adb,
        Effects::MUTATES_SESSION | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK | Effects::CARRIES_TARGET_CONTENT,
        Requires::Engine("adb"),
        schema_of::<UiParams>,
        adb_ui_show,
    )]
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UiParams {
    /// Device serial from adb_devices_find (e.g. emulator-5554).
    serial: String,
    /// Skip this many matching controls.
    #[serde(default)]
    offset: usize,
    /// Return at most this many (default 200, at most 1000).
    #[serde(default)]
    limit: Option<usize>,
    /// Only controls a person can act on: tap, hold, tick, scroll or type into.
    #[serde(default)]
    interactive_only: bool,
    /// Only controls whose text, description or resource id contains this (case ignored).
    #[serde(default)]
    text_contains: Option<String>,
}

/// What one call returns, worked out from a parsed screen.
struct Page {
    result: Value,
    summary: String,
    limitations: Vec<Limitation>,
    coverage: Coverage,
}

fn matches(n: &UiNode, interactive_only: bool, needle: Option<&str>) -> bool {
    if interactive_only && !n.interactive() {
        return false;
    }
    match needle {
        Some(needle) => [&n.text, &n.content_desc, &n.resource_id, &n.label].iter().any(|v| v.to_lowercase().contains(needle)),
        None => true,
    }
}

fn page(tree: &UiTree, offset: usize, limit: usize, interactive_only: bool, text_contains: Option<&str>) -> Page {
    let needle = text_contains.map(str::to_lowercase).filter(|s| !s.is_empty());
    let matching: Vec<&UiNode> = tree.nodes.iter().filter(|n| matches(n, interactive_only, needle.as_deref())).collect();
    let shown: Vec<&UiNode> = matching.iter().skip(offset).take(limit).copied().collect();
    let packages: BTreeSet<&str> = tree.nodes.iter().map(|n| n.package.as_str()).filter(|p| !p.is_empty()).take(4096).collect();
    let interactive = tree.nodes.iter().filter(|n| n.interactive()).count();
    let with_text = tree.nodes.iter().filter(|n| !n.text.is_empty() || !n.content_desc.is_empty()).count();
    let hidden = tree.nodes.iter().filter(|n| n.text_hidden).count();
    let screen = tree.nodes.iter().find(|n| n.parent.is_none()).and_then(|n| n.bounds);

    let mut limitations = vec![Limitation::new(
        "visible_only",
        "Only what is on screen now is listed. A web page, a game or a custom-drawn view can appear as one control with nothing inside it.",
    )];
    if hidden > 0 {
        limitations.push(Limitation::new(
            "typed_text_hidden",
            format!("{hidden} field(s) a person types into are listed without their text (text_hidden); their hint and description are kept."),
        ));
    }
    if tree.dropped > 0 {
        limitations.push(Limitation::new(
            "truncated",
            format!(
                "{} control(s) were left out: a screen is read up to {} controls and {} levels deep.",
                tree.dropped,
                android_ui::MAX_NODES,
                android_ui::MAX_DEPTH
            ),
        ));
    }
    if tree.incomplete {
        limitations.push(Limitation::new(
            "incomplete_dump",
            "The device's description of the screen ended early or held something unexpected; what came before is listed.",
        ));
    }
    if tree.nodes.iter().any(|n| n.cut) {
        limitations.push(Limitation::new("long_values_cut", format!("Text longer than {} bytes is cut.", android_ui::MAX_VALUE)));
    }

    let filtered = interactive_only || needle.is_some();
    let summary = format!(
        "{} control(s) on screen ({} interactive, {} with text){}{}; showing {}.",
        tree.nodes.len(),
        interactive,
        with_text,
        if filtered { format!(", {} matching", matching.len()) } else { String::new() },
        packages.iter().next().map(|p| format!(" in {p}")).unwrap_or_default(),
        shown.len()
    );
    let mut coverage = Coverage::page(shown.len() as u64, matching.len() as u64, offset as u64);
    if tree.dropped > 0 || tree.incomplete {
        // Controls were left out before paging, so even the last page isn't the whole screen.
        coverage.status = craft_anything_evidence::CoverageStatus::Partial;
    }
    let result = json!({
        "rotation": tree.rotation,
        "screen": screen,
        "packages": packages,
        "total": tree.nodes.len(),
        "interactive": interactive,
        "with_text": with_text,
        "matching": matching.len(),
        "offset": offset,
        "nodes": shown,
        "dropped": tree.dropped,
        "incomplete": tree.incomplete,
    });
    Page { result, summary, limitations, coverage }
}

/// The device's description of its screen, parsed. `adb` should allow [`DUMP_TIMEOUT`].
pub(super) fn read_screen(adb: &craft_anything_engines::adb::Adb, serial: &str) -> Result<UiTree, ToolError> {
    // `/dev/tty` makes uiautomator print the description instead of writing a file on the device.
    let out = adb.exec_out(serial, "uiautomator dump /dev/tty")?;
    let text = out.stdout_text();
    android_ui::parse(&text).map_err(|UiError::NotADump| {
        let said = format!("{}{}", text, out.stderr_text());
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("nothing").trim().chars().take(300).collect::<String>();
        ToolError::new(ErrorCode::ProgramFailed, format!("The device did not describe its screen. It said: {last}")).with_remediation(
            "A screen that never stops moving (a video, an animation) can't be described; wait for it to settle, or unlock the device, and try again.",
        )
    })
}

fn adb_ui_show(ctx: &mut Ctx<'_>, params: Value) -> Result<ToolOutput, ToolError> {
    let p: UiParams = parse_params(params)?;
    let serial = serial_of(&p.serial)?;
    let limit = p.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ToolError::invalid(format!("limit must be between 1 and {MAX_LIMIT}.")));
    }
    if p.text_contains.as_ref().is_some_and(|t| t.len() > MAX_FILTER) {
        return Err(ToolError::invalid(format!("text_contains can be at most {MAX_FILTER} bytes.")));
    }
    let adb = adb_for(ctx)?.timeout(DUMP_TIMEOUT).idle_timeout(DUMP_TIMEOUT);
    ensure_online(&adb, serial)?;
    let tree = read_screen(&adb, serial)?;
    let page = page(&tree, p.offset, limit, p.interactive_only, p.text_contains.as_deref());
    let e = dev_ev(
        ctx,
        UI_TOOL,
        serial,
        json!({"serial": serial, "offset": p.offset, "limit": limit, "interactive_only": p.interactive_only, "text_contains": p.text_contains}),
    )
    .result(page.result)
    .summary(page.summary)
    .limitations(page.limitations)
    .coverage(page.coverage)
    .build()?;
    Ok(ToolOutput::evidence(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use crate::tools::native_common::test_support::call_err;
    use craft_anything_evidence::CoverageStatus;

    fn screen() -> UiTree {
        let dump = concat!(
            "<hierarchy rotation=\"0\">",
            "<node class=\"android.widget.FrameLayout\" package=\"com.example.app\" bounds=\"[0,0][1080,2400]\" enabled=\"true\">",
            "<node class=\"android.widget.TextView\" package=\"com.example.app\" text=\"Sign in\" bounds=\"[40,200][400,260]\" enabled=\"true\" />",
            "<node class=\"android.widget.EditText\" package=\"com.example.app\" resource-id=\"com.example.app:id/user\" text=\"ana\" focusable=\"true\" clickable=\"true\" bounds=\"[40,300][1040,400]\" />",
            "<node class=\"android.widget.EditText\" package=\"com.example.app\" resource-id=\"com.example.app:id/secret\" text=\"hunter2\" password=\"true\" focusable=\"true\" clickable=\"true\" bounds=\"[40,420][1040,520]\" />",
            "<node class=\"android.widget.Button\" package=\"com.example.app\" content-desc=\"Continue\" clickable=\"true\" bounds=\"[40,560][1040,660]\" />",
            "</node></hierarchy>UI hierchary dumped to: /dev/tty"
        );
        android_ui::parse(dump).unwrap()
    }

    fn codes(p: &Page) -> Vec<&str> {
        p.limitations.iter().map(|l| l.code.as_str()).collect()
    }

    #[test]
    fn a_screen_is_summarised_and_listed_without_a_password() {
        let p = page(&screen(), 0, 200, false, None);
        assert_eq!(
            (p.result["total"].as_u64(), p.result["interactive"].as_u64(), p.result["with_text"].as_u64()),
            (Some(5), Some(3), Some(2)),
            "the typed user name no longer counts as text"
        );
        assert_eq!(p.result["screen"]["right"], 1080);
        assert_eq!(p.result["packages"], json!(["com.example.app"]));
        assert_eq!(p.result["nodes"].as_array().map(Vec::len), Some(5));
        assert!(!p.result.to_string().contains("hunter2"));
        assert_eq!(codes(&p), ["visible_only", "typed_text_hidden"]);
        assert!(!p.result.to_string().contains("\"ana\""), "a typed user name is not returned either");
        assert_eq!(p.coverage.status, CoverageStatus::Complete);
        assert!(p.summary.contains("com.example.app"), "{}", p.summary);
    }

    #[test]
    fn filters_and_paging_say_where_the_next_page_starts() {
        let tree = screen();
        let p = page(&tree, 0, 2, true, None);
        assert_eq!(p.result["matching"], 3);
        assert_eq!(p.result["nodes"].as_array().map(Vec::len), Some(2));
        assert_eq!((p.coverage.status, p.coverage.next_offset), (CoverageStatus::Partial, Some(2)));
        let last = page(&tree, 2, 2, true, None);
        assert_eq!(last.result["nodes"][0]["content_desc"], "Continue");
        assert_eq!(last.coverage.status, CoverageStatus::Complete);

        for (needle, expected) in [("SIGN", 1), ("id/user", 1), ("continue", 1), ("nothing", 0), ("hunter2", 0)] {
            assert_eq!(page(&tree, 0, 200, false, Some(needle)).result["matching"], expected, "{needle}");
        }
        assert_eq!(page(&tree, 0, 200, false, Some("")).result["matching"], 5, "an empty filter is no filter");
        // A row whose words are in a child is found by them when only tappable controls are kept.
        let row =
            android_ui::parse("<hierarchy><node class=\"Row\" clickable=\"true\"><node class=\"T\" text=\"Network &amp; internet\" /></node></hierarchy>")
                .unwrap();
        let p = page(&row, 0, 200, true, Some("network"));
        assert_eq!(p.result["matching"], 1);
        assert_eq!(p.result["nodes"][0]["label"], "Network & internet");
        assert_eq!(page(&tree, 99, 200, false, None).result["nodes"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn a_screen_read_in_part_is_never_reported_as_complete() {
        let mut tree = screen();
        tree.dropped = 3;
        let p = page(&tree, 0, 200, false, None);
        assert_eq!(p.coverage.status, CoverageStatus::Partial);
        assert!(codes(&p).contains(&"truncated"));
        let mut tree = screen();
        tree.incomplete = true;
        let p = page(&tree, 0, 200, false, None);
        assert_eq!(p.coverage.status, CoverageStatus::Partial);
        assert!(codes(&p).contains(&"incomplete_dump"));
    }

    #[test]
    fn bad_input_is_refused_before_the_device_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let adb = dir.path().join("adb.exe");
        std::fs::write(&adb, b"stand-in").unwrap();
        let mut s = crate::testing::session();
        s.settings.engines.tools.adb = Some(adb);
        let e = Engine::new(s);
        for params in [
            json!({"serial": "bad serial;"}),
            json!({"serial": "emulator-5554", "limit": 0}),
            json!({"serial": "emulator-5554", "limit": MAX_LIMIT + 1}),
            json!({"serial": "emulator-5554", "text_contains": "x".repeat(MAX_FILTER + 1)}),
            json!({"serial": "emulator-5554", "depth": 3}),
            json!({}),
        ] {
            assert_eq!(call_err(&e, UI_TOOL, params.clone()).code, ErrorCode::BadInput, "{params}");
        }
    }

    #[test]
    fn it_needs_adb() {
        let e = Engine::new(crate::testing::session());
        assert_eq!(call_err(&e, UI_TOOL, json!({"serial": "emulator-5554"})).code, ErrorCode::EngineMissing);
    }
}
