//! Publish a native mod's requested slots through the game's shared slot parser.
use horizon_mod_api_gen::sites::{Slots, slots};
use horizon_mod_package::{
    Manifest,
    manifest::{MANIFEST, inject::Kind},
};
use std::{collections::BTreeSet, fs, path::Path};

#[cfg(test)]
#[path = "inject_sync_tests.rs"]
mod tests;

const DIRECTORY: &str = "crates/horizon-mod-api-gen/slots";

pub fn run(args: &[String]) -> Result<(), String> {
    let [flag, repo] = args else {
        return Err("usage: cargo horizon-mod inject sync --repo <game checkout>".into());
    };
    if flag != "--repo" || repo.starts_with('-') {
        return Err("usage: cargo horizon-mod inject sync --repo <game checkout>".into());
    }
    sync(Path::new("."), Path::new(repo))
}

fn sync(dir: &Path, repo: &Path) -> Result<(), String> {
    let path = dir.join(MANIFEST);
    let manifest = Manifest::parse(
        &fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )?;
    // Manifest::parse validates the owner id (charset and no path separators) at this write boundary.
    if manifest.id == "horizon" {
        return Err("horizon.txt is reserved for the engine seed".into());
    }
    manifest
        .native
        .as_ref()
        .ok_or("inject sync needs a [native] mod")?;
    let directory = repo.join(DIRECTORY);
    if !repo.join("Cargo.toml").is_file() || !directory.is_dir() {
        return Err(format!(
            "{}: not a Horizon workspace (missing Cargo.toml or {DIRECTORY})",
            repo.display()
        ));
    }
    let output = directory.join(format!("{}.txt", manifest.id));
    let existing = slots(&directory).map_err(|e| e.to_string())?;
    let mut requested = Slots::default();
    let lines = manifest
        .inject
        .iter()
        .map(|entry| {
            if [
                &entry.at,
                entry.call.as_ref().unwrap_or(&entry.at),
                entry.ctx.as_ref().unwrap_or(&entry.at),
            ]
            .iter()
            .any(|field| field.contains(['\n', '\r', '#', '|']))
            {
                return Err(format!(
                    "E_INJECT_INVALID: {}:{}: slot fields cannot contain line breaks, # or |",
                    manifest.id, entry.id
                ));
            }
            Ok(if entry.kind == Kind::Field {
                format!("type {}", entry.at)
            } else if let (Some(call), Some(nth), Some(ctx)) = (&entry.call, entry.nth, &entry.ctx)
            {
                format!("{} | {call} | {nth} | {ctx}", entry.at)
            } else {
                entry.at.clone()
            })
        })
        .collect::<Result<BTreeSet<_>, String>>()?
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n");
    requested
        .extend(&output, &lines)
        .map_err(|e| e.to_string())?;
    let old = match fs::read_to_string(&output) {
        Ok(old) => old,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", output.display())),
    };
    let header = format!(
        "# {}: written by cargo horizon-mod inject sync\n",
        manifest.id
    );
    for (index, line) in old.lines().enumerate() {
        if line.contains('#') && line.trim() != header.trim() {
            return Err(format!(
                "{}:{}: move the comment out of this generated slot list before syncing",
                output.display(),
                index + 1
            ));
        }
    }
    let mut owned = Slots::default();
    owned.extend(&output, &old).map_err(|e| e.to_string())?;
    for (at, origin) in requested.functions {
        if !existing.functions.contains_key(&at) {
            owned.functions.insert(at, origin);
        }
    }
    for (at, origin) in requested.types {
        if !existing.types.contains_key(&at) {
            owned.types.insert(at, origin);
        }
    }
    for call in requested.calls {
        if !existing.calls.iter().any(|c| {
            c.target == call.target && c.selector == call.selector && c.ordinal == call.ordinal
        }) {
            owned.calls.push(call);
        }
    }
    // Validate proposed calls against the whole union before preparing any write.
    let mut checked = existing;
    checked.extend(&output, &lines).map_err(|e| e.to_string())?;
    let mut sorted = BTreeSet::new();
    sorted.extend(owned.functions.into_keys());
    sorted.extend(owned.types.into_keys().map(|at| format!("type {at}")));
    for call in owned.calls {
        let nth = call.ordinal.map(|n| n.to_string()).unwrap_or_default();
        sorted.insert(format!(
            "{} | {} | {nth} | {}",
            call.target, call.selector, call.ctx
        ));
    }
    let mut candidate = header;
    for line in sorted {
        candidate.push_str(&line);
        candidate.push('\n');
    }
    if old != candidate {
        crate::internals::atomic_write(&output, candidate)?;
    }
    Ok(())
}
