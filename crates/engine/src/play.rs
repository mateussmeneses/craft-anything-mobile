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

/// Every player, in the order they are asked: this computer first, so a project that makes both a
/// program and something else plays as it did unless `on` says otherwise.
pub fn players() -> &'static [&'static dyn Player] {
    &[&Computer]
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
