<!-- chapter: Block-state facts -->
<!-- separator: 1 -->

### Block-state facts

A `BlockStateId` is a registry value, not a live object, so the facts about one that need no world are a small
read-only service on any host, both sides: `host.block_states()` (import `HostBlockStates`). Each reads the game's own
tables. Fact queries and matching allocate nothing; `definition` copies the property data for the caller:

| Method | Vanilla |
|---|---|
| `replaceable(state) -> Option<bool>` | `BlockState.canBeReplaced()` |
| `destroy_speed(state) -> Option<f32>` | `BlockBehaviour.Properties.destroyTime` (`getDestroySpeed`): stone 1.5, -1 unbreakable |
| `sound(state) -> Option<BlockSoundType>` | `BlockState.getSoundType()`: volume, pitch and the break, step, place, hit and fall events |
| `has_block_entity(state) -> Option<bool>` | `BlockState.hasBlockEntity()` |
| `map_color(state) -> Option<u8>` | `BlockState.getMapColor`: the `MapColor.id` a map item colours a column by, 0 for none |
| `states_of(block) -> Option<BlockStateRange>` | `StateDefinition.getPossibleStates`, as the block's consecutive ids |
| `definition(block)` | Property names and legal serialized values |
| `matches(state, block, constraints)` | Partial property match with typed invalid-property and invalid-value errors |

Use `StateConstraint` for each partial constraint; `StateMatchError` identifies an invalid property or value by index. `StateProperty` describes a property's legal values. These types are shared by state-definition services.

None for an unknown state or block, and on a host with no game behind it (a mod's test double).

### Fluid-state facts

Import `HostFluidStates` to reach `host.fluid_states()` on either owning side.
`definition(key)` returns `Some(FluidDefinition::World { default, properties })` for a world fluid,
`Some(FluidDefinition::TankOnly)` for a registered tank-only fluid, and `None` without a loaded registry.
The properties and typed default come from the loaded preparation, not a second vanilla table.
These metadata reads are available in setup, reload and world callbacks after the fluid keys resolve,
not during registration or configuration.

`validate(key, constraints)` checks partial `StateConstraint` values in input order without matching
any particular state. It returns `Result<(), FluidStateError>`: `UnknownProperty(index)`,
`InvalidValue(index)` or `Duplicate(index)` identifies the first offending input.
`NoWorldState` means the fluid is tank-only or the host has no definition; use `definition` to distinguish them.

`type_properties(key)` returns a `FluidTypeProperties` record, or `None` without a loaded registry or for
a key it doesn't hold; `host.fluid_key(name)` is `None` for an unknown name. The record is the same in every
level: `description_id`, `light_level`, `fall_distance_modifier`, and whether the fluid can push entities, be
swum in, drown, extinguish, hydrate and carry boats. Flowing forms answer as their source. The empty fluid and
tank-only fluids emit no light, do nothing to an entity and have a fall modifier of 1. Anything that depends on
a level, such as lava's current or source conversion, isn't in the record.

`FluidState::matches(constraints)` is a pure local check of carried vanilla fields. Unknown properties
or noncanonical serialized values return false. A falling source remains a source; a flowing state
of level 8 remains flowing. Empty constraints match any state. Validate separately for indexed errors.
Neither matching nor validation allocates; explicitly copying a definition may allocate.
Unresolved saved tank identities remain `FluidIdentity::Unresolved(handle)`, not world states or tank-only keys.
