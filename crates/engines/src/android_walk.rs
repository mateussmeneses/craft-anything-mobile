//! Choosing the next step of an automatic walk through an Android app, from what its screens show.
//!
//! A walk taps each control of the app's first screen once, in reading order, and presses Back
//! after a tap that led somewhere else, so every tap starts from the first screen. It types nothing,
//! never taps a field a person types into, and leaves out controls whose label says they delete,
//! buy, send or sign out ([`RISKY_WORDS`]). It is a pure function over saved control trees (the
//! `step-NN.json` files `adb_walkthrough_record` writes), so the same screens always give the same
//! walk, and it can be tested without a device.

use crate::android_ui::is_editable;
use serde_json::Value;
use std::collections::BTreeSet;

/// Most taps in one walk.
pub const MAX_TAPS: usize = 12;
/// Most controls looked at on one screen.
const MAX_CONTROLS: usize = 2_000;

/// Words in a control's label, id or description that mark a tap as one that could change or
/// spend something. Matched on lower-cased text; errs towards leaving a control out.
pub const RISKY_WORDS: &[&str] = &[
    "delete",
    "remove",
    "erase",
    "reset",
    "uninstall",
    "buy",
    "purchase",
    "pay",
    "subscribe",
    "order",
    "checkout",
    "sign out",
    "signout",
    "log out",
    "logout",
    "call",
    "send",
    "share",
    "post",
    "publish",
    "excluir",
    "apagar",
    "remover",
    "comprar",
    "pagar",
    "assinar",
    "sair",
    "ligar",
    "enviar",
    "compartilhar",
    "publicar",
    "eliminar",
    "borrar",
    "suscribir",
    "cerrar sesión",
    "llamar",
    "compartir",
];

/// Words that mark a field as one for a secret (the walk's recorder uses the same list).
pub const SECRET_WORDS: &[&str] = &["pass", "pwd", "pin", "secret", "otp", "cvv", "cvc", "security code", "token", "senha", "contraseña"];

/// Why a walk ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// Every control of the first screen that may be tapped was tapped.
    AllTried,
    /// [`MAX_TAPS`] taps were made.
    TapLimit,
    /// Back did not bring the first screen back.
    Lost,
    /// The screen belongs to another app, and Back did not return to this one.
    LeftApp,
    /// The first screen has a field to type into and nothing a walk may tap.
    AsksForText,
}

/// What to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Tap here; `key` names the control, to be added to [`Plan::tried`].
    Tap { x: u32, y: u32, key: String },
    /// Press Back.
    Back,
    /// The walk is over.
    Done(Why),
}

/// What a walk has done so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// The controls already tapped, by [`Next::Tap`]'s `key`.
    pub tried: BTreeSet<String>,
    /// Taps made.
    pub taps: usize,
}

fn flag(n: &Value, k: &str) -> bool {
    n.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn text<'a>(n: &'a Value, k: &str) -> &'a str {
    n.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn nodes(tree: &Value) -> impl Iterator<Item = &Value> {
    tree.get("nodes").and_then(Value::as_array).into_iter().flatten().take(MAX_CONTROLS)
}

/// A control's kind and label, as the replay names it on another app: `Button "Save"`.
fn name(n: &Value) -> String {
    let class = text(n, "class");
    format!("{} \"{}\"", class.rsplit('.').next().unwrap_or_default(), text(n, "label"))
}

/// The controls a person can act on, by kind and label: what makes two captures the same screen.
pub fn controls(tree: &Value) -> BTreeSet<String> {
    nodes(tree).filter(|n| ["clickable", "long_clickable", "checkable", "scrollable", "focusable"].iter().any(|k| flag(n, k))).map(name).collect()
}

/// What tells one screen of an app from another during a walk: the controls a tap, a long press or
/// a tick acts on. Text that only takes focus is left out: a counter's number changes with every
/// tap (Flutter marks such text focusable), and that is the same screen.
fn screen_of(tree: &Value) -> BTreeSet<String> {
    nodes(tree).filter(|n| ["clickable", "long_clickable", "checkable"].iter().any(|k| flag(n, k))).map(name).collect()
}

/// The app a capture shows: the package of its first control that names one.
pub fn package(tree: &Value) -> Option<&str> {
    nodes(tree).map(|n| text(n, "package")).find(|p| !p.is_empty())
}

fn says(n: &Value, words: &[&str]) -> bool {
    ["label", "resource_id", "content_desc", "hint", "text"].iter().any(|k| {
        let v = text(n, k).to_lowercase();
        words.iter().any(|w| v.contains(w))
    })
}

/// A field a person types into, or one that looks like it holds a secret.
fn typed_into(n: &Value) -> bool {
    is_editable(text(n, "class")) || flag(n, "password") || flag(n, "text_hidden") || says(n, SECRET_WORDS)
}

/// Where a tap on a control lands: the middle of its bounds, when it has an area on the screen.
fn centre(n: &Value) -> Option<(u32, u32, i64, i64)> {
    let b = n.get("bounds")?;
    let edge = |k: &str| b.get(k).and_then(Value::as_i64);
    let (l, t, r, bt) = (edge("left")?, edge("top")?, edge("right")?, edge("bottom")?);
    let (w, h) = (r.checked_sub(l)?, bt.checked_sub(t)?);
    if w <= 0 || h <= 0 || l < 0 || t < 0 {
        return None;
    }
    let x = u32::try_from(l.checked_add(w / 2)?).ok()?;
    let y = u32::try_from(t.checked_add(h / 2)?).ok()?;
    Some((x, y, t, l))
}

/// The controls of a screen a walk may tap, in reading order (top to bottom, then left to right),
/// each with its key: its kind and label, numbered when several share them (list rows).
fn candidates(tree: &Value) -> Vec<(String, u32, u32)> {
    let mut found: Vec<(i64, i64, String, u32, u32)> = nodes(tree)
        .filter(|n| flag(n, "clickable") && flag(n, "enabled") && !flag(n, "checkable") && !typed_into(n) && !says(n, RISKY_WORDS))
        .filter_map(|n| centre(n).map(|(x, y, top, left)| (top, left, name(n), x, y)))
        .collect();
    found.sort_by_key(|a| (a.0, a.1));
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    found
        .into_iter()
        .map(|(_, _, name, x, y)| {
            let n = seen.entry(name.clone()).or_insert(0);
            *n = n.saturating_add(1);
            (if *n == 1 { name } else { format!("{name} #{n}") }, x, y)
        })
        .collect()
}

/// The next step. `first` is the capture of the app's first screen and `current` the last one;
/// `app` is the package being walked; `after_back` says the last step was a Back this function
/// asked for.
pub fn next(plan: &Plan, first: &Value, current: &Value, app: &str, after_back: bool) -> Next {
    if package(current) != Some(app) {
        // A share sheet, a browser, a permission dialog: one Back, then give up.
        return if after_back { Next::Done(Why::LeftApp) } else { Next::Back };
    }
    if screen_of(current) != screen_of(first) {
        // The tap led somewhere: go back to the first screen before the next tap.
        return if after_back { Next::Done(Why::Lost) } else { Next::Back };
    }
    if plan.taps >= MAX_TAPS {
        return Next::Done(Why::TapLimit);
    }
    let all = candidates(current);
    match all.iter().find(|(key, _, _)| !plan.tried.contains(key)) {
        Some((key, x, y)) => Next::Tap { x: *x, y: *y, key: key.clone() },
        None if all.is_empty() && nodes(current).any(typed_into) => Next::Done(Why::AsksForText),
        None => Next::Done(Why::AllTried),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(class: &str, label: &str, top: i64, left: i64) -> Value {
        json!({
            "class": format!("android.widget.{class}"), "package": "com.me.app", "label": label, "resource_id": "",
            "clickable": true, "enabled": true,
            "bounds": {"left": left, "top": top, "right": left + 200, "bottom": top + 100},
        })
    }

    fn screen(nodes: Vec<Value>) -> Value {
        json!({"nodes": nodes})
    }

    fn with(mut n: Value, k: &str, v: Value) -> Value {
        n[k] = v;
        n
    }

    fn tap(n: &Next) -> (&str, u32, u32) {
        match n {
            Next::Tap { key, x, y } => (key.as_str(), *x, *y),
            other => panic!("not a tap: {other:?}"),
        }
    }

    #[test]
    fn it_taps_the_first_screens_controls_in_reading_order_once_each() {
        let first = screen(vec![node("Button", "Lower", 900, 0), node("Button", "Right", 100, 500), node("Button", "Left", 100, 0)]);
        let mut plan = Plan::default();
        let mut order = Vec::new();
        loop {
            match next(&plan, &first, &first, "com.me.app", false) {
                Next::Tap { key, x, y } => {
                    order.push((key.clone(), x, y));
                    plan.tried.insert(key);
                    plan.taps += 1;
                }
                other => {
                    assert_eq!(other, Next::Done(Why::AllTried));
                    break;
                }
            }
        }
        let names: Vec<&str> = order.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(names, ["Button \"Left\"", "Button \"Right\"", "Button \"Lower\""]);
        assert_eq!((order[0].1, order[0].2), (100, 150), "the middle of its bounds");
    }

    #[test]
    fn rows_with_one_label_are_told_apart_and_each_tapped() {
        let first = screen(vec![node("TextView", "Item", 100, 0), node("TextView", "Item", 300, 0)]);
        let mut plan = Plan::default();
        let a = next(&plan, &first, &first, "com.me.app", false);
        assert_eq!(tap(&a), ("TextView \"Item\"", 100, 150));
        plan.tried.insert("TextView \"Item\"".into());
        let b = next(&plan, &first, &first, "com.me.app", false);
        assert_eq!(tap(&b), ("TextView \"Item\" #2", 100, 350));
    }

    #[test]
    fn fields_secrets_risky_and_unusable_controls_are_never_tapped() {
        let skipped = vec![
            node("EditText", "Name", 0, 0),
            with(node("Button", "Show", 100, 0), "resource_id", json!("app:id/password_toggle")),
            with(node("View", "", 200, 0), "password", json!(true)),
            node("Button", "Delete account", 300, 0),
            node("Button", "Sair", 400, 0),
            with(node("Button", "Share", 500, 0), "label", json!("Compartilhar")),
            with(node("Button", "Off", 600, 0), "enabled", json!(false)),
            with(node("Switch", "Dark mode", 700, 0), "checkable", json!(true)),
            with(node("Button", "No area", 800, 0), "bounds", json!({"left": 5, "top": 5, "right": 5, "bottom": 5})),
            with(node("Button", "No bounds", 900, 0), "bounds", Value::Null),
            with(node("Button", "Off screen", 950, 0), "bounds", json!({"left": -300, "top": 0, "right": -100, "bottom": 50})),
            with(node("TextView", "Just text", 1000, 0), "clickable", json!(false)),
        ];
        let mut all = skipped.clone();
        all.push(node("Button", "About", 1100, 0));
        let first = screen(all);
        let n = next(&Plan::default(), &first, &first, "com.me.app", false);
        assert_eq!(tap(&n).0, "Button \"About\"");
        // With only such controls and a field to type into, the screen asks for text.
        let only = screen(skipped);
        assert_eq!(next(&Plan::default(), &only, &only, "com.me.app", false), Next::Done(Why::AsksForText));
        // With nothing to type into either, there is simply nothing to try.
        let plain = screen(vec![with(node("TextView", "Hello", 0, 0), "clickable", json!(false))]);
        assert_eq!(next(&Plan::default(), &plain, &plain, "com.me.app", false), Next::Done(Why::AllTried));
    }

    #[test]
    fn it_goes_back_after_a_tap_that_led_elsewhere_and_stops_when_back_does_not_return() {
        let first = screen(vec![node("Button", "Open", 0, 0)]);
        let detail = screen(vec![node("Button", "Close", 0, 0)]);
        let other = screen(vec![with(node("Button", "Open", 0, 0), "package", json!("com.android.chrome"))]);
        let plan = Plan { tried: ["Button \"Open\"".to_string()].into(), taps: 1 };
        assert_eq!(next(&plan, &first, &detail, "com.me.app", false), Next::Back);
        assert_eq!(next(&plan, &first, &detail, "com.me.app", true), Next::Done(Why::Lost));
        assert_eq!(next(&plan, &first, &other, "com.me.app", false), Next::Back);
        assert_eq!(next(&plan, &first, &other, "com.me.app", true), Next::Done(Why::LeftApp));
        // Back on the first screen: the walk carries on (here, nothing is left).
        assert_eq!(next(&plan, &first, &first, "com.me.app", true), Next::Done(Why::AllTried));
        // A text that changed without changing what can be acted on is the same screen.
        let counted = screen(vec![node("Button", "Open", 0, 0), with(node("TextView", "1", 300, 0), "clickable", json!(false))]);
        assert_eq!(next(&plan, &first, &counted, "com.me.app", false), Next::Done(Why::AllTried));
    }

    /// Seen live on a Flutter counter: its number is focusable text, so every tap changed the
    /// controls, the walk pressed Back and Back closed the app after one tap.
    #[test]
    fn focusable_text_that_changes_is_the_same_screen() {
        let number = |n: &str| with(with(node("View", n, 300, 0), "clickable", json!(false)), "focusable", json!(true));
        let first = screen(vec![node("Button", "Increment", 900, 0), number("0")]);
        let counted = screen(vec![node("Button", "Increment", 900, 0), number("1")]);
        assert_ne!(controls(&first), controls(&counted), "the fixture's text does change");
        let plan = Plan { tried: ["Button \"Increment\"".to_string()].into(), taps: 1 };
        assert_eq!(next(&plan, &first, &counted, "com.me.app", false), Next::Done(Why::AllTried));
        assert_eq!(tap(&next(&Plan::default(), &first, &counted, "com.me.app", false)).0, "Button \"Increment\"");
    }

    #[test]
    fn it_stops_at_the_tap_limit_and_never_panics_on_odd_trees() {
        let first = screen((0..40).map(|i| node("Button", &format!("B{i}"), i * 100, 0)).collect());
        let plan = Plan { tried: BTreeSet::new(), taps: MAX_TAPS };
        assert_eq!(next(&plan, &first, &first, "com.me.app", false), Next::Done(Why::TapLimit));
        for odd in [json!(null), json!({}), json!({"nodes": 3}), json!({"nodes": [1, "x", {"bounds": {"left": "a"}}, {"class": 7, "clickable": "yes"}]})] {
            // No package named: it is not this app's screen.
            assert_eq!(next(&Plan::default(), &odd, &odd, "com.me.app", false), Next::Back);
            assert!(controls(&odd).len() <= 4);
        }
        let huge = screen(vec![with(node("Button", "Far", 0, 0), "bounds", json!({"left": 0, "top": i64::MAX - 1, "right": i64::MAX, "bottom": i64::MAX}))]);
        assert!(matches!(next(&Plan::default(), &huge, &huge, "com.me.app", false), Next::Done(_)));
    }
}
