//! Checked native source inputs and private point staging.
use horizon_mod_package::manifest::inject::{
    Access, Entry, Inject, InjectError, Kind, NoSlotReason, Route, Target,
};
#[cfg(test)]
use horizon_mod_package::{
    manifest::inject::{route, Place, SlotKind},
    Manifest,
};
use point_shape::{raw_able, shape};
use proc_macro2::TokenStream;
use quote::quote;
use std::fmt::Write;

struct Adapter {
    inject: Inject,
    reason: NoSlotReason,
    identity: String,
    kind: &'static str,
    code: TokenStream,
}

fn lower(
    entry: Entry<'_>,
    decision: Route,
    signature: &syn::Signature,
    receiver: Option<(&syn::Type, &syn::Generics)>,
) -> Result<Option<Adapter>, InjectError> {
    let inject = entry.inject;
    let owner = entry.mod_id;
    let reason = match decision {
        Route::Slot(_) => return Ok(None),
        Route::Source(reason) => reason,
    };
    let invalid = |field, fix| InjectError::Invalid {
        id: format!("{owner}:{}", inject.id),
        field,
        fix,
    };
    if inject.access != Access::Internals {
        return Err(invalid("access", "API source adapters await U5f"));
    }
    let kind = match inject.kind {
        Kind::Head => "HEAD",
        Kind::Return => "RETURN",
        _ => {
            return Err(invalid(
                "kind",
                "this source adapter requires head or return",
            ))
        }
    };
    if !raw_able(signature, receiver.map(|(_, generics)| generics)) {
        return Err(invalid(
            "at",
            "source listeners require a non-generic signature",
        ));
    }
    if !point_shape::eligible(&[], signature) {
        return Err(invalid(
            "at",
            "source listeners require a plain, safe, synchronous Rust signature",
        ));
    }
    // Length-prefixed UTF-8, not another signature hasher. Encoding every byte also keeps
    // the symbol distinct after the raw-record generator's CamelCase conversion.
    let mut identity = String::from("source");
    for part in [owner, inject.id.as_str(), inject.at.as_str(), kind] {
        for byte in (part.len() as u64)
            .to_le_bytes()
            .into_iter()
            .chain(part.bytes())
        {
            write!(identity, "{byte:02x}").unwrap();
        }
    }
    let point = shape(quote!(raw, name = #identity), signature)
        .map_err(|_| invalid("at", "the source signature cannot form a raw record"))?;
    let items = point.raw_items(
        signature,
        receiver.map(|(ty, _)| ty),
        receiver.map(|(_, generics)| generics),
    );
    Ok(Some(Adapter {
        inject: inject.clone(),
        reason,
        identity,
        kind,
        code: items,
    }))
}

pub(super) fn stage(
    root: &std::path::Path,
    catalog_dir: &std::path::Path,
    mods: &[super::Bundle],
) -> Result<(), String> {
    use horizon_mod_api_gen::{
        points::source::{self, Point},
        sites::{source as call_source, Declaration},
    };
    use syn::parse::Parser;
    let catalog = super::internals::Catalog::read(catalog_dir)?;
    let mut points = std::collections::BTreeMap::new();
    let mut calls = std::collections::BTreeMap::new();
    let mut bundles = mods.iter().collect::<Vec<_>>();
    bundles.sort_by_key(|bundle| &bundle.manifest.id);
    for bundle in bundles {
        let manifest = &bundle.manifest;
        for (inject, decision) in manifest
            .inject
            .iter()
            .zip(super::slots::routes(&catalog, manifest).map_err(|error| error.to_string())?)
        {
            if !matches!(decision, Route::Source(_)) {
                continue;
            }
            if let Some(call) = &inject.call {
                let target = super::internals::plain_path(&inject.at);
                let selector = call.clone();
                let ordinal = inject.nth.expect("validated call ordinal");
                let ctx = inject.ctx.clone().expect("validated statement identity");
                calls
                    .entry((target.clone(), selector.clone(), ordinal, ctx.clone()))
                    .or_insert(Declaration {
                        target,
                        selector,
                        ordinal: Some(ordinal),
                        ctx,
                        origin: format!("{}:{}", manifest.id, inject.id),
                    });
                continue;
            }
            let item = catalog
                .internals
                .iter()
                .find(|item| {
                    super::internals::plain_path(&item.path)
                        == super::internals::plain_path(&inject.at)
                })
                .expect("checked source target");
            let parse = |input: syn::parse::ParseStream<'_>| -> syn::Result<_> {
                if input.peek(syn::Token![impl]) {
                    input.parse::<syn::Token![impl]>()?;
                    let mut generics: syn::Generics = input.parse()?;
                    let ty: syn::Type = input.parse()?;
                    generics.where_clause = input.parse()?;
                    let inner;
                    syn::braced!(inner in input);
                    Ok((inner.parse::<syn::Signature>()?, Some((ty, generics))))
                } else {
                    Ok((input.parse::<syn::Signature>()?, None))
                }
            };
            let (signature, owner) = parse.parse_str(&item.signature).map_err(|_| {
                InjectError::Invalid {
                    id: inject.id.clone(),
                    field: "at",
                    fix: "source listeners require a function signature",
                }
                .to_string()
            })?;
            let adapter = lower(
                Entry {
                    inject,
                    mod_id: &manifest.id,
                    target: Target::Native,
                    resolved_call: None,
                },
                decision,
                &signature,
                owner.as_ref().map(|(ty, generics)| (ty, generics)),
            )
            .map_err(|error| error.to_string())?
            .expect("source route");
            eprintln!(
                "{}:{}: {} -> source (private slot, {}, {})",
                manifest.id, adapter.inject.id, adapter.inject.at, adapter.kind, adapter.reason
            );
            points
                .entry(super::internals::plain_path(&adapter.inject.at))
                .or_insert(Point {
                    name: adapter.identity,
                    target: item.path.clone(),
                    items: adapter.code.to_string(),
                    module: String::new(),
                    record_export: String::new(),
                });
        }
    }
    let calls = calls.into_values().collect::<Vec<_>>();
    let entries = if calls.is_empty() {
        Vec::new()
    } else {
        horizon_mod_api_gen::internals_view::catalog(
            &std::fs::read_to_string(catalog_dir.join(super::internals::INTERNALS))
                .map_err(|error| error.to_string())?,
        )?
    };
    call_source::stage(root, &entries, &calls)?;
    std::fs::copy(
        root.join(call_source::FILE),
        catalog_dir.join(call_source::FILE),
    )
    .map_err(|error| error.to_string())?;
    let mut points = points.into_values().collect::<Vec<_>>();
    source::stage(root, &mut points)?;
    std::fs::copy(root.join(source::FILE), catalog_dir.join(source::FILE))
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn checked_routes_and_identities_do_not_alias() {
    let make = |owner: &str, id: &str, at: &str, kind: &str, slots| {
        let manifest = declaration(owner, id, at, kind);
        adapt(&manifest, slots, "fn target(value: i32) -> i32").unwrap()
    };
    let original = make("one", "ab", "game::c", "head", vec![]).unwrap();
    assert_eq!(original.inject.with, "callback");
    assert_eq!(original.reason, NoSlotReason::NotListed);
    for (owner, id, at, kind) in [
        ("two", "ab", "game::c", "head"),
        ("one", "a", "bgame::c", "head"),
        ("one", "ab", "game::c", "return"),
    ] {
        let other = make(owner, id, at, kind, vec![]).unwrap();
        assert_ne!(original.identity, other.identity);
    }
    assert!(make("one", "ab", "game::c", "head", vec![SlotKind::Head]).is_none());
    let mut manifest = declaration("one", "ab", "game::c", "head");
    manifest.inject[0].hash = "changed".into();
    assert!(matches!(
        adapt(&manifest, vec![], "fn target()"),
        Err(InjectError::Changed { .. })
    ));
    manifest.inject[0].hash = "fingerprint".into();
    for (access, kind, field) in [
        (Access::Api, Kind::Head, "access"),
        (Access::Internals, Kind::ModifyArg, "kind"),
    ] {
        manifest.inject[0].access = access;
        manifest.inject[0].kind = kind;
        let error = adapt(&manifest, vec![], "fn target()").err();
        assert!(
            matches!(error, Some(InjectError::Invalid { field: actual, .. }) if actual == field)
        );
    }
}

#[cfg(test)]
fn declaration(owner: &str, id: &str, at: &str, kind: &str) -> Manifest {
    Manifest::parse(&format!(
        "id='{owner}'\nname='Source fixture'\nversion='0.1.0'\napi='0.2'\ncapabilities=['registries']\n[native]\ngame='26.3'\nhorizon='{}'\nmixins='{}'\n[[inject]]\nid='{id}'\nat='{at}'\nkind='{kind}'\nwith='callback'\naccess='internals'\nhash='fingerprint'\n", "0".repeat(40), "0".repeat(40)
    )).unwrap()
}

#[cfg(test)]
fn adapt(m: &Manifest, slots: Vec<SlotKind>, sig: &str) -> Result<Option<Adapter>, InjectError> {
    let entry = || Entry {
        inject: &m.inject[0],
        target: Target::Native,
        mod_id: &m.id,
        resolved_call: None,
    };
    let decision = route(
        entry(),
        &[Place {
            at: m.inject[0].at.clone(),
            hash: "fingerprint".into(),
            slots,
            calls: vec![],
            native_only: false,
        }],
    )?;
    lower(
        entry(),
        decision,
        &syn::parse_str(sig).unwrap(),
        Some((&syn::parse_quote!(Game), &syn::Generics::default())),
    )
}

#[test]
fn source_adapter_inputs_compile_and_write_back() {
    for (kind, signature, fields, body, setup, expected) in [
        ("head", "fn target(&mut self, value: i32) -> i32", "i32, i32", "*args.value += 2; *args.result = Some(9);", "let mut result = None; for _ in 0..2 { let mut args = RECORD { value: &mut value, result: &mut result }; call(&mut game, &mut args); } assert_eq!(result, Some(9));", "assert_eq!(value, 7);"),
        ("head", "fn target(&mut self, value: i32)", "i32", "*args.value += 2; *args.cancel = true;", "let mut cancel = false; for _ in 0..2 { let mut args = RECORD { value: &mut value, cancel: &mut cancel }; call(&mut game, &mut args); } assert!(cancel);", "assert_eq!(value, 7);"),
        ("return", "fn target(&mut self, value: i32) -> i32", "", "*args += 5;", "for _ in 0..2 { call(&mut game, &mut value); }", "assert_eq!(value, 13);"),
    ] {
        let adapter = adapt(&declaration("one", "test", "game::target", kind), vec![], signature).unwrap().unwrap();
        assert_eq!(adapter.kind, kind.to_uppercase());
        let record = format!("__{}Raw", point_shape::camel(&adapter.identity));
        let alias = format!("__{}{}Listener", point_shape::camel(&adapter.identity), if kind == "return" { "Return" } else { "" });
        let arg = if kind == "return" { "i32".into() } else { format!("{record}<'_, {fields}>") };
        let code = format!(r#"
            pub struct Game {{ bias: i32 }}
            {generated}
            fn callback(game: &mut Game, args: &mut {arg}) {{ game.bias += 1; {body} }}
            fn main() {{ let call: {alias} = callback;
                let mut game = Game {{ bias: 0 }}; let mut value = 3; {setup} {expected} assert_eq!(game.bias, 2);
            }}
        "#, generated=adapter.code, setup=setup.replace("RECORD", &record));
        let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/source-adapter-fixture");
        std::fs::create_dir_all(&target).unwrap();
        let input = target.join("main.rs");
        let output = target.join("fixture");
        std::fs::write(&input, &code).unwrap();
        let status = std::process::Command::new("rustc").args(["--edition", "2024"]).arg(&input).arg("-o").arg(&output).status().unwrap();
        assert!(status.success(), "{code}");
        assert!(std::process::Command::new(&output).status().unwrap().success());
    }
}

#[test]
fn private_source_stage_exports_and_registers_priorities() {
    use super::{api, internals::Catalog, Bundle};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../target/source-wrapper-{}", std::process::id()));
    let put = |path: &str, text: &str| {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    put(
        "crates/horizon-mod-points/Cargo.toml",
        "[package]\nname='horizon-mod-points'\n",
    );
    put(
        "crates/horizon-client/src/game.rs",
        "pub struct Game; impl Game { #[inline(always)] fn hidden(&mut self) {} }",
    );
    std::fs::create_dir_all(root.join("crates/horizon-mod-api-gen/slots")).unwrap();
    let target = "horizon_client::game::Game::hidden";
    put("catalog/points.json", r#"{"points":[],"sites":[]}"#);
    put(
        "catalog/internals.json",
        &serde_json::json!([{
            "path":target, "kind":"fn", "signature":"impl Game { fn hidden(&mut self) }",
            "hash":"fingerprint", "cfg":"", "shape_only":false
        }])
        .to_string(),
    );
    let mut manifest = declaration("one", "high", target, "head");
    manifest.inject[0].priority = 17;
    let mut low = manifest.inject[0].clone();
    low.id = "low".into();
    low.priority = -3;
    manifest.inject.push(low);
    let bundle = Bundle {
        manifest,
        dir: root.clone(),
    };
    let catalog_dir = root.join("catalog");
    stage(&root, &catalog_dir, std::slice::from_ref(&bundle)).unwrap();
    let catalog = Catalog::read(&catalog_dir).unwrap();
    assert_eq!(catalog.points.len(), 1);
    assert!(catalog.points[0].private);
    let facade = catalog.facade(&bundle.manifest).unwrap();
    assert!(facade.source.contains("Raw as __HiddenRaw;"));
    let main = api::main_rs(
        "/* MOD_SLOTS */",
        &[&bundle.manifest],
        &catalog.points,
        &catalog.sites,
    );
    assert_eq!(main.matches("mod_0::callback").count(), 2);
    for registration in [".ordered(17, \"high\")", ".ordered(-3, \"low\")"] {
        assert!(main.contains(registration), "{main}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn private_calls_use_ctx_shared_ids_and_real_registration_priorities() {
    use super::{api, internals::Catalog, Bundle};
    use horizon_mod_api_gen::sites::{source as calls, Declaration};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../target/source-call-wrapper-{}",
        std::process::id()
    ));
    let file = root.join("crates/horizon-client/src/game.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::create_dir_all(root.join("crates/horizon-mod-api-gen/slots")).unwrap();
    std::fs::create_dir_all(root.join("crates/horizon-mod-points")).unwrap();
    std::fs::write(
        root.join("crates/horizon-mod-points/Cargo.toml"),
        "[package]\nname='horizon-mod-points'\n",
    )
    .unwrap();
    let text = "pub struct Game; impl Game { #[inline(always)] fn run(&mut self) -> u32 { self.step(2) } #[inline(always)] fn step(&mut self, value: u32) -> u32 { value + 1 } }";
    std::fs::write(&file, text).unwrap();
    let target = "horizon_client::game::Game::run";
    let callee = "horizon_client::game::Game::step";
    let metadata = serde_json::json!([
        {"path":"horizon_client::game::Game", "kind":"struct", "signature":"pub struct Game;", "hash":"game"},
        {"path":target, "kind":"method", "signature":"impl Game { fn run(&mut self) -> u32 }", "hash":"fingerprint"},
        {"path":callee, "kind":"method", "signature":"impl Game { fn step(&mut self, value: u32) -> u32 }", "hash":"step"}
    ]).to_string();
    let entries = horizon_mod_api_gen::internals_view::catalog(&metadata).unwrap();
    let discovered = calls::stage(
        &root,
        &entries,
        &[Declaration {
            target: target.into(),
            selector: callee.into(),
            ordinal: Some(0),
            ctx: String::new(),
            origin: "discovery".into(),
        }],
    )
    .unwrap()
    .remove(0);
    std::fs::write(&file, text).unwrap();
    let dir = root.join("catalog");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("points.json"), r#"{"points":[],"sites":[]}"#).unwrap();
    std::fs::write(dir.join("internals.json"), metadata).unwrap();
    let mut manifest = declaration("one", "high", target, "head");
    let inject = &mut manifest.inject[0];
    inject.kind = Kind::ModifyValue;
    inject.call = Some(callee.into());
    inject.nth = Some(0);
    inject.ctx = Some(discovered.ctx.clone());
    inject.priority = 17;
    let mut low = inject.clone();
    low.id = "low".into();
    low.priority = -3;
    manifest.inject.push(low);
    let bundle = Bundle {
        manifest,
        dir: root.clone(),
    };
    stage(&root, &dir, std::slice::from_ref(&bundle)).unwrap();
    let catalog = Catalog::read(&dir).unwrap();
    assert_eq!(catalog.sites.len(), 1);
    assert!(catalog.sites[0].private);
    assert_eq!(catalog.sites[0].ctx, discovered.ctx);
    assert_ne!(discovered.hash, discovered.ctx);
    assert!(super::slots::routes(&catalog, &bundle.manifest)
        .unwrap()
        .iter()
        .all(|route| matches!(route, Route::Source(_))));
    let main = api::main_rs(
        "/* MOD_SLOTS */",
        &[&bundle.manifest],
        &catalog.points,
        &catalog.sites,
    );
    for registration in ["(\"one\", 17, \"high\",", "(\"one\", -3, \"low\","] {
        assert!(main.contains(registration), "{main}");
    }
    assert_eq!(main.matches("mod_0::callback").count(), 2);
    let mut bad = bundle;
    for index in 0..2 {
        bad.manifest
            .inject
            .iter_mut()
            .for_each(|entry| entry.ctx = Some(discovered.ctx.clone()));
        bad.manifest.inject[index].ctx = Some(discovered.hash.clone());
        std::fs::write(&file, text).unwrap();
        assert!(stage(&root, &dir, std::slice::from_ref(&bad))
            .unwrap_err()
            .contains("E_INJECT_LOST"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    }
    let mut public = serde_json::to_value(&discovered).unwrap();
    public["call"] = "different::callee".into();
    std::fs::write(
        dir.join("points.json"),
        serde_json::json!({"points":[], "sites":[public]}).to_string(),
    )
    .unwrap();
    assert!(Catalog::read(&dir)
        .err()
        .unwrap()
        .contains("E_SOURCE_COLLISION"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsupported_source_signatures_are_refused() {
    for signature in [
        "fn target<T>(value: T)",
        "fn target(value: impl Copy)",
        "fn target<const N: usize>()",
        "async fn target()",
        "unsafe fn target()",
        "const fn target()",
        "extern \"C\" fn target()",
    ] {
        let manifest = declaration("one", "generic", "game::target", "head");
        assert!(matches!(
            adapt(&manifest, vec![], signature),
            Err(InjectError::Invalid { field: "at", .. })
        ));
    }
}
