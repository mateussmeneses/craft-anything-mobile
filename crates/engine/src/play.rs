//! What Play starts: a registry of players, each of which knows one place a finished build runs.
//!
//! `project_play` picks a player (the one `on` names, else the first that [`Player::finds`]
//! something in the app folder) and calls [`Player::play`]. [`Computer`] starts a program on this
//! computer (games, desktop apps, Godot and Electron projects); a fork adds a player (an iOS
//! simulator, say) by implementing the trait and listing it in [`players`], without touching the
//! tool.

use crate::tool::Ctx;
use crate::workspace::Workspace;
use craft_anything_evidence::{ErrorCode, ToolError};
use serde_json::Value;
use std::path::Path;

/// What the caller asked of Play, beyond the project.
#[derive(Debug, Clone, Default)]
pub struct PlayRequest {
    /// Start another copy even if one is running (programs on this computer).
    pub another: bool,
    /// The device to use (a serial or a virtual device's name), already validated.
    pub device: Option<String>,
    /// Start a virtual device without a window.
    pub no_window: bool,
}

/// One place a finished build can be played.
pub trait Player: Send + Sync {
    /// Its id, the value of `project_play`'s `on`.
    fn id(&self) -> &'static str;
    /// Whether `app` holds something this player starts. Reads files only: cheap and bounded, so
    /// the UI's worker can ask it.
    fn finds(&self, app: &Path, godot: Option<&Path>) -> bool;
    /// Start it. The result is a JSON object with typed fields; `on` is added by the caller.
    fn play(&self, ctx: &mut Ctx<'_>, ws: &Workspace, req: &PlayRequest) -> Result<Value, ToolError>;
}

/// Programs on this computer: what [`crate::outputs::launch`] finds, started through the
/// session's run list.
pub struct Computer;

impl Computer {
    pub const ID: &'static str = "computer";
}

impl Player for Computer {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn finds(&self, app: &Path, godot: Option<&Path>) -> bool {
        crate::outputs::launch(app, godot).is_some()
    }

    fn play(&self, ctx: &mut Ctx<'_>, ws: &Workspace, req: &PlayRequest) -> Result<Value, ToolError> {
        crate::tools::pin_tools::start(ctx, &ws.app_dir(), &crate::pins::play_log(ws), req.another, "the finished project")
    }
}

/// An Android build: its debug APK installed on a device ([`crate::android_device::choose`]) and
/// started there. A virtual device is started when nothing is online, with a window unless
/// `no_window`; one this session started without a window is started again with one, so the app
/// doesn't open where nobody can see it.
pub struct Android;

impl Android {
    pub const ID: &'static str = "android";
}

/// Why a device was used, as the result says it.
fn how_word(how: crate::android_device::How) -> Value {
    serde_json::to_value(how).unwrap_or(Value::Null)
}

/// Start the virtual device `avd` for Play and return its serial once it has booted.
fn start_device(ctx: &mut Ctx<'_>, avd: &str, no_window: bool) -> Result<String, ToolError> {
    use crate::tools::emulator_tools::{DEFAULT_WAIT_SECS, start_emulator};
    ctx.report(None, format!("Starting {avd}…"));
    let options = craft_anything_engines::emulator::StartOptions { no_window, cold_boot: false };
    let s = start_emulator(ctx, avd, options, DEFAULT_WAIT_SECS, None)?;
    match (s.serial, s.booted) {
        (Some(serial), true) => Ok(serial),
        (Some(serial), false) => {
            Err(ToolError::new(ErrorCode::TimedOut, format!("{avd} is running as {serial} but had not finished booting after {} s.", s.waited_seconds))
                .with_remediation("Wait for it to finish starting, then play again."))
        }
        (None, _) => Err(ToolError::new(ErrorCode::TimedOut, format!("{avd} was started but had not appeared in adb after {} s.", s.waited_seconds))
            .with_remediation("Wait for it to finish starting, then play again; its log is in the logs folder beside config.toml.")),
    }
}

impl Player for Android {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn finds(&self, app: &Path, _godot: Option<&Path>) -> bool {
        crate::outputs::debug_apk(app).is_some()
    }

    fn play(&self, ctx: &mut Ctx<'_>, ws: &Workspace, req: &PlayRequest) -> Result<Value, ToolError> {
        use crate::android_device::{self, Choice, DeviceRef};
        use crate::tools::{adb_tools, emulator_tools};
        let app = ws.app_dir();
        let Some(apk) = crate::outputs::debug_apk(&app) else {
            return Err(ToolError::new(ErrorCode::NotFound, "No debug APK was built yet in app/.")
                .with_remediation("Build it with the stack's own build so it lands under build/outputs/…/debug/ (a rebuild does)."));
        };
        let bytes = std::fs::read(&apk).map_err(|e| ToolError::io(apk.display(), e))?;
        let Some(package) = crate::outputs::apk_package(&bytes) else {
            return Err(ToolError::new(ErrorCode::NotFound, format!("{} has no readable AndroidManifest.xml naming a package.", apk.display())));
        };
        let package = craft_anything_engines::adb::validate_package(&package)?.to_string();
        drop(bytes);

        let seen = emulator_tools::seen_now(ctx)?;
        let default = android_device::load(&android_device::path_in(&ctx.session.config_dir())).file.default;
        let asked = req.device.as_deref().map(|d| DeviceRef::parse(d, &seen)).transpose()?;
        let chosen = android_device::choose(&seen, asked.as_ref(), default.as_ref());
        let mut restarted = false;
        let mut started = false;
        let (serial, avd, how) = match chosen.choice {
            Choice::Use { serial, avd: Some(name), how } if !req.no_window && emulator_tools::started_headless(ctx, &serial) => {
                // Started by a check where nobody can see it: start it again with a window.
                ctx.report(None, format!("{name} is running without a window; starting it again with one…"));
                emulator_tools::stop_emulator(ctx, &serial)?;
                restarted = true;
                (start_device(ctx, &name, false)?, Some(name), how)
            }
            Choice::Use { serial, avd, how } => (serial, avd, how),
            Choice::Start { avd, how } => {
                started = true;
                (start_device(ctx, &avd, req.no_window)?, Some(avd), how)
            }
            Choice::Choose { candidates } => {
                return Err(ToolError::invalid(format!("{} devices are connected; say which one to play on.", candidates.len()))
                    .with_remediation("Pass device (a serial or a virtual device's name from the list), or set a default with adb_default_device_set.")
                    .with_details(serde_json::json!({"reason": "device_choice_needed", "devices": candidates})));
            }
            Choice::None { why } => return Err(why.error()),
        };

        let adb = adb_tools::adb_for(ctx)?;
        ctx.report(None, format!("Installing {package} on {serial}…"));
        let installed = adb_tools::install_apk(&adb, &serial, &apk, true)?;
        adb_tools::launch_package(&adb, &serial, &package)?;
        Ok(serde_json::json!({
            "serial": serial,
            "avd": avd,
            "device_how": how_word(how),
            "started_device": started,
            "restarted_with_window": restarted,
            "default_missing": chosen.default_missing,
            "no_window": req.no_window,
            "package": package,
            "apk": apk.display().to_string(),
            "installed": true,
            "started": true,
            "install_output": installed,
        }))
    }
}

/// Every player, in the order they are asked: this computer first, so a project that makes both a
/// program and an APK (Kotlin Multiplatform with a desktop target) plays as it did unless `on`
/// says otherwise.
pub fn players() -> &'static [&'static dyn Player] {
    &[&Computer, &Android]
}

/// The player `id` names.
pub fn by_id(id: &str) -> Option<&'static dyn Player> {
    players().iter().copied().find(|p| p.id() == id)
}

/// Every player's id.
pub fn ids() -> Vec<&'static str> {
    players().iter().map(|p| p.id()).collect()
}

/// The first player that finds something to start in `app`.
pub fn found(app: &Path, godot: Option<&Path>) -> Option<&'static str> {
    players().iter().find(|p| p.finds(app, godot)).map(|p| p.id())
}

/// The player `on` names, or a `BadInput` that lists them.
pub fn named(on: &str) -> Result<&'static dyn Player, ToolError> {
    by_id(on).ok_or_else(|| {
        let ids = ids();
        ToolError::invalid(format!("There is no player named {on:?}. Players: {}.", ids.join(", "))).with_details(serde_json::json!({"players": ids}))
    })
}

/// The error for a player that finds nothing to start.
pub fn nothing_to_start(player: &str) -> ToolError {
    ToolError::new(ErrorCode::NotFound, format!("There's nothing for {player} to start in this project yet."))
        .with_remediation("Finish a build first (rebuild), or leave out `on` to use what the project made.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computers_player_finds_a_built_program_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(found(d.path(), None), None, "nothing built");
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"game\"\n").unwrap();
        std::fs::create_dir_all(d.path().join("target/debug")).unwrap();
        let program = d.path().join("target/debug").join(if cfg!(windows) { "game.exe" } else { "game" });
        std::fs::write(&program, b"stand-in").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(found(d.path(), None), Some(Computer::ID));
    }

    /// An app folder with a debug APK where Gradle (or Flutter, with `flutter`) puts it.
    fn android_app(app: &Path, flutter: bool) {
        let apk = if flutter { app.join("build/app/outputs/flutter-apk/app-debug.apk") } else { app.join("app/build/outputs/apk/debug/app-debug.apk") };
        std::fs::create_dir_all(apk.parent().unwrap()).unwrap();
        std::fs::write(&apk, craft_anything_testkit::android::notes_apk()).unwrap();
    }

    #[test]
    fn an_apk_is_played_on_android_and_a_program_beside_it_on_this_computer() {
        for flutter in [false, true] {
            let d = tempfile::tempdir().unwrap();
            android_app(d.path(), flutter);
            assert_eq!(found(d.path(), None), Some(Android::ID), "flutter={flutter}");
        }
        // A project that also built a program for this computer (a jar with a Main-Class, or a
        // Rust program) plays it here unless `on` says android.
        let d = tempfile::tempdir().unwrap();
        android_app(d.path(), false);
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname = \"game\"\n").unwrap();
        std::fs::create_dir_all(d.path().join("target/debug")).unwrap();
        let program = d.path().join("target/debug").join(if cfg!(windows) { "game.exe" } else { "game" });
        std::fs::write(&program, b"stand-in").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(found(d.path(), None), Some(Computer::ID));
        assert!(Android.finds(d.path(), None));
    }

    /// Calls project_play on a thread with a time limit, so a lock or a wait can't hang the test.
    fn play_within_limit(e: std::sync::Arc<crate::Engine>, params: Value, consent: bool) -> Result<Value, ToolError> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut opts = crate::CallOptions::new(crate::Caller::Mcp);
            if consent {
                opts = opts.with_consent(crate::Consent::Granted);
            }
            let _ = tx.send(e.call("project_play", params, opts).map(|o| o.to_json()["result"].clone()));
        });
        rx.recv_timeout(std::time::Duration::from_secs(30)).expect("project_play returned within 30 s")
    }

    #[test]
    fn android_play_asks_first_and_refuses_bad_input_before_running_anything() {
        use crate::workspace::TargetRef;
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Notes", "An Android notes app", TargetRef::None).unwrap();
        android_app(&ws.app_dir(), false);
        // A stand-in where adb would be: none of these calls gets far enough to run it.
        let adb = d.path().join(if cfg!(windows) { "adb.exe" } else { "adb" });
        std::fs::write(&adb, b"stand-in").unwrap();
        let mut s = crate::testing::session();
        s.settings.deep_mode = false; // Play runs in Clean room: it installs the builder's own APK.
        s.settings.engines.tools.adb = Some(adb.clone());
        let e = std::sync::Arc::new(crate::Engine::new(s));
        let root = ws.root.display().to_string();

        let err = play_within_limit(e.clone(), serde_json::json!({"project": root, "on": "android"}), false).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConsentRequired, "it changes a device: {err:?}");
        let err = play_within_limit(e.clone(), serde_json::json!({"project": root, "on": "nope"}), true).unwrap_err();
        assert_eq!(err.code, ErrorCode::BadInput);
        assert!(err.message.contains("android") && err.message.contains("computer"), "{}", err.message);
        for bad in ["a b", "a;rm", ""] {
            let err = play_within_limit(e.clone(), serde_json::json!({"project": root, "device": bad}), true).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadInput, "{bad:?}: {err:?}");
        }

        // Without adb, the Android player says what's missing, having started nothing.
        let mut s = crate::testing::session();
        s.settings.deep_mode = false;
        let e = std::sync::Arc::new(crate::Engine::new(s));
        let err = play_within_limit(e.clone(), serde_json::json!({"project": root}), true).unwrap_err();
        assert_eq!(err.code, ErrorCode::EngineMissing, "{err:?}");
        assert!(!e.session().config_dir().join("logs").exists(), "no emulator was started");

        // A project that built only for this computer has nothing for Android.
        let host = Workspace::create(d.path(), "Game", "A game", TargetRef::None).unwrap();
        let err = play_within_limit(e, serde_json::json!({"project": host.root, "on": "android"}), true).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
    }

    #[test]
    fn players_are_named_by_id_and_an_unknown_one_lists_them() {
        assert_eq!(by_id("computer").map(|p| p.id()), Some("computer"));
        assert!(ids().contains(&"computer"));
        let e = named("nope").err().unwrap();
        assert_eq!(e.code, ErrorCode::BadInput);
        assert!(e.message.contains("computer"), "{}", e.message);
        assert_eq!(e.details.unwrap()["players"][0], "computer");
    }
}
