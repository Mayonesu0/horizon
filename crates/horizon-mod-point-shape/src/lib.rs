//! The shape of an injection point (#589): what a `#[point]` function hands its listeners. Shared
//! by the `#[point]` attribute (the check at the function's head) and the mod API generator (the
//! records, `PointId` and `points.json`), so the two always agree.
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{FnArg, Pat, Type};

/// Where a point's function runs, as a hook's `side`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Server,
    Client,
    Both,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
            Self::Both => "both",
        }
    }
}

/// One argument a point passes: the parameter's binding and type.
pub struct Arg {
    pub name: syn::Ident,
    pub ty: Type,
}

/// What a point's listeners may do with the function's return value.
pub enum Returns {
    /// `()`: a head listener may `cancel`, and the function returns at once.
    Unit,
    /// `Result<()>`: the same, returning `Ok(())`.
    UnitResult,
    /// An API-typed value: a head listener may set `result`, which the function returns at once;
    /// a return listener sees the value in `result` and may change it.
    Value(Type),
    /// A value the API can't carry: the listeners neither stop the function nor see its value.
    Opaque,
}

/// How a point's function takes `self`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receiver {
    /// A free function, or an associated one without `self`.
    None,
    /// `&self`.
    Shared,
    /// `&mut self`.
    Mut,
}

/// A point's items in `mod_points` ([`Shape::api_items`]), each list one entry per place (its head,
/// then its return).
#[derive(Default)]
pub struct ApiItems {
    /// Its typed function aliases and constructors.
    pub listeners: TokenStream,
    /// Its `PointId` variants.
    pub variants: TokenStream,
    /// Its typed native and wasm dispatch functions.
    pub dispatch: TokenStream,
}

/// A `#[point]` function's point.
pub struct Shape {
    /// Unique among the game's points: the attribute's `name`, or the function's own.
    pub name: String,
    /// The side its `Game` method runs on, which picks its API listeners' host. A point off `Game`
    /// has none, and its API listeners get no host.
    pub side: Option<Side>,
    pub receiver: Receiver,
    /// The parameters with API types, in order. The others aren't passed: an internal type
    /// would become API.
    pub args: Vec<Arg>,
    pub returns: Returns,
    /// The value it returns borrows (`-> &T`, `-> Option<&mut T>`): the receiver is still
    /// borrowed when the body ends, so its return place is raw only, and gets the value itself.
    pub borrowed_return: bool,
    /// It has a raw form (`raw` in the attribute): the staging pass declared its raw record and
    /// listener types beside it ([`Shape::raw_items`]).
    pub raw: bool,
    /// Staging-only raw dispatch, with no API record or API listeners.
    pub raw_only: bool,
    /// A free function (`free` in the attribute), not an associated one.
    pub free: bool,
}

/// Whether the staging pass gives a function with these `attrs` and `sig` a point: one predicate for
/// the pass and for the generator that lists the points, so the two always agree. Left out:
/// the hooks' own functions (`native_*`, whose bodies the generator reads), anything compiled only
/// under a `cfg`, `const`, `async` and `unsafe` functions, methods taking `self` by value,
/// `impl Trait` in the signature, `-> !`, `#[inline(always)]`, and parameters named `cancel` or
/// `result`, which the record has its own of. A `Game` method also needs a `&self` or `&mut self`
/// receiver: its listeners get the game as their host.
pub fn eligible(attrs: &[syn::Attribute], sig: &syn::Signature) -> bool {
    let impl_trait = |ty: &Type| matches!(ty, Type::ImplTrait(_));
    !sig.ident.to_string().starts_with("native_")
        && sig.constness.is_none()
        && sig.asyncness.is_none()
        && sig.unsafety.is_none()
        && sig.abi.is_none()
        && !matches!(sig.inputs.first(), Some(FnArg::Receiver(receiver)) if receiver.reference.is_none())
        && !matches!(&sig.output, syn::ReturnType::Type(_, ty) if impl_trait(ty) || matches!(**ty, Type::Never(_)))
        && sig.inputs.iter().all(|input| match input {
            FnArg::Typed(typed) => {
                !impl_trait(&typed.ty)
                    && !matches!(&*typed.pat, Pat::Ident(pat) if pat.ident == "cancel" || pat.ident == "result")
            }
            FnArg::Receiver(_) => true,
        })
        && !attrs.iter().any(|attr| {
            let path = attr.path();
            path.is_ident("cfg")
                || path.is_ident("cfg_attr")
                || path.is_ident("test")
                || (path.is_ident("inline")
                    && attr
                        .parse_args::<syn::Ident>()
                        .is_ok_and(|arg| arg == "always"))
        })
}

/// Whether `ty` holds a reference or a lifetime other than `'static`: a value that may borrow.
fn borrows(ty: &Type) -> bool {
    struct Find(bool);
    impl<'ast> syn::visit::Visit<'ast> for Find {
        fn visit_type_reference(&mut self, reference: &'ast syn::TypeReference) {
            match &reference.lifetime {
                Some(lifetime) if lifetime.ident == "static" => {}
                _ => self.0 = true,
            }
            syn::visit::visit_type_reference(self, reference);
        }
        fn visit_lifetime(&mut self, lifetime: &'ast syn::Lifetime) {
            if lifetime.ident != "static" {
                self.0 = true;
            }
        }
    }
    let mut find = Find(false);
    syn::visit::Visit::visit_type(&mut find, ty);
    find.0
}

/// Whether a point with this `sig`, in an `impl` with `impl_generics` (none for a free function),
/// has a raw form: its listener's type is a plain function pointer, so neither the function nor its
/// `impl` may have type or const parameters, nor its signature an `impl Trait` anywhere.
pub fn raw_able(sig: &syn::Signature, impl_generics: Option<&syn::Generics>) -> bool {
    struct Find(bool);
    impl<'ast> syn::visit::Visit<'ast> for Find {
        fn visit_type_impl_trait(&mut self, _: &'ast syn::TypeImplTrait) {
            self.0 = true;
        }
    }
    let mut find = Find(false);
    syn::visit::Visit::visit_signature(&mut find, sig);
    if find.0 {
        return false;
    }
    let plain = |generics: &syn::Generics| {
        generics
            .params
            .iter()
            .all(|param| matches!(param, syn::GenericParam::Lifetime(_)))
    };
    plain(&sig.generics) && impl_generics.is_none_or(plain)
}

/// The point of the function `sig`, from `#[point(side = "…", name = "…", raw, free)]`'s arguments.
pub fn shape(attribute: TokenStream, sig: &syn::Signature) -> syn::Result<Shape> {
    let (mut name, mut side, mut raw, mut free) = (sig.ident.to_string(), None, false, false);
    let mut raw_only = false;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("raw") {
            raw = true;
        } else if meta.path.is_ident("raw_only") {
            raw = true;
            raw_only = true;
        } else if meta.path.is_ident("free") {
            free = true;
        } else if meta.path.is_ident("name") {
            name = meta.value()?.parse::<syn::LitStr>()?.value();
        } else if meta.path.is_ident("side") {
            let value = meta.value()?.parse::<syn::LitStr>()?;
            side = Some(match value.value().as_str() {
                "server" => Side::Server,
                "client" => Side::Client,
                "both" => Side::Both,
                _ => {
                    return Err(syn::Error::new(
                        value.span(),
                        "side is server, client or both",
                    ));
                }
            });
        } else {
            return Err(meta.error("a point takes side = \"…\", name = \"…\", raw and free"));
        }
        Ok(())
    });
    syn::parse::Parser::parse2(parser, attribute)?;
    let receiver = match sig.inputs.first() {
        Some(FnArg::Receiver(receiver)) if receiver.reference.is_none() => {
            return Err(syn::Error::new_spanned(
                receiver,
                "a point's function takes `self` by reference, or not at all",
            ));
        }
        Some(FnArg::Receiver(receiver)) if receiver.mutability.is_some() => Receiver::Mut,
        Some(FnArg::Receiver(_)) => Receiver::Shared,
        _ => Receiver::None,
    };
    if side.is_some() && receiver == Receiver::None {
        return Err(syn::Error::new(
            sig.ident.span(),
            "a point with a side is a `&self` or `&mut self` method of `Game`",
        ));
    }
    let args = sig
        .inputs
        .iter()
        .filter_map(|input| match input {
            FnArg::Typed(typed) => match &*typed.pat {
                Pat::Ident(pat)
                    if pat.by_ref.is_none() && pat.subpat.is_none() && api_typed(&typed.ty) =>
                {
                    Some(Arg {
                        name: pat.ident.clone(),
                        ty: (*typed.ty).clone(),
                    })
                }
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect::<Vec<_>>();
    if let Some(arg) = args
        .iter()
        .find(|arg| arg.name == "cancel" || arg.name == "result")
    {
        return Err(syn::Error::new(
            arg.name.span(),
            "a point's record has its own `cancel` and `result`: rename the parameter",
        ));
    }
    let borrowed_return = matches!(&sig.output, syn::ReturnType::Type(_, ty) if borrows(ty));
    // Without a receiver, a borrowed value may borrow a text argument: the function gets that text
    // as passed, so the record doesn't carry it.
    let mut args = args;
    if borrowed_return && receiver == Receiver::None {
        args.retain(|arg| !matches!(arg.ty, Type::Reference(_)));
    }
    let returns = match &sig.output {
        syn::ReturnType::Default => Returns::Unit,
        syn::ReturnType::Type(_, ty) if unit_result(ty) => Returns::UnitResult,
        syn::ReturnType::Type(_, ty) if scalar(ty) => Returns::Value((**ty).clone()),
        syn::ReturnType::Type(..) => Returns::Opaque,
    };
    Ok(Shape {
        name,
        side,
        receiver,
        args,
        returns,
        borrowed_return,
        raw,
        raw_only,
        free,
    })
}

/// `Result<()>` (the game's `anyhow` alias) or `Result<(), E>`.
fn unit_result(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    let syn::PathArguments::AngleBracketed(generics) = &last.arguments else {
        return false;
    };
    last.ident == "Result"
        && matches!(generics.args.first(), Some(syn::GenericArgument::Type(Type::Tuple(unit))) if unit.elems.is_empty())
}

/// A scalar the API carries by value.
fn scalar(ty: &Type) -> bool {
    matches!(ty, Type::Path(_)) && api_typed(ty)
}

/// The type a scalar crosses as in a record: `usize`/`isize` as `u64`/`i64`, one width on every
/// host and in a wasm guest (whose `usize` is 32-bit); none for a type that crosses as itself.
pub fn wide(ty: &Type) -> Option<Type> {
    let Type::Path(path) = ty else { return None };
    if path.path.is_ident("usize") {
        Some(syn::parse_quote!(u64))
    } else if path.path.is_ident("isize") {
        Some(syn::parse_quote!(i64))
    } else {
        None
    }
}

/// `tick_return` as `TickReturn`.
pub fn camel(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// Whether the API can carry `ty` to a listener: a scalar, or `&str` with its lifetime elided (a listener
/// may replace that text with its own, which a named lifetime such as `'static` couldn't hold).
fn api_typed(ty: &Type) -> bool {
    const SCALARS: [&str; 14] = [
        "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32",
        "u64", "usize",
    ];
    match ty {
        Type::Path(path) => {
            path.qself.is_none() && SCALARS.iter().any(|scalar| path.path.is_ident(scalar))
        }
        Type::Reference(reference) => {
            reference.mutability.is_none()
                && reference.lifetime.is_none()
                && matches!(&*reference.elem, Type::Path(path) if path.path.is_ident("str"))
        }
        _ => false,
    }
}

impl Shape {
    /// The `PointId` variant of its head: `Tick` for `tick`.
    pub fn variant(&self) -> syn::Ident {
        syn::Ident::new(&camel(&self.name), Span::call_site())
    }

    /// The `PointId` variant of its return.
    pub fn return_variant(&self) -> syn::Ident {
        syn::Ident::new(&camel(&format!("{}_return", self.name)), Span::call_site())
    }

    /// The record its listeners get, in `mod_points`: `TickArgs` for `tick`.
    pub fn record(&self) -> syn::Ident {
        syn::Ident::new(&format!("{}Args", camel(&self.name)), Span::call_site())
    }

    /// Whether the record borrows (a `&str` parameter).
    fn borrows(&self) -> bool {
        self.args
            .iter()
            .any(|arg| matches!(arg.ty, Type::Reference(_)))
    }

    /// The record's declaration in `mod_points`, documented from the function's `target`.
    pub fn record_item(&self, target: &str) -> TokenStream {
        let record = self.record();
        let lifetime = if self.borrows() {
            quote!(<'a>)
        } else {
            quote!()
        };
        let names: Vec<_> = self.args.iter().map(|arg| &arg.name).collect();
        // Text crosses as a `Cow`, so a listener may replace it with an owned `String`.
        let (types, fields): (Vec<_>, Vec<_>) = self
            .args
            .iter()
            .map(|arg| match (&arg.ty, wide(&arg.ty)) {
                (Type::Reference(_), _) => (quote!(&'a str), quote!(::std::borrow::Cow<'a, str>)),
                (ty, Some(wide)) => (quote!(#ty), quote!(#wide)),
                (ty, None) => (quote!(#ty), quote!(#ty)),
            })
            .unzip();
        let inits = self.args.iter().map(|arg| {
            let name = &arg.name;
            match (&arg.ty, wide(&arg.ty)) {
                (Type::Reference(_), _) => quote!(#name: ::std::borrow::Cow::Borrowed(#name)),
                (_, Some(wide)) => quote!(#name: #name as #wide),
                (_, None) => quote!(#name),
            }
        });
        let field_docs = names.iter().map(|name| {
            format!(
                "`{name}`: a head listener may change it, and the function runs with the change."
            )
        });
        let (extra, extra_init) = match &self.returns {
            Returns::Unit | Returns::UnitResult => (
                quote! {
                    /// Set by a head listener: the function returns at once, skipping its body.
                    pub cancel: bool,
                },
                quote!(cancel: false,),
            ),
            Returns::Value(ty) => {
                let ty = wide(ty).unwrap_or_else(|| ty.clone());
                (
                    quote! {
                        /// Set by a head listener: the function returns this at once, skipping its body.
                        /// Return listeners find the value it returns here, and may change it.
                        pub result: Option<#ty>,
                    },
                    quote!(result: None,),
                )
            }
            Returns::Opaque => (quote!(), quote!()),
        };
        let doc = format!(
            "The `{}` point's record: what its listeners get from `{target}`, a slot filled at load.",
            self.name
        );
        let wire = self.wire_items(&record, &lifetime);
        quote! {
            #wire
            #[doc = #doc]
            #[non_exhaustive]
            pub struct #record #lifetime {
                #( #[doc = #field_docs] pub #names: #fields, )*
                #extra
            }
            impl #lifetime #record #lifetime {
                /// The record of a call, before its listeners run (the game's side of the point).
                #[doc(hidden)]
                #[inline]
                pub fn __new(#(#names: #types),*) -> Self {
                    Self { #(#inits,)* #extra_init }
                }
            }
        }
    }

    /// Typed API function constructors, dispatchers and place ids for this point.
    pub fn api_items(&self, target: &str) -> ApiItems {
        let record = self.record();
        let record = if self.borrows() {
            quote!(#record<'_>)
        } else {
            quote!(#record)
        };
        let (host, passed, forwarded, reborrow) = match (self.receiver, self.side) {
            (_, None) => (quote!(), quote!(), quote!(None), quote!()),
            (Receiver::Mut, Some(Side::Server)) => (
                quote!(host: &mut dyn ::mod_api::ServerHost,),
                quote!(&mut *host,),
                quote!(Server(&mut *host)),
                quote!(),
            ),
            (Receiver::Mut, Some(Side::Client)) => (
                quote!(host: &mut dyn ::mod_api::ClientHost,),
                quote!(&mut *host,),
                quote!(Client(&mut *host)),
                quote!(),
            ),
            (Receiver::Mut, Some(Side::Both)) => (
                quote!(host: ::mod_api::HostSide<'_>,),
                quote!(host.reborrow(),),
                quote!(Side(host.reborrow())),
                quote!(let mut host = host;),
            ),
            (_, Some(_)) => (
                quote!(host: &dyn ::mod_api::Host,),
                quote!(host,),
                quote!(Read(host)),
                quote!(),
            ),
        };
        let mut places = vec![(self.name.clone(), "head")];
        if self.has_return() {
            places.push((format!("{}_return", self.name), "return"));
        }
        let mut items = ApiItems::default();
        for (method, place) in places {
            let listener = format_ident!("{}Listener", camel(&method));
            let variant = format_ident!("{}", camel(&method));
            let method = format_ident!("{method}");
            let doc = format!("`{target}`'s {place} function slot.");
            items.listeners.extend(quote! {
                #[doc = #doc]
                pub type #listener = fn(#host args: &mut #record);
                #[doc = #doc]
                pub const fn #method(id: &'static str, priority: i32, inject: &'static str, listener: #listener) -> crate::__private::SlotListener {
                    // SAFETY: the alias checks this place's host and record before erasure.
                    unsafe { crate::__private::SlotListener::api(id, PointId::#variant, listener as *const ()) }.ordered(priority, inject)
                }
            });
            items.dispatch.extend(quote! {
                #[doc = #doc]
                pub fn #method(#host args: &mut #record) {
                    #reborrow
                    crate::__private::fire_api(PointId::#variant, &mut |kind| match kind {
                        crate::__private::ApiKind::Typed(function) => {
                            // SAFETY: the typed constructor checked this place's exact function type.
                            let function = unsafe { ::std::mem::transmute::<*const (), super::listeners::#listener>(function) };
                            function(#passed args);
                        }
                        #[cfg(all(feature = "wasm-host", not(target_arch = "wasm32")))]
                        crate::__private::ApiKind::Forward(forward, index) => forward.forward(index, crate::wire::PointHost::#forwarded, &mut crate::wire::RecordRef(args)),
                    });
                }
            });
            items.variants.extend(quote!(#variant,));
        }
        if self.raw && !self.has_return() {
            let variant = self.return_variant();
            items.variants.extend(quote!(#variant,));
        }
        items
    }

    /// Every field of its record, in order: the arguments, then `cancel` or `result`.
    fn fields(&self) -> Vec<syn::Ident> {
        let extra = match self.returns {
            Returns::Unit | Returns::UnitResult => Some("cancel"),
            Returns::Value(_) => Some("result"),
            Returns::Opaque => None,
        };
        self.args
            .iter()
            .map(|arg| arg.name.clone())
            .chain(extra.map(|name| syn::Ident::new(name, Span::call_site())))
            .collect()
    }

    /// Its listener's contract, which a wasm mod's listener and the game check against each other
    /// at load: the host it gets, then its record's field names and types,
    /// `side|yaw:f64,text:str,cancel:bool`.
    pub fn wire_shape(&self) -> String {
        let host = match (self.receiver, self.side) {
            (_, None) => "none",
            (Receiver::Mut, Some(Side::Server)) => "server",
            (Receiver::Mut, Some(Side::Client)) => "client",
            (Receiver::Mut, Some(Side::Both)) => "side",
            (_, Some(_)) => "read",
        };
        // The record's own field types: a native-width integer as it crosses (`wide`).
        let spelled = |ty: &Type| {
            let ty = wide(ty).unwrap_or_else(|| ty.clone());
            quote!(#ty).to_string().replace(' ', "")
        };
        let types = self.args.iter().map(|arg| match &arg.ty {
            Type::Reference(_) => "str".to_owned(),
            ty => spelled(ty),
        });
        let extra = match &self.returns {
            Returns::Unit | Returns::UnitResult => Some("bool".to_owned()),
            Returns::Value(ty) => Some(format!("Option<{}>", spelled(ty))),
            Returns::Opaque => None,
        };
        let fields = self
            .fields()
            .iter()
            .zip(types.chain(extra))
            .map(|(name, ty)| format!("{name}:{ty}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{host}|{fields}")
    }

    /// How the record crosses to a wasm mod (`mod_points::wire`): its fields lowered in order and
    /// written back, and, in the mod, lifted. Compiled only for wasm mods and the game's wasm
    /// runtime: a native mod builds none of it.
    fn wire_items(&self, record: &syn::Ident, lifetime: &TokenStream) -> TokenStream {
        let fields = self.fields();
        let shape = self.wire_shape();
        let lifted = if self.borrows() {
            quote!(#record<'v>)
        } else {
            quote!(#record)
        };
        let lowered = quote!(#(crate::wire::Wire::lower(&self.#fields)),*);
        let write_back = quote! {
            let taken = (#(crate::wire::taken(&self.#fields, &mut fields)?,)*);
            crate::wire::end(&mut fields)?;
            (#(self.#fields,)*) = taken;
            ::std::result::Result::Ok(())
        };
        quote! {
            #[cfg(target_arch = "wasm32")]
            impl #lifetime crate::wire::Record for #record #lifetime {
                fn lower(&self) -> ::std::vec::Vec<crate::wire::Value> {
                    ::std::vec![#lowered]
                }
                fn write_back(
                    &mut self,
                    fields: ::std::vec::Vec<crate::wire::Value>,
                ) -> ::std::result::Result<(), crate::wire::Mismatch> {
                    let mut fields = fields.into_iter();
                    #write_back
                }
            }
            #[cfg(all(feature = "wasm-host", not(target_arch = "wasm32")))]
            impl #lifetime crate::wire::Fields for #record #lifetime {
                fn lower(&self, fields: &mut impl ::std::iter::Extend<crate::wire::Value>) {
                    fields.extend([#lowered]);
                }
                fn write_back(
                    &mut self,
                    mut fields: impl ::std::iter::Iterator<Item = crate::wire::Value>,
                ) -> ::std::result::Result<(), crate::wire::Mismatch> {
                    #write_back
                }
            }
            #[cfg(target_arch = "wasm32")]
            impl<'v> crate::wire::Lift<'v> for #lifted {
                const SHAPE: &'static str = #shape;
                fn lift(
                    fields: &'v [crate::wire::Value],
                ) -> ::std::result::Result<Self, crate::wire::Mismatch> {
                    let mut fields = fields.iter();
                    let record = Self { #(#fields: crate::wire::field(&mut fields)?,)* };
                    crate::wire::end(&mut fields)?;
                    ::std::result::Result::Ok(record)
                }
            }
        }
    }

    /// Whether its API listeners also run after the body: not when the value borrows.
    pub fn has_return(&self) -> bool {
        !self.borrowed_return
    }

    /// Whether it has a return place: for API listeners, or for raw ones only.
    pub fn at_return(&self) -> bool {
        self.has_return() || self.raw
    }

    /// The raw record's type, declared beside the function: `__TickRaw` for `tick`.
    pub fn raw_record(&self) -> syn::Ident {
        format_ident!("__{}Raw", camel(&self.name))
    }

    /// The raw head listeners' function type: `__TickListener`.
    pub fn raw_listener(&self) -> syn::Ident {
        format_ident!("__{}Listener", camel(&self.name))
    }

    /// The raw return listeners' function type: `__TickReturnListener`.
    pub fn raw_return_listener(&self) -> syn::Ident {
        format_ident!("__{}ReturnListener", camel(&self.name))
    }

    /// What the raw record holds for the function's outcome: `cancel`, a `result` the head may set
    /// to return at once, or nothing (a borrowed value, which no head listener can make).
    pub fn raw_outcome(&self, sig: &syn::Signature) -> RawOutcome {
        match (&self.returns, &sig.output) {
            (Returns::Unit | Returns::UnitResult, _) => RawOutcome::Cancel,
            _ if self.borrowed_return => RawOutcome::None,
            (_, syn::ReturnType::Type(_, ty)) => RawOutcome::Result((**ty).clone()),
            (_, syn::ReturnType::Default) => RawOutcome::Cancel,
        }
    }

    /// The raw record and listener types of this point, declared by the staging pass beside the
    /// function, in its module, so the parameters' types name what they name there. `self_ty` is
    /// the `impl`'s type (none for a free function), standing in for `Self`; `impl_generics` its
    /// lifetimes, bound by the listener type like the function's own.
    ///
    /// The record holds a `&mut` to each parameter (generic over their types, so its declaration
    /// needs none of them) and to the outcome; the listener type names the real types. Text the API
    /// carries is the API record's `Cow`, the same place an API listener changes.
    pub fn raw_items(
        &self,
        sig: &syn::Signature,
        self_ty: Option<&Type>,
        impl_generics: Option<&syn::Generics>,
    ) -> TokenStream {
        let mut lifetimes: Vec<syn::Lifetime> = impl_generics
            .into_iter()
            .flat_map(|generics| generics.lifetimes())
            .chain(sig.generics.lifetimes())
            .map(|param| param.lifetime.clone())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        lifetimes.retain(|lifetime| seen.insert(lifetime.ident.to_string()));
        // An elided lifetime of the `impl`'s type (`impl Game<'_>`) is one lifetime for the receiver
        // and every `Self` in the signature, so it gets a name the listener type binds once.
        // Left elided, each `Self` would get its own, and a parameter such as `fn(&mut Self)`
        // would not match the function's.
        let self_ty = self_ty.map(|ty| {
            struct Name<'a>(&'a mut Vec<syn::Lifetime>, usize);
            impl syn::visit_mut::VisitMut for Name<'_> {
                fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
                    if lifetime.ident == "_" {
                        *lifetime =
                            syn::Lifetime::new(&format!("'__self{}", self.1), lifetime.apostrophe);
                        self.1 += 1;
                        self.0.push(lifetime.clone());
                    }
                }
            }
            let mut ty = ty.clone();
            syn::visit_mut::VisitMut::visit_type_mut(&mut Name(&mut lifetimes, 0), &mut ty);
            ty
        });
        let self_ty = self_ty.as_ref();
        let concrete = |ty: &Type| {
            let mut ty = ty.clone();
            if let Some(self_ty) = self_ty {
                struct Replace<'a>(&'a Type);
                impl syn::visit_mut::VisitMut for Replace<'_> {
                    fn visit_type_mut(&mut self, ty: &mut Type) {
                        if matches!(ty, Type::Path(path) if path.qself.is_none() && path.path.is_ident("Self"))
                        {
                            *ty = self.0.clone();
                        } else {
                            syn::visit_mut::visit_type_mut(self, ty);
                        }
                    }
                }
                syn::visit_mut::VisitMut::visit_type_mut(&mut Replace(self_ty), &mut ty);
            }
            ty
        };
        let record_mut = record_mutability();
        let fields = raw_fields(sig);
        let names: Vec<_> = fields.iter().map(|field| &field.name).collect();
        let params: Vec<_> = (0..fields.len()).map(|i| format_ident!("T{i}")).collect();
        let args: Vec<_> = fields
            .iter()
            .map(|field| {
                if self.args.iter().any(|arg| arg.name == field.name)
                    && matches!(field.ty, Type::Reference(_))
                {
                    quote!(::std::borrow::Cow<'_, str>)
                } else {
                    let ty = concrete(&field.ty);
                    quote!(#ty)
                }
            })
            .collect();
        let (outcome, outcome_param, outcome_arg) = match self.raw_outcome(sig) {
            RawOutcome::Cancel => (
                quote! {
                    /// Set by a head listener: the function returns at once, skipping its body.
                    pub cancel: &'r #record_mut bool,
                },
                quote!(),
                quote!(),
            ),
            RawOutcome::Result(ty) => {
                let ty = concrete(&ty);
                (
                    quote! {
                        /// Set by a head listener: the function returns it at once, skipping its
                        /// body. A return listener finds the value returned here, and may change it
                        /// but not take it.
                        pub result: &'r #record_mut Option<R>,
                    },
                    quote!(R,),
                    quote!(#ty,),
                )
            }
            RawOutcome::None => (quote!(), quote!(), quote!()),
        };
        let (record, listener) = (self.raw_record(), self.raw_listener());
        let empty = fields.is_empty() && matches!(self.raw_outcome(sig), RawOutcome::None);
        let (generics, record_ty) = if empty {
            (quote!(), quote!(#record))
        } else {
            (
                quote!(<'r, #(#params,)* #outcome_param>),
                quote!(#record<'r, #(#args,)* #outcome_arg>),
            )
        };
        let receiver = match (self.receiver, self_ty) {
            (Receiver::Mut, Some(ty)) => quote!(&'r mut #ty,),
            (Receiver::Shared, Some(ty)) => quote!(&'r #ty,),
            _ => quote!(),
        };
        let doc = format!(
            "The `{}` point's raw record: a `&mut` to each argument, internal types included.",
            self.name
        );
        let listener_doc = format!(
            "The `{}` point's raw listener: a mod's function of this type, registered by name.",
            self.name
        );
        let mut items = quote! {
            #[doc = #doc]
            #[doc(hidden)]
            pub struct #record #generics {
                #( pub #names: &'r #record_mut #params, )*
                #outcome
            }
            #[doc = #listener_doc]
            #[doc(hidden)]
            pub type #listener = for<'r, #(#lifetimes),*> fn(#receiver &'r mut #record_ty);
        };
        // At the return the arguments are spent: a return listener gets the receiver (unless the
        // value borrows it) and the value returned, which it may change.
        let receiver = if self.borrowed_return {
            quote!()
        } else {
            receiver
        };
        let return_mut = return_mutability(&sig.output);
        let value = match &sig.output {
            syn::ReturnType::Type(_, ty) => {
                let ty = concrete(ty);
                quote!(&'r #return_mut #ty)
            }
            syn::ReturnType::Default => quote!(),
        };
        let return_listener = self.raw_return_listener();
        items.extend(quote! {
            /// The point's raw return listener: it gets the value the function returns.
            #[doc(hidden)]
            pub type #return_listener = for<'r, #(#lifetimes),*> fn(#receiver #value);
        });
        items
    }
}

/// What a raw record holds for its function's outcome ([`Shape::raw_outcome`]).
pub enum RawOutcome {
    Cancel,
    Result(Type),
    None,
}

// Raw listeners may replace every argument (including by-value and shared-reference arguments)
// and the outcome. Keep the emitted reference and the catalog flag on this one policy.
fn record_mutability() -> Option<syn::token::Mut> {
    Some(Default::default())
}

// Return listeners receive a mutable slot for every explicit return type. Unlike the head
// outcome (absent for borrowed returns), this includes reference-valued slots.
fn return_mutability(output: &syn::ReturnType) -> Option<syn::token::Mut> {
    match output {
        syn::ReturnType::Type(..) => Some(Default::default()),
        syn::ReturnType::Default => None,
    }
}

/// One field of a raw record: a parameter, by its binding, or `arg<i>` for a pattern.
pub struct RawField {
    pub name: syn::Ident,
    /// The parameter's position among the function's inputs, receiver included.
    pub input: usize,
    pub ty: Type,
}

/// The raw record's fields: every typed parameter of `sig`, in order.
pub fn raw_fields(sig: &syn::Signature) -> Vec<RawField> {
    sig.inputs
        .iter()
        .enumerate()
        .filter_map(|(input, arg)| match arg {
            FnArg::Typed(typed) => Some(RawField {
                name: match &*typed.pat {
                    Pat::Ident(pat) if pat.by_ref.is_none() && pat.subpat.is_none() => {
                        pat.ident.clone()
                    }
                    _ => format_ident!("arg{input}"),
                },
                input,
                ty: (*typed.ty).clone(),
            }),
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// Staging metadata for a raw point; type strings use the catalog's token spelling.
#[derive(Debug, serde::Serialize)]
pub struct RawShape {
    pub receiver: RawReceiver,
    pub args: Vec<RawArg>,
    pub outcome: &'static str,
    /// Mutability of the record's cancel/result field, absent when there is no outcome field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_record_mutable: Option<bool>,
    /// Whether the native return listener receives a mutable outcome slot.
    /// Consumers must still forbid reference retargeting and writes through shared pointees.
    pub return_record_mutable: bool,
    pub return_ty: String,
    pub return_class: &'static str,
}

#[derive(Debug, serde::Serialize)]
pub struct RawReceiver {
    pub kind: &'static str,
    /// The pointee type, absent for a free or associated function.
    pub ty: Option<String>,
    pub write_back: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct RawArg {
    pub name: String,
    pub ty: String,
    pub write_back: bool,
    /// Whether the native raw record permits replacement, independent of pointee write-back.
    pub record_mutable: bool,
    pub class: &'static str,
}

/// `return_record_mutable` describes the return alias slot, separately from the head outcome.
/// Describe the raw listener contract, not the API-filtered arguments. Call only for raw-able
/// signatures. Mutable references permit pointee write-back; moving a value does not.
/// Callable syntax is a callback, including FnOnce: syntax alone cannot prove a continuation.
/// Borrowed returns follow the same predicate that suppresses the API return place. Nominal
/// types are plain (no alias resolution); unsupported syntax is explicitly unclassified.
pub fn raw_shape(shape: &Shape, sig: &syn::Signature, owner: Option<&Type>) -> RawShape {
    use quote::ToTokens;
    let spelling = |ty: &Type| ty.to_token_stream().to_string().replace("& ", "&");
    fn class(ty: &Type) -> &'static str {
        match ty {
            Type::BareFn(_) => "callback",
            Type::Reference(r) => class(&r.elem),
            Type::Paren(p) => class(&p.elem),
            Type::Group(g) => class(&g.elem),
            Type::TraitObject(t) => bounds_class(&t.bounds),
            Type::ImplTrait(t) => bounds_class(&t.bounds),
            Type::Path(p) => {
                if let Some(segment) = p.path.segments.last()
                    && segment.ident == "Box"
                    && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
                    && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
                {
                    return class(inner);
                }
                "plain"
            }
            Type::Array(_) | Type::Slice(_) | Type::Tuple(_) | Type::Ptr(_) | Type::Never(_) => {
                "plain"
            }
            _ => "unclassified",
        }
    }
    fn bounds_class(
        bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::token::Plus>,
    ) -> &'static str {
        if bounds.iter().any(|bound| matches!(bound, syn::TypeParamBound::Trait(t)
            if t.path.segments.last().is_some_and(|s| matches!(s.ident.to_string().as_str(), "Fn" | "FnMut" | "FnOnce"))))
        {
            "callback"
        } else {
            "unclassified"
        }
    }
    RawShape {
        receiver: RawReceiver {
            kind: match shape.receiver {
                Receiver::None => "none",
                Receiver::Shared => "shared",
                Receiver::Mut => "mut",
            },
            ty: (shape.receiver != Receiver::None)
                .then(|| owner.map_or_else(|| "Self".into(), spelling)),
            write_back: shape.receiver == Receiver::Mut,
        },
        args: raw_fields(sig)
            .into_iter()
            .map(|field| RawArg {
                name: field.name.to_string(),
                ty: spelling(&field.ty),
                write_back: matches!(&field.ty, Type::Reference(r) if r.mutability.is_some()),
                record_mutable: record_mutability().is_some(),
                class: class(&field.ty),
            })
            .collect(),
        outcome: match shape.raw_outcome(sig) {
            RawOutcome::Cancel => "cancel",
            RawOutcome::Result(_) => "result",
            RawOutcome::None => "none",
        },
        outcome_record_mutable: (!matches!(shape.raw_outcome(sig), RawOutcome::None))
            .then(|| record_mutability().is_some()),
        return_record_mutable: return_mutability(&sig.output).is_some(),
        return_ty: match &sig.output {
            syn::ReturnType::Default => "()".into(),
            syn::ReturnType::Type(_, ty) => spelling(ty),
        },
        return_class: if shape.borrowed_return {
            "borrowed_return"
        } else {
            "plain"
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn return_mutability_matches_native_alias() {
        for (sig, head, expected) in [
            (syn::parse_quote!(fn f() -> u32), Some(true), true),
            (syn::parse_quote!(fn f() -> &u32), None, true),
            (syn::parse_quote!(fn f() -> &mut u32), None, true),
            (syn::parse_quote!(fn f()), Some(true), false),
            (syn::parse_quote!(fn f() -> Result<(), E>), Some(true), true),
        ] {
            let shape = shape(quote!(raw), &sig).unwrap();
            let metadata = raw_shape(&shape, &sig, None);
            assert_eq!(metadata.return_record_mutable, expected);
            assert_eq!(metadata.outcome_record_mutable, head);
            let file: syn::File = syn::parse2(shape.raw_items(&sig, None, None)).unwrap();
            let alias = file
                .items
                .iter()
                .find_map(|item| match item {
                    syn::Item::Type(alias) if alias.ident == shape.raw_return_listener() => {
                        Some(alias)
                    }
                    _ => None,
                })
                .unwrap();
            let Type::BareFn(listener) = &*alias.ty else {
                panic!("not a listener")
            };
            assert_eq!(listener.inputs.len(), usize::from(expected));
            let mutable = listener.inputs.first().is_some_and(|arg| {
                let Type::Reference(reference) = &arg.ty else {
                    panic!("not a slot")
                };
                reference.mutability.is_some()
            });
            assert_eq!(mutable, metadata.return_record_mutable);
        }
    }

    #[test]
    fn record_mutability_matches_native_record() {
        for sig in [
            syn::parse_quote!(fn f(value: u8, shared: &u8, mutable: &mut u8)),
            syn::parse_quote!(fn f(value: u8, shared: &u8, mutable: &mut u8) -> bool),
            syn::parse_quote!(fn f(value: u8, shared: &u8, mutable: &mut u8) -> &u8),
        ] {
            let shape = shape(quote!(raw), &sig).unwrap();
            let metadata = raw_shape(&shape, &sig, None);
            let file: syn::File = syn::parse2(shape.raw_items(&sig, None, None)).unwrap();
            let record = file
                .items
                .iter()
                .find_map(|item| match item {
                    syn::Item::Struct(record) => Some(record),
                    _ => None,
                })
                .unwrap();
            let fields = record
                .fields
                .iter()
                .map(|field| {
                    let Type::Reference(reference) = &field.ty else {
                        panic!("record field must borrow")
                    };
                    (
                        field.ident.as_ref().unwrap().to_string(),
                        reference.mutability.is_some(),
                    )
                })
                .collect::<Vec<_>>();
            let mut expected = metadata
                .args
                .iter()
                .map(|arg| (arg.name.clone(), arg.record_mutable))
                .collect::<Vec<_>>();
            if metadata.outcome != "none" {
                expected.push((
                    metadata.outcome.into(),
                    metadata.outcome_record_mutable.expect("outcome flag"),
                ));
            } else {
                assert_eq!(metadata.outcome_record_mutable, None);
            }
            assert_eq!(fields, expected);
            assert!(metadata.args.iter().all(|arg| arg.record_mutable));
            assert_eq!(
                metadata
                    .args
                    .iter()
                    .map(|arg| arg.write_back)
                    .collect::<Vec<_>>(),
                [false, false, true]
            );
        }
    }

    #[test]
    fn raw_shape_contract() {
        let describe = |sig: syn::Signature| {
            let shape = shape(quote!(raw), &sig).unwrap();
            raw_shape(&shape, &sig, Some(&syn::parse_quote!(Game<'_>)))
        };
        for (sig, kind, ty, writable) in [
            (syn::parse_quote!(fn f()), "none", None, false),
            (
                syn::parse_quote!(fn f(&self)),
                "shared",
                Some("Game < '_ >"),
                false,
            ),
            (
                syn::parse_quote!(fn f(&mut self)),
                "mut",
                Some("Game < '_ >"),
                true,
            ),
        ] {
            let got = describe(sig);
            assert_eq!(got.receiver.kind, kind);
            assert_eq!(got.receiver.ty.as_deref(), ty);
            assert_eq!(got.receiver.write_back, writable);
            assert_eq!(got.outcome, "cancel");
        }
        let got = describe(
            syn::parse_quote!(fn f(a: &mut u8, b: &u8, c: *const u8, d: u8,
            fire: fn(&mut Self, u8), boxed: Box<dyn FnOnce()>, borrowed: &dyn FnMut(),
            unknown: opaque!(), other: &dyn std::any::Any, other_mut: &mut dyn std::any::Any) -> bool),
        );
        assert_eq!(
            got.args.iter().map(|a| a.write_back).collect::<Vec<_>>(),
            [
                true, false, false, false, false, false, false, false, false, true
            ]
        );
        assert_eq!(
            got.args.iter().map(|a| a.class).collect::<Vec<_>>(),
            [
                "plain",
                "plain",
                "plain",
                "plain",
                "callback",
                "callback",
                "callback",
                "unclassified",
                "unclassified",
                "unclassified"
            ]
        );
        assert_eq!(got.args[0].name, "a");
        assert_eq!(got.args[0].ty, "&mut u8");
        assert_eq!(got.outcome, "result");
        assert_eq!(got.return_ty, "bool");
        assert_eq!(got.return_class, "plain");
        for sig in [
            syn::parse_quote!(fn f(&self) -> &u8),
            syn::parse_quote!(fn f(&self) -> Ref<'_, u8>),
        ] {
            let got = describe(sig);
            assert_eq!(got.outcome, "none");
            assert_eq!(got.return_class, "borrowed_return");
        }
    }

    /// `impl Game<'_>`'s lifetime is one lifetime: the receiver and a `Self` in a parameter name
    /// the same one, so a function such as `fn(&mut Self, u8)` matches the function's own.
    #[test]
    fn the_impl_lifetime_is_shared_by_the_receiver_and_self() {
        let sig: syn::Signature =
            syn::parse_quote!(fn guest(&mut self, fire: fn(&mut Self, u8), other: &Self));
        let shape = shape(quote!(side = "both", raw), &sig).unwrap();
        let self_ty: Type = syn::parse_quote!(Game<'_>);
        let items = shape.raw_items(&sig, Some(&self_ty), None).to_string();
        let listener = items
            .split("pub type __GuestListener")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .unwrap();
        assert!(listener.contains("for < 'r , '__self0 >"), "{listener}");
        assert!(
            listener.contains("& 'r mut Game < '__self0 >"),
            "{listener}"
        );
        assert!(
            listener.contains("fn (& mut Game < '__self0 > , u8)"),
            "{listener}"
        );
        assert!(listener.contains("& Game < '__self0 >"), "{listener}");
        assert!(!listener.contains("'_ "), "{listener}");
    }
}
