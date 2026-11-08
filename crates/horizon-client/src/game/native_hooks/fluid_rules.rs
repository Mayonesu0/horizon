use super::*;

impl GameLevel<'_> {
    /// How a mod's world fluid flows at `pos`, starting from what it declared (NeoForge's
    /// `BaseFlowingFluid` and `FluidType` getters: `getTickDelay`, `getDropOff`,
    /// `getSlopeFindDistance`, `canConvertToSource`). `fluid` is the id it was declared with.
    /// Asked for every flow step and every cell a spread looks at: answer fast. An answer
    /// outside what `WorldFluid` accepts is brought back within it. Server-side.
    #[inline(always)]
    pub(in crate::game) fn native_fluid_rules(&self, call: mod_api::__private::FluidRulesCall<'_>) {
        self.fire_fluid_rules(call);
    }
}
