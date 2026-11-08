//! Tool-written native inject declarations, pinned to the game's metadata.
use horizon_mod_package::{
    Manifest,
    internals::ListedInternal,
    manifest::{
        MANIFEST,
        inject::{InjectError, Kind},
    },
};
use std::{fs, path::Path};
use toml_edit::{DocumentMut, Table, value};

const USAGE: &str = "usage: cargo horizon-mod inject add <at> --kind <kind> [--call <canonical callee>] [--nth N] [--with <fn>]";

#[derive(serde::Deserialize)]
struct Catalog {
    sites: Vec<Call>,
}
#[derive(serde::Deserialize)]
struct Call {
    target: String,
    call: String,
    ordinal: usize,
    ctx: String,
}

struct Request<'a> {
    at: &'a str,
    kind: Kind,
    kind_name: &'a str,
    call: Option<&'a str>,
    nth: Option<usize>,
    with: &'a str,
}

fn request(args: &[String]) -> Result<Request<'_>, String> {
    let [command, at, rest @ ..] = args else {
        return Err(USAGE.into());
    };
    if command != "add" || at.starts_with('-') {
        return Err(USAGE.into());
    }
    let (mut kind, mut call, mut nth, mut with) = (None, None, None, None);
    for pair in rest.chunks(2) {
        let [flag, val] = pair else {
            return Err(USAGE.into());
        };
        if val.starts_with('-') {
            return Err(USAGE.into());
        }
        match flag.as_str() {
            "--kind" if kind.is_none() => kind = Some(val.as_str()),
            "--call" if call.is_none() => call = Some(val.as_str()),
            "--nth" if nth.is_none() => nth = Some(val.parse::<usize>().map_err(|_| USAGE)?),
            "--with" if with.is_none() => with = Some(val.as_str()),
            _ => return Err(USAGE.into()),
        }
    }
    let kind_name = kind.ok_or(USAGE)?;
    let kind = serde_json::from_value(serde_json::Value::String(kind_name.into()))
        .map_err(|e| format!("invalid inject kind: {e}"))?;
    Ok(Request {
        at,
        kind,
        kind_name,
        call,
        nth,
        with: with.unwrap_or("on_inject"),
    })
}

pub fn run(args: &[String]) -> Result<(), String> {
    if args.first().is_some_and(|command| command == "sync") {
        return crate::inject_sync::run(&args[1..]);
    }
    add(Path::new("."), args)
}

fn add(dir: &Path, args: &[String]) -> Result<(), String> {
    let request = request(args)?;
    let path = dir.join(MANIFEST);
    let original = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Manifest::parse owns mod id, native pin and existing declaration validation.
    let manifest = Manifest::parse(&original)?;
    let native = manifest
        .native
        .as_ref()
        .ok_or("inject add needs a [native] mod")?;
    let internals = crate::internals::load(dir, &native.horizon)?;
    let points = dir
        .join("target/horizon-mod/prebuilt")
        .join(&native.horizon)
        .join("points.json");
    let catalog: Catalog = serde_json::from_str(
        &fs::read_to_string(&points).map_err(|e| format!("{}: {e}", points.display()))?,
    )
    .map_err(|e| format!("{}: {e}", points.display()))?;
    let candidate = append(&original, &manifest, &request, &internals, &catalog)?;
    crate::internals::atomic_write(&path, candidate)
}

fn append(
    text: &str,
    manifest: &Manifest,
    request: &Request<'_>,
    internals: &[ListedInternal],
    catalog: &Catalog,
) -> Result<String, String> {
    let missing = || {
        InjectError::Missing {
            id: manifest.id.clone(),
            at: request.at.into(),
        }
        .to_string()
    };
    let item = internals
        .iter()
        .find(|i| {
            i.path == request.at
                && !i.shape_only
                && if request.kind == Kind::Field {
                    i.kind == "struct"
                } else {
                    matches!(i.kind.as_str(), "fn" | "method")
                }
        })
        .ok_or_else(missing)?;
    let item = horizon_mod_package::internals::resolve(
        internals,
        &manifest.id,
        request.at,
        Some(&item.kind),
    )
    .map_err(|e| e.to_string())?;
    let needs_call = !matches!(
        request.kind,
        Kind::Head
            | Kind::Return
            | Kind::ModifyArg
            | Kind::Replace
            | Kind::Field
            | Kind::Expression
    );
    let selected = if needs_call {
        let candidates: Vec<_> = catalog
            .sites
            .iter()
            .filter(|s| {
                s.target == request.at
                    && request.call.is_none_or(|c| s.call == c)
                    && s.ordinal == request.nth.unwrap_or(0)
            })
            .collect();
        match candidates.as_slice() {
            [site] => {
                use horizon_mod_api_gen::sites::identity::{CallIdentity, Selection, select};
                let identities: Vec<_> = catalog
                    .sites
                    .iter()
                    .filter(|s| s.target == request.at)
                    .map(|s| CallIdentity {
                        call: &s.call,
                        nth: s.ordinal,
                        ctx: &s.ctx,
                    })
                    .collect();
                if !matches!(
                    select(&identities, &site.call, site.ordinal, &site.ctx),
                    Selection::Selected(_)
                ) {
                    return Err(InjectError::Lost {
                        id: manifest.id.clone(),
                        at: request.at.into(),
                    }
                    .to_string());
                }
                Some(*site)
            }
            [] => return Err(missing()),
            _ => {
                return Err(format!(
                    "{}: ambiguous call at {}; pass --call <canonical callee>",
                    manifest.id, request.at
                ));
            }
        }
    } else {
        if request.call.is_some() || request.nth.is_some() {
            return Err("--call and --nth apply only to call/local kinds".into());
        }
        None
    };
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{MANIFEST}: {e}"))?;
    if doc.get("inject").is_none() {
        doc["inject"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    let array = doc["inject"]
        .as_array_of_tables_mut()
        .ok_or("inject add requires [[inject]] tables")?;
    let mut table = Table::new();
    table["id"] = value(request.with);
    table["at"] = value(&item.path);
    table["kind"] = value(request.kind_name);
    table["with"] = value(request.with);
    table["hash"] = value(&item.hash);
    if let Some(site) = selected {
        table["call"] = value(&site.call);
        table["nth"] = value(
            i64::try_from(site.ordinal).map_err(|_| "call ordinal exceeds TOML integer range")?,
        );
        table["ctx"] = value(&site.ctx);
    }
    array.push(table);
    let result = doc.to_string();
    Manifest::parse(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEXT: &str = "# keep this comment\nid = 'example'\nname = 'Example'\nversion = '1.0.0'\napi = '0.2'\ncapabilities = ['registries']\n\n[native]\ngame = '26.3'\nmixins = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb'\nhorizon = '0123456789012345678901234567890123456789'\n";
    fn read(path: impl AsRef<Path>) -> String {
        fs::read_to_string(path).unwrap()
    }
    fn parse_manifest(text: &str) -> Manifest {
        Manifest::parse(text).unwrap()
    }
    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(String::from).collect()
    }
    fn invoke(dir: &Path, command: &str) -> Result<(), String> {
        add(dir, &args(command))
    }
    fn fixture() -> (Vec<ListedInternal>, Catalog) {
        (serde_json::from_str(r#"[{"path":"game::tick","kind":"method","signature":"fn tick(&mut self)","hash":"signature-fixture"}]"#).unwrap(), serde_json::from_str(r#"{"sites":[{"target":"game::tick","call":"game::first","ordinal":0,"ctx":"context-zero"},{"target":"game::tick","call":"game::first","ordinal":2,"ctx":"context-two"},{"target":"game::tick","call":"game::other","ordinal":0,"ctx":"other-context"}]}"#).unwrap())
    }
    fn edit(text: &str, args: &str) -> Result<String, String> {
        let args = self::args(args);
        let manifest = Manifest::parse(text)?;
        let (internals, catalog) = fixture();
        append(text, &manifest, &request(&args)?, &internals, &catalog)
    }
    #[test]
    fn inject_add_writes_only_validated_manifests() {
        let dir = reviewer_directory("validation");
        let path = dir.join(MANIFEST);
        add(&dir, &args("add game::tick --kind head --with before_tick")).unwrap();
        let result = read(&path);
        assert!(result.starts_with(TEXT));
        assert_eq!(parse_manifest(&result).inject[0].hash, "signature-fixture");
        for id in ["../escape", "bad/name", "bad\\name"] {
            let invalid = TEXT.replace("id = 'example'", &format!("id = '{id}'"));
            fs::write(&path, &invalid).unwrap();
            assert!(invoke(&dir, "add game::tick --kind head").is_err());
            assert_eq!(read(&path), invalid);
        }
        fs::write(&path, &result).unwrap();
        for command in [
            "add missing::place --kind head",
            "add game::tick --kind head --with fn",
            "add game::tick --kind head --with before_tick",
        ] {
            assert!(add(&dir, &args(command)).is_err());
            assert_eq!(read(&path), result);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn inject_add_refuses_gated_or_shape_only_targets() {
        let args = ["add", "game::tick", "--kind", "head"].map(String::from);
        let request = request(&args).unwrap();
        let manifest = parse_manifest(TEXT);
        let (mut internals, catalog) = fixture();
        internals[0].cfg = "unix".into();
        assert!(
            append(TEXT, &manifest, &request, &internals, &catalog)
                .unwrap_err()
                .contains("E_INTERNAL_GATED")
        );
        let mut unconditional = internals[0].clone();
        unconditional.cfg.clear();
        unconditional.hash = "unconditional".into();
        internals.push(unconditional);
        let result = append(TEXT, &manifest, &request, &internals, &catalog).unwrap();
        assert_eq!(parse_manifest(&result).inject[0].hash, "unconditional");
        for item in &mut internals {
            item.shape_only = true;
        }
        assert!(
            append(TEXT, &manifest, &request, &internals, &catalog)
                .unwrap_err()
                .contains("E_INJECT_MISSING")
        );
    }

    #[test]
    fn inject_add_pins_exact_call() {
        let result = edit(
            TEXT,
            "add game::tick --kind wrap_call --call game::first --nth 2 --with wrap",
        )
        .unwrap();
        let inject = &parse_manifest(&result).inject[0];
        assert_eq!(inject.hash, "signature-fixture");
        assert_eq!(inject.call.as_deref(), Some("game::first"));
        assert_eq!(inject.nth, Some(2));
        assert_eq!(inject.ctx.as_deref(), Some("context-two"));
        assert!(
            edit(TEXT, "add game::tick --kind wrap_call")
                .unwrap_err()
                .contains("ambiguous")
        );
    }
    #[test]
    fn inject_add_refuses_invalid_requests() {
        for args in [
            "add missing::place --kind head",
            "add game::tick --kind head --nth 0",
            "add game::tick --kind nope",
            "add game::tick --kind head --with fn",
            "add game::tick --kind wrap_call --call missing",
            "add game::tick --kind head --nth -1",
            "add game::tick --kind head --kind return",
            "add game::tick --kind",
            "sync",
        ] {
            assert!(edit(TEXT, args).is_err(), "{args}");
        }
        let error = edit(TEXT, "add missing::place --kind head").unwrap_err();
        assert!(error.contains("E_INJECT_MISSING") && error.contains("missing::place"));
        let first = edit(TEXT, "add game::tick --kind head").unwrap();
        assert!(edit(&first, "add game::tick --kind head").is_err());
    }

    #[test]
    fn reviewer_defaults_and_literal_selection() {
        let result = edit(TEXT, "add game::tick --kind head").unwrap();
        let parsed = parse_manifest(&result);
        assert_eq!(parsed.inject[0].id, "on_inject");
        assert_eq!(parsed.inject[0].with, "on_inject");
        let result = edit(TEXT, "add game::tick --kind wrap_call --call game::other").unwrap();
        let parsed = parse_manifest(&result);
        assert_eq!(parsed.inject[0].call.as_deref(), Some("game::other"));
        assert_eq!(parsed.inject[0].ctx.as_deref(), Some("other-context"));
        assert_eq!(parsed.inject[0].nth, Some(0));
    }

    #[test]
    fn reviewer_shared_selector_duplicate_context() {
        let (internals, mut catalog) = fixture();
        catalog.sites[1].ctx = "context-zero".into();
        let args = args("add game::tick --kind wrap_call --call game::first");
        let manifest = parse_manifest(TEXT);
        let request = request(&args).unwrap();
        assert!(append(TEXT, &manifest, &request, &internals, &catalog).is_err());
    }

    fn reviewer_directory(label: &str) -> std::path::PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/reviewer-{label}-{}",
            std::process::id()
        ));
        let cache =
            dir.join("target/horizon-mod/prebuilt/0123456789012345678901234567890123456789");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("internals.json"), r#"[{"path":"game::tick","kind":"method","signature":"fn tick(&mut self)","hash":"signature-fixture"}]"#).unwrap();
        fs::write(cache.join("points.json"), r#"{"sites":[]}"#).unwrap();
        fs::write(dir.join(MANIFEST), TEXT).unwrap();
        dir
    }

    #[test]
    fn reviewer_atomic_replacement() {
        let dir = reviewer_directory("atomic");
        let path = dir.join(MANIFEST);
        let previous = dir.join("previous");
        fs::hard_link(&path, &previous).unwrap();
        invoke(&dir, "add game::tick --kind head").unwrap();
        let old = read(previous);
        let parsed = Manifest::parse(&read(&path)).unwrap();
        let leftovers = fs::symlink_metadata(path.with_extension("toml.sync")).is_ok();
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            old, TEXT,
            "atomic replacement must not modify the previous inode"
        );
        assert_eq!(parsed.inject[0].id, "on_inject");
        assert!(!leftovers);
    }

    #[cfg(unix)]
    #[test]
    fn reviewer_permissions_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let dir = reviewer_directory("mode");
        let path = dir.join(MANIFEST);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        invoke(&dir, "add game::tick --kind head").unwrap();
        let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn reviewer_format_and_odd_input() {
        let original = TEXT.replace(
            "capabilities = ['registries']",
            "capabilities = ['registries'] # cap\ndependencies = { helper = { version = '^1.0', optional = true } }",
        );
        let result = edit(&original, "add game::tick --kind head --with first").unwrap();
        let inject = &parse_manifest(&result).inject[0];
        assert_eq!(inject.hash, "signature-fixture");
        assert_eq!(inject.with, "first");
        assert_eq!(inject.id, "first");
        assert_eq!(inject.nth, None);
        assert!(result.starts_with(&original));
        let again = edit(&result, "add game::tick --kind return --with second").unwrap();
        assert!(again.starts_with(&result));
        assert_eq!(parse_manifest(&again).inject.len(), 2);
        let (internals, catalog) = fixture();
        let manifest = parse_manifest(TEXT);
        for value in ["", "☃", "e\u{301}", "a\0b", &"x".repeat(100_000)] {
            let args = ["add", value, "--kind", "head"].map(String::from);
            let request = request(&args).unwrap();
            assert!(append(TEXT, &manifest, &request, &internals, &catalog).is_err());
        }
    }

    #[test]
    fn reviewer_io_failures_leave_original() {
        let dir = reviewer_directory("failure");
        let path = dir.join(MANIFEST);
        let command = "add game::tick --kind head";
        let temp = path.with_extension("toml.sync");
        fs::create_dir(&temp).unwrap();
        assert!(invoke(&dir, command).is_err());
        assert_eq!(read(&path), TEXT);
        fs::remove_dir(&temp).unwrap();
        let points = dir.join(
            "target/horizon-mod/prebuilt/0123456789012345678901234567890123456789/points.json",
        );
        for contents in ["{", "[]", r#"{"sites":[{"target":"x"}]}"#] {
            fs::write(&points, contents).unwrap();
            assert!(invoke(&dir, command).is_err());
            assert_eq!(read(&path), TEXT);
        }
        fs::remove_file(points).unwrap();
        assert!(invoke(&dir, command).is_err());
        fs::remove_file(&path).unwrap();
        assert!(invoke(&dir, command).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reviewer_symlink_manifest_does_not_overwrite_referent() {
        let dir = reviewer_directory("symlink");
        let path = dir.join(MANIFEST);
        let referent = dir.join("referent.toml");
        fs::rename(&path, &referent).unwrap();
        std::os::unix::fs::symlink(&referent, &path).unwrap();
        invoke(&dir, "add game::tick --kind head").unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        assert_eq!(read(&referent), TEXT);
        assert_eq!(parse_manifest(&read(path)).inject.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
