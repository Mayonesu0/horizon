//! Raw call-site routing and adapters; every kind uses the same typed wrap constructor.
use std::collections::BTreeSet;

#[derive(Clone, Debug, serde::Deserialize)]
pub(super) struct Site {
    pub name: String,
    pub target: String,
    pub call: String,
    pub ctx: String,
    pub ordinal: usize,
    #[serde(default)]
    pub private: bool,
    pub listener: String,
    pub record: String,
    pub output: String,
}

impl From<horizon_mod_api_gen::sites::Site> for Site {
    fn from(site: horizon_mod_api_gen::sites::Site) -> Self {
        Self {
            name: site.name,
            target: site.target,
            call: site.call,
            ctx: site.ctx,
            ordinal: site.ordinal,
            listener: site.listener,
            record: site.record,
            output: site.output,
            private: true,
        }
    }
}

impl Site {
    pub(super) fn constructor(&self) -> String {
        self.export().to_ascii_lowercase()
    }

    fn export(&self) -> String {
        horizon_mod_api_gen::sites::source::facade_name(&self.target, &self.call, self.ordinal)
    }

    pub(super) fn facade(
        &self,
        exports: &mut BTreeSet<String>,
        constructors: &mut BTreeSet<String>,
        crates: &mut BTreeSet<String>,
    ) {
        crates.insert(
            self.listener
                .split("::")
                .next()
                .expect("listener crate")
                .into(),
        );
        let prefix = self
            .listener
            .strip_suffix("Listener")
            .expect("listener suffix");
        let export = self.export();
        for suffix in ["Listener", "Receiver", "Output"] {
            exports.insert(format!(
                "    pub use {prefix}{suffix} as __{export}{suffix};\n"
            ));
        }
        exports.insert(format!("    pub use {} as __{export}Args;\n", self.record));
        let listener = format!("__{export}Listener");
        constructors.insert(format!("    /// Registers this site's exact typed wrap listener.\n    pub const fn {}(id: &'static str, priority: i32, injection: &'static str, f: {listener}) -> mod_points::__private::SlotListener {{\n        // SAFETY: the function's exact type is checked before erasure.\n        unsafe {{ mod_points::__private::SlotListener::site(id, mod_points::SiteId::{}, priority, injection, f as *const ()) }}\n    }}\n", self.constructor(), self.name));
    }
}

pub(super) fn adapter(kind: horizon_mod_package::manifest::inject::Kind, f: &str) -> String {
    use horizon_mod_package::manifest::inject::Kind;
    match kind {
        Kind::BeforeCall | Kind::ModifyCallArg => format!("|receiver, args, original| {{ {f}(receiver, args); original(receiver, args) }}"),
        Kind::AfterCall => format!("|receiver, args, original| {{ let value = original(receiver, args); {f}(receiver, &value); value }}"),
        Kind::ModifyValue => format!("|receiver, args, original| {{ let value = original(receiver, args); {f}(receiver, value) }}"),
        Kind::WrapCall => f.into(),
        Kind::WrapIf => format!("|receiver, args, original| {{ if {f}(receiver, &*args) {{ original(receiver, args); }} }}"),
        _ => unreachable!("a site kind"),
    }
}

#[cfg(test)]
mod tests;
