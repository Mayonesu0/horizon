//! Facts about block states that need no world (`host.block_states()`): a `BlockStateId` is a
//! registry value, not a live object, so these are a read-only service reached from the host
//! rather than handle methods. Both sides read the game's tables; definitions are copied out.
use super::{Backend, BackendRef, BlockSoundType, BlockStateId, BlockStateRange, Host};

/// One property and its legal values in a state definition.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StateProperty {
    /// Property name.
    pub name: String,
    /// Legal serialized values.
    pub values: Vec<String>,
}

impl StateProperty {
    /// Construct a definition entry from a property name and legal values.
    pub fn new(name: String, values: Vec<String>) -> Self {
        Self { name, values }
    }
}

/// A property constraint used when matching a state.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StateConstraint {
    /// Property name.
    pub name: String,
    /// Required serialized value.
    pub value: String,
}

impl StateConstraint {
    /// Construct a property constraint.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// An invalid constraint, identifying its index in the supplied list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StateMatchError {
    /// The definition has no such property.
    UnknownProperty(u32),
    /// The property does not admit this value.
    InvalidValue(u32),
}

/// The block-state facts service: `host.block_states()`.
// exhaustive: its field is private; only `host.block_states()` builds one
pub struct BlockStates<'a> {
    backend: Option<&'a dyn Backend>,
}

/// Reaches [`BlockStates`] from any host.
pub trait HostBlockStates {
    /// Facts about block states, as the game's tables hold them.
    fn block_states(&self) -> BlockStates<'_>;
}

impl<H: Host + ?Sized> HostBlockStates for H {
    fn block_states(&self) -> BlockStates<'_> {
        BlockStates {
            backend: self.backend().map(BackendRef::open),
        }
    }
}

impl BlockStates<'_> {
    /// `BlockState.canBeReplaced()`: whether placing a block where `state` stands replaces it
    /// (air, grass, water, snow layers). None for an unknown state, or a host without a game.
    pub fn replaceable(&self, state: BlockStateId) -> Option<bool> {
        self.backend?.block_state_replaceable(state)
    }
    /// How hard `state` is to break (`BlockBehaviour.Properties.destroyTime`, as
    /// `BlockState.getDestroySpeed` reads it): stone 1.5, obsidian 50, -1 for a block that
    /// can't be broken. None as for `replaceable`.
    pub fn destroy_speed(&self, state: BlockStateId) -> Option<f32> {
        self.backend?.block_state_destroy_speed(state)
    }
    /// `state`'s sounds (`BlockState.getSoundType()`). None as for `replaceable`.
    pub fn sound(&self, state: BlockStateId) -> Option<BlockSoundType> {
        self.backend?.block_state_sound(state)
    }
    /// `BlockState.hasBlockEntity()`. None as for `replaceable`.
    pub fn has_block_entity(&self, state: BlockStateId) -> Option<bool> {
        self.backend?.block_state_has_block_entity(state)
    }
    /// `BlockState.getMapColor`: the state's `MapColor.id`, 0 for none (`MapColor.NONE`).
    /// None as for `replaceable`.
    pub fn map_color(&self, state: BlockStateId) -> Option<u8> {
        self.backend?.block_state_map_color(state)
    }
    /// The block's property names and legal serialized values. None without resolved tables.
    pub fn definition(&self, block: crate::BlockKey) -> Option<Vec<StateProperty>> {
        self.backend?.block_state_definition(block)
    }

    /// Match a state against a block and partial property constraints. Unknown properties and
    /// values are errors even when the state belongs to a different block. None means the
    /// owning thread has no resolved tables or the state is unknown.
    pub fn matches(
        &self,
        state: BlockStateId,
        block: crate::BlockKey,
        constraints: &[StateConstraint],
    ) -> Option<Result<bool, StateMatchError>> {
        self.backend?.block_state_matches(state, block, constraints)
    }

    /// The states of `block` (`minecraft:oak_stairs`), `StateDefinition.getPossibleStates`.
    /// None before the mapping is loaded, or for a host without a game.
    pub fn states_of(&self, block: crate::BlockKey) -> Option<BlockStateRange> {
        self.backend?.block_states_of(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mod's test double: a host with no game behind it.
    struct Double;
    impl Host for Double {}
    mod_api::impl_host_areas!(Double);

    #[test]
    fn a_host_without_a_game_answers_none() {
        let facts = Double.block_states();
        assert_eq!(facts.replaceable(BlockStateId(1)), None);
        assert_eq!(facts.destroy_speed(BlockStateId(1)), None);
        assert_eq!(facts.sound(BlockStateId(1)), None);
        assert_eq!(facts.has_block_entity(BlockStateId(1)), None);
        assert_eq!(facts.map_color(BlockStateId(1)), None);
        assert_eq!(facts.states_of(crate::BlockKey(0)), None);
    }
}
