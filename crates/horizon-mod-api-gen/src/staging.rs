//! One source staging pass for the launcher and `horizon-stage`.
pub use crate::sites::identity::{CallIdentity, Selection, select};
use crate::{internals, points, sites};
use std::path::Path;

/// Discovery instrumentation must never be used to build or publish a game.
#[derive(Clone, Copy)]
pub enum Mode {
    /// Widen visibility and generate injection points, without diagnostic attributes.
    Build,
    /// Add internal-use diagnostics for an isolated metadata-only mod check.
    Discovery,
}

/// Counts and catalog produced by source staging.
pub struct Staged {
    /// Injection point counts.
    pub points: points::Staged,
    /// Exposed internal signatures and changed visibility counts.
    pub internals: internals::Widened,
}

/// Mark points, expose internals and write their manifests in a disposable source copy.
/// Internals precede points so their catalog contains only the original game items.
pub fn stage(
    root: &Path,
    points_json: &Path,
    internals_json: &Path,
    mode: Mode,
) -> Result<Staged, String> {
    let mut internals = match mode {
        Mode::Build => internals::widen(root)?,
        Mode::Discovery => internals::discover(root)?,
    };
    internals
        .entries
        .extend(crate::crossing::foreign_internals());
    write(internals_json, &internals.entries)?;
    let sites = sites::stage(root, &internals.entries).map_err(|e| e.to_string())?;
    let points = points::stage(root)?;
    #[derive(serde::Serialize)]
    struct Manifest {
        points: Vec<points::GamePoint>,
        sites: Vec<sites::Site>,
    }
    write(
        points_json,
        &Manifest {
            points: points::game_points(root)?,
            sites,
        },
    )?;
    Ok(Staged { points, internals })
}

fn write(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(path, text + "\n").map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_points_json_contains_return_slot_authority() {
        let root =
            std::env::temp_dir().join(format!("horizon-staging-return-{}", std::process::id()));
        let source = root.join("crates/horizon-client/src/game.rs");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(
            &source,
            "impl Game<'_> { fn unit(&self) {} fn optional(&self) -> Option<u32> { None } }",
        )
        .unwrap();
        std::fs::write(source.parent().unwrap().join("entry.rs"), "").unwrap();
        std::fs::write(
            root.join("crates/horizon-client/Cargo.toml"),
            "[package]\nname = 'horizon-client'\n",
        )
        .unwrap();
        let lists = root.join("crates/horizon-mod-api-gen");
        std::fs::create_dir_all(&lists).unwrap();
        std::fs::write(lists.join("internals.txt"), "").unwrap();
        std::fs::create_dir_all(lists.join("slots")).unwrap();
        let package = root.join("crates/horizon-mod-points");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = 'horizon-mod-points'\n",
        )
        .unwrap();
        let output = root.join("points.json");
        stage(&root, &output, &root.join("internals.json"), Mode::Build).unwrap();
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let points = manifest["points"].as_array().unwrap();
        for (name, expected) in [("unit", false), ("optional", true)] {
            let point = points.iter().find(|point| point["name"] == name).unwrap();
            assert_eq!(point["raw_shape"]["return_record_mutable"], expected);
        }
        assert!(manifest["sites"].is_array());
        std::fs::remove_dir_all(root).unwrap();
    }
}
