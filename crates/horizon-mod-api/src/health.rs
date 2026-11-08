//! Moved here from the game so mods can name it.
#[allow(unused_imports)]
use crate::*;
pub use horizon_world::living_damage::{DamageContainer, Reduction, ReductionChange};
/// A hit's `DamageSource` for `HURT_SERVER`: its type (`minecraft:fall`),
/// the entity ids of `getEntity` (`causing`) and `getDirectEntity`
/// (`direct`), 0 for none or not known, and where it comes from
/// (`getSourcePosition`), none when not known.
#[derive(Clone, Copy, Debug, PartialEq)]
// exhaustive: the game builds it with a literal; mods only read it
pub struct HurtSource<'a> {
    pub damage_type: &'a str,
    pub causing: i32,
    /// The projectile's entity id when the damage type names one (an arrow, a thrown
    /// entity, an owned wither skull), including an ownerless projectile; the attacker
    /// itself for melee when known. 0 when the source names none, including `magic()`
    /// dealt during a projectile's impact (an ownerless wither skull, a tipped arrow's
    /// instant harm).
    pub direct: i32,
    pub position: Option<[f64; 3]>,
}
/// Where the entity a hit lands on stands and looks (`getViewVector`), for
/// `HURT_SERVER`.
#[derive(Clone, Copy, Debug, PartialEq)]
// exhaustive: the game builds it with a literal; mods only read it
pub struct Victim {
    pub position: [f64; 3],
    pub view: [f64; 3],
}
/// What a player's raised blocking item does to a hit, as `native_living_shield_block`
/// may change it (`LivingShieldBlockEvent`).
#[derive(Clone, Copy, Debug, PartialEq)]
// exhaustive: the game builds it with a literal; mods only read it
pub struct ShieldBlock {
    /// The player counts as blocking.
    pub blocked: bool,
    /// The damage the item stops, kept within 0 and the hit's damage.
    pub blocked_damage: f32,
    /// The durability the item loses (`ItemDamageFunction` of what it stops). Below 0,
    /// vanilla's `ItemDamageFunction` of the final `blocked_damage` applies.
    pub shield_damage: i32,
}
