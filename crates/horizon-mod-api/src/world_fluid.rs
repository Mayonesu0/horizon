//! Mod-owned world fluid registration values.
/// Simulation and bucket settings for a declared world fluid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorldFluid {
    /// Ticks between flow steps, at least one.
    pub tick_delay: i32,
    /// Amount lost per block, from one to seven.
    pub drop_off: i32,
    /// Search distance for a way down, from one to four.
    pub slope_find_distance: i32,
    /// Whether two sources can create another source.
    pub source_conversion: bool,
    /// Whether a filled bucket is registered.
    pub bucket: bool,
}
impl Default for WorldFluid {
    fn default() -> Self {
        Self {
            tick_delay: 5,
            drop_off: 1,
            slope_find_distance: 4,
            source_conversion: false,
            bucket: true,
        }
    }
}
