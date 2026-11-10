//! What an Android device says about the apps running on it: the activity on screen and the
//! back stack (`dumpsys activity activities`), and an app's permissions (`dumpsys package <pkg>`).
//!
//! Both are text meant for people, so these readers take only the lines they know, ignore the rest,
//! and are bounded: at most [`MAX_TASKS`] tasks with [`MAX_ACTIVITIES`] activities each, and
//! [`MAX_PERMISSIONS`] permissions. Shapes were taken from Android 17 (API 37).

use serde::Serialize;
use std::collections::BTreeSet;

/// Most tasks kept.
pub const MAX_TASKS: usize = 64;
/// Most activities kept in one task.
pub const MAX_ACTIVITIES: usize = 64;
/// Most permissions kept.
pub const MAX_PERMISSIONS: usize = 2_000;
/// Longest line looked at.
const MAX_LINE: usize = 4_096;
/// Most lines looked at.
const MAX_LINES: usize = 400_000;

/// One activity: the component as the device writes it, split into package and class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityRef {
    /// `com.android.settings/.Settings$WifiSettingsActivity`
    pub component: String,
    pub package: String,
    /// The class, with a leading `.` expanded to the package: `com.android.settings.Settings$WifiSettingsActivity`.
    pub class: String,
    /// The task it is in (`t13`), when the record says.
    pub task: Option<u32>,
}

/// A task: a stack of activities the back button walks down, top first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Task {
    pub id: u32,
    /// `standard`, `home`, `undefined`…
    pub kind: Option<String>,
    /// The task's affinity (`A=1000:com.android.settings`), usually its app.
    pub affinity: Option<String>,
    pub visible: Option<bool>,
    /// Activities, top of the stack first.
    pub activities: Vec<ActivityRef>,
    /// Each activity's place (`Hist #n`, 0 at the bottom), for ordering.
    #[serde(skip)]
    places: Vec<u32>,
}

/// The activities on a device, as `dumpsys activity activities` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ActivityStack {
    /// The activity on screen and taking input.
    pub resumed: Option<ActivityRef>,
    /// Tasks, top first, across displays.
    pub tasks: Vec<Task>,
    /// Tasks or activities were left out because of the caps.
    pub truncated: bool,
}

impl ActivityStack {
    /// The tasks an app's activities are in, top first.
    pub fn tasks_of<'a>(&'a self, package: &'a str) -> impl Iterator<Item = &'a Task> + 'a {
        self.tasks.iter().filter(move |t| t.activities.iter().any(|a| a.package == package))
    }
}

/// `ActivityRecord{237441405 u0 com.android.settings/.Settings$WifiSettingsActivity t13}`, from
/// anywhere in a line.
pub fn parse_activity_record(line: &str) -> Option<ActivityRef> {
    let start = line.find("ActivityRecord{")?;
    let inner = line.get(start.checked_add("ActivityRecord{".len())?..)?;
    let inner = inner.get(..inner.find('}')?)?;
    let mut component = None;
    let mut task = None;
    for word in inner.split_whitespace().take(8) {
        if word.contains('/') && component.is_none() {
            component = Some(word);
        } else if let Some(n) = word.strip_prefix('t').and_then(|n| n.parse::<u32>().ok()) {
            task = Some(n);
        }
    }
    let component = component?;
    let (package, class) = component.split_once('/')?;
    if !valid_name(package) || class.is_empty() || !class.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'$')) {
        return None;
    }
    let class = if class.starts_with('.') { format!("{package}{class}") } else { class.to_string() };
    Some(ActivityRef { component: component.to_string(), package: package.to_string(), class, task })
}

/// A Java package or permission name: letters, digits, `.` and `_`.
fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
}

/// `* Task{1a4b872 #13 type=standard A=1000:com.android.settings U=0 visible=true …}`.
fn parse_task(line: &str) -> Option<Task> {
    let inner = line.get(line.find("Task{")?.checked_add(5)?..)?;
    let inner = inner.get(..inner.find('}')?)?;
    let mut task = Task { id: 0, kind: None, affinity: None, visible: None, activities: Vec::new(), places: Vec::new() };
    let mut has_id = false;
    for word in inner.split_whitespace().take(32) {
        if let Some(id) = word.strip_prefix('#').and_then(|n| n.parse::<u32>().ok()) {
            task.id = id;
            has_id = true;
        } else if let Some(v) = word.strip_prefix("type=") {
            task.kind = Some(v.chars().take(64).collect());
        } else if let Some(v) = word.strip_prefix("A=") {
            task.affinity = Some(v.chars().take(256).collect());
        } else if let Some(v) = word.strip_prefix("visible=") {
            task.visible = v.parse::<bool>().ok();
        }
    }
    has_id.then_some(task)
}

/// Read `dumpsys activity activities`. Only the per-display listing is read (from each
/// `Display #` line to its `ResumedActivity:`); the window hierarchy printed after it repeats
/// the same tasks and is skipped.
pub fn parse_activities(text: &str) -> ActivityStack {
    let mut stack = ActivityStack::default();
    let mut listing = false;
    for line in text.lines().take(MAX_LINES) {
        let line = line.get(..line.len().min(MAX_LINE)).unwrap_or_default();
        let t = line.trim_start();
        if t.starts_with("Display #") {
            listing = true;
            continue;
        }
        if t.starts_with("ResumedActivity:") || t.starts_with("mResumedActivity:") {
            if stack.resumed.is_none() {
                stack.resumed = parse_activity_record(t);
            }
            listing = false;
            continue;
        }
        if !listing {
            continue;
        }
        if t.starts_with("* Task{") {
            match parse_task(t) {
                Some(task) if stack.tasks.len() < MAX_TASKS => stack.tasks.push(task),
                Some(_) => stack.truncated = true,
                None => {}
            }
        } else if t.starts_with("* Hist") {
            if let (Some(task), Some(a)) = (stack.tasks.last_mut(), parse_activity_record(t)) {
                if task.activities.len() < MAX_ACTIVITIES {
                    let place = t.split('#').nth(1).and_then(|s| s.split(':').next()).and_then(|n| n.trim().parse::<u32>().ok()).unwrap_or(0);
                    task.activities.push(a);
                    task.places.push(place);
                } else {
                    stack.truncated = true;
                }
            }
        } else if t.starts_with("topResumedActivity=") && stack.resumed.is_none() {
            stack.resumed = parse_activity_record(t);
        }
    }
    // `Hist #n` counts up from the bottom of the stack; keep the top first, whatever order they
    // were printed in (a stable sort keeps the printed order between equal places).
    for task in &mut stack.tasks {
        let mut both: Vec<(u32, ActivityRef)> = task.places.drain(..).zip(task.activities.drain(..)).collect();
        both.sort_by_key(|a| std::cmp::Reverse(a.0));
        task.activities = both.into_iter().map(|(_, a)| a).collect();
    }
    stack
}

/// Where a permission is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    /// Granted at install, never asked about.
    Install,
    /// Asked of the person at run time (camera, location…).
    Runtime,
}

/// One permission and whether the device says it is granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Permission {
    pub name: String,
    pub kind: PermissionKind,
    pub granted: Option<bool>,
}

/// An app's permissions, as `dumpsys package` lists them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Permissions {
    /// What the app asks for in its manifest.
    pub requested: Vec<String>,
    /// What the device has decided, install and run-time.
    pub held: Vec<Permission>,
    /// The listing named this package (`Package [<pkg>]`); when false, nothing above is about it.
    pub found: bool,
    pub truncated: bool,
}

/// Read `dumpsys package <pkg>`'s permissions. Only its `Packages:` and `Shared users:` sections
/// are read (an app with a shared user id keeps its run-time permissions under the shared user).
pub fn parse_permissions(text: &str, package: &str) -> Permissions {
    #[derive(Clone, Copy, PartialEq)]
    enum In {
        Requested,
        Held(PermissionKind),
    }
    let mut out = Permissions::default();
    let mut requested = BTreeSet::new();
    let mut held = BTreeSet::new();
    let mut section_ok = false;
    let mut sub: Option<(In, usize)> = None;
    let marker = format!("Package [{package}]");
    for line in text.lines().take(MAX_LINES) {
        let line = line.get(..line.len().min(MAX_LINE)).unwrap_or_default();
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if indent == 0 {
            section_ok = t == "Packages:" || t == "Shared users:";
            sub = None;
            continue;
        }
        if !section_ok {
            continue;
        }
        if t.starts_with(&marker) {
            out.found = true;
        }
        let header = match t {
            "requested permissions:" => Some(In::Requested),
            "install permissions:" => Some(In::Held(PermissionKind::Install)),
            "runtime permissions:" => Some(In::Held(PermissionKind::Runtime)),
            _ => None,
        };
        if let Some(h) = header {
            sub = Some((h, indent));
            continue;
        }
        match sub {
            Some((what, header_indent)) if indent > header_indent => {
                let (name, rest) = t.split_once(':').unwrap_or((t, ""));
                let name = name.trim();
                if !valid_name(name) {
                    continue;
                }
                if out.requested.len().saturating_add(out.held.len()) >= MAX_PERMISSIONS {
                    out.truncated = true;
                    continue;
                }
                match what {
                    In::Requested => {
                        if requested.insert(name.to_string()) {
                            out.requested.push(name.to_string());
                        }
                    }
                    In::Held(kind) => {
                        if held.insert((kind, name.to_string())) {
                            let granted = rest.split([',', ' ']).find_map(|w| w.strip_prefix("granted=")).and_then(|v| v.parse::<bool>().ok());
                            out.held.push(Permission { name: name.to_string(), kind, granted });
                        }
                    }
                }
            }
            Some(_) => sub = None,
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIVITIES: &str = "ACTIVITY MANAGER ACTIVITIES (dumpsys activity activities)
Display #0 (activities from top to bottom):
  * Task{1a4b872 #13 type=standard A=1000:com.android.settings U=0 visible=true visibleRequested=true mode=fullscreen translucent=false sz=2}
    topResumedActivity=ActivityRecord{237441405 u0 com.android.settings/.Settings$WifiSettingsActivity t13}
    * Hist  #1: ActivityRecord{237441405 u0 com.android.settings/.Settings$WifiSettingsActivity t13}
    * Hist  #0: ActivityRecord{204539030 u0 com.android.settings/.homepage.SettingsHomepageActivity t13}
  * Task{7e0ec4e #1 type=home U=0 visible=false visibleRequested=false mode=fullscreen translucent=true sz=1}
    * Task{97e7b49 #2 type=home I=com.google.android.apps.nexuslauncher/.NexusLauncherActivity U=0 rootTaskId=1 visible=false sz=2}
      * Hist  #1: ActivityRecord{206261584 u0 com.google.android.apps.nexuslauncher/com.google.android.apps.nexuslauncher.NexusLauncherActivity t2}

  ResumedActivity: ActivityRecord{237441405 u0 com.android.settings/.Settings$WifiSettingsActivity t13}
  mFocusedApp=ActivityRecord{237441405 u0 com.android.settings/.Settings$WifiSettingsActivity t13}
  Window hierarchy:
      * Task{1a4b872 #13 type=standard A=1000:com.android.settings U=0 visible=true sz=1}
        * Hist  #0: ActivityRecord{1 u0 com.example/.Ghost t13}
";

    #[test]
    fn the_resumed_activity_and_the_back_stack_are_read_top_first() {
        let s = parse_activities(ACTIVITIES);
        let r = s.resumed.as_ref().unwrap();
        assert_eq!((r.package.as_str(), r.class.as_str(), r.task), ("com.android.settings", "com.android.settings.Settings$WifiSettingsActivity", Some(13)));
        assert_eq!(s.tasks.iter().map(|t| t.id).collect::<Vec<_>>(), [13, 1, 2], "the repeated listing after ResumedActivity is skipped");
        let top = &s.tasks[0];
        assert_eq!((top.kind.as_deref(), top.affinity.as_deref(), top.visible), (Some("standard"), Some("1000:com.android.settings"), Some(true)));
        let names: Vec<&str> = top.activities.iter().map(|a| a.class.as_str()).collect();
        assert_eq!(names, ["com.android.settings.Settings$WifiSettingsActivity", "com.android.settings.homepage.SettingsHomepageActivity"]);
        assert_eq!(s.tasks[2].activities[0].class, "com.google.android.apps.nexuslauncher.NexusLauncherActivity", "a full class name is kept as it is");
        assert_eq!(s.tasks_of("com.android.settings").count(), 1);
        let flipped = parse_activities(
            "Display #0
  * Task{a #1 type=standard}
    * Hist  #0: ActivityRecord{1 u0 a/.Bottom t1}
    * Hist  #1: ActivityRecord{2 u0 a/.Top t1}
",
        );
        assert_eq!(flipped.tasks[0].activities.iter().map(|a| a.class.as_str()).collect::<Vec<_>>(), ["a.Top", "a.Bottom"]);
        assert!(!s.truncated);
    }

    #[test]
    fn records_that_are_not_well_formed_are_left_out() {
        for bad in [
            "ActivityRecord{1 u0 nothing t1}",
            "ActivityRecord{1 u0 a b/c t1",
            "ActivityRecord{1 u0 /x t1}",
            "ActivityRecord{1 u0 a/ t1}",
            "ActivityRecord{1 u0 a;b/.X t1}",
            "ActivityRecord{1 u0 a/.X;rm t1}",
            "",
        ] {
            assert_eq!(parse_activity_record(bad), None, "{bad}");
        }
        assert_eq!(parse_activity_record("ActivityRecord{1 u0 a.b/.C}").and_then(|a| a.task), None);
        let s = parse_activities("Display #0\n  * Hist  #0: ActivityRecord{1 u0 a/.B t1}\n  * Task{x #notanumber}\n");
        assert!(s.tasks.is_empty(), "an activity before any task, and a task without an id, are not kept");
    }

    #[test]
    fn the_caps_hold_and_say_so() {
        let mut text = String::from("Display #0\n");
        for i in 0..MAX_TASKS + 3 {
            text.push_str(&format!("  * Task{{a #{i} type=standard}}\n"));
        }
        let s = parse_activities(&text);
        assert_eq!((s.tasks.len(), s.truncated), (MAX_TASKS, true));
        let mut text = String::from("Display #0\n  * Task{a #1 type=standard}\n");
        for i in 0..MAX_ACTIVITIES + 2 {
            text.push_str(&format!("    * Hist  #{i}: ActivityRecord{{1 u0 a/.A{i} t1}}\n"));
        }
        let s = parse_activities(&text);
        assert_eq!((s.tasks[0].activities.len(), s.truncated), (MAX_ACTIVITIES, true));
        let long = format!("Display #0\n  * Task{{a #1 {}}}\n", "x".repeat(100_000));
        assert!(parse_activities(&long).tasks.is_empty(), "a line past the cap is cut, and a cut task has no closing brace");
    }

    const PACKAGE: &str = "Activity Resolver Table:
  Non-Data Actions:
      android.permission.FAKE_IN_RESOLVER: granted=true
Packages:
  Package [com.example.app] (bbf0ae3):
    requested permissions:
      android.permission.CAMERA
      android.permission.INTERNET
      android.permission.ACCESS_FINE_LOCATION: restricted=true
    install permissions:
      android.permission.INTERNET: granted=true
    User 0: ceDataInode=1 installed=true
      runtime permissions:
        android.permission.CAMERA: granted=false, flags=[ USER_SET|USER_SENSITIVE_WHEN_GRANTED]
        android.permission.ACCESS_FINE_LOCATION: granted=true, flags=[ USER_SET]
    enabledComponents:
      com.example.app.Thing
Queries:
  android.permission.NOT_THIS: granted=true
Shared users:
  SharedUser [android.uid.system] (3b1):
    install permissions:
      android.permission.INTERNET: granted=true
      android.permission.WRITE_SETTINGS: granted=true
";

    #[test]
    fn requested_and_held_permissions_are_read_from_their_sections_only() {
        let p = parse_permissions(PACKAGE, "com.example.app");
        assert!(p.found);
        assert_eq!(p.requested, ["android.permission.CAMERA", "android.permission.INTERNET", "android.permission.ACCESS_FINE_LOCATION"]);
        let held: Vec<(&str, PermissionKind, Option<bool>)> = p.held.iter().map(|x| (x.name.as_str(), x.kind, x.granted)).collect();
        assert_eq!(
            held,
            [
                ("android.permission.INTERNET", PermissionKind::Install, Some(true)),
                ("android.permission.CAMERA", PermissionKind::Runtime, Some(false)),
                ("android.permission.ACCESS_FINE_LOCATION", PermissionKind::Runtime, Some(true)),
                ("android.permission.WRITE_SETTINGS", PermissionKind::Install, Some(true)),
            ],
            "a permission listed twice is kept once, and nothing outside Packages and Shared users is read"
        );
        assert!(!p.truncated);
        assert!(!parse_permissions(PACKAGE, "com.other").found);
    }

    #[test]
    fn hostile_permission_lines_are_skipped_and_capped() {
        let mut text = String::from("Packages:\n  Package [a] (1):\n    requested permissions:\n");
        text.push_str("      bad name with spaces\n      $(reboot)\n");
        for i in 0..MAX_PERMISSIONS + 5 {
            text.push_str(&format!("      p.P{i}\n"));
        }
        let p = parse_permissions(&text, "a");
        assert_eq!((p.requested.len(), p.truncated), (MAX_PERMISSIONS, true));
        assert!(p.requested.iter().all(|n| n.starts_with("p.P")));
        let odd = parse_permissions("Packages:\n  Package [a]:\n    install permissions:\n      a.B: granted=maybe\n", "a");
        assert_eq!(odd.held[0].granted, None, "an answer that isn't true or false is unknown");
    }
}
