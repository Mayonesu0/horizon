# Controlling spawns

Four hooks stand in for the checks and setup around a mob. All run on the server, get the read-only host, and fire on replayed ticks, so a listener must be a pure function of its inputs. `finalize_spawn` can also change the mob it is about to finalize, through the editor it is handed, and the pack's group data; it changes nothing else.

| Moment | Hook | Answer |
|---|---|---|
| The spawn predicate for a type and position | `mob_spawn_placement_check` | `NativeSpawnCheck::{Default, Succeed, Fail}` |
| The mob's own position and obstruction checks | `mob_spawn_position_check` | `NativeSpawnCheck::{Default, Succeed, Fail}` |
| Just before `finalizeSpawn` | `finalize_spawn` | `Flow::Cancel`, `spawn_cancelled`, a new `difficulty`, edits to `mob`, a new `group` |
| Each tick, before a mob's `checkDespawn` | `mob_despawn` | `NativeDespawn::{Default, Allow, Deny}` |

## Stopping a spawn or leaving a mob plain

`finalize_spawn` has two cancels, as in NeoForge. Returning `Flow::Cancel` skips finalization: the mob still spawns, but with no equipment, attributes or class setup. Setting `event.spawn_cancelled = true` adds no mob at all.

```rust,ignore
fn finalize_spawn(&self, _: &dyn mod_api::Host, event: &mut mod_api::FinalizeSpawnEvent<'_>) -> mod_api::Flow {
    if event.kind == "minecraft:zombie" && event.reason == mod_api::NativeSpawnReason::Natural {
        event.spawn_cancelled = true;
    }
    mod_api::Flow::Continue
}
```

`finalize_spawn` fires only where vanilla calls `finalizeSpawn`, so a breeding, an infested block's silverfish or a thrown egg's chick does not tell it.

For `/summon minecraft:ender_dragon` without NBT, command placement starts at zero
rotation and consumes no random yaw draw. Skipping finalization also skips its
follow-range and handedness draws. Spawn eggs retain their random placement yaw;
an explicit NBT compound follows the load route without finalization.

## Changing the mob before it is finalized

`event.mob` is the mob itself, an `UnspawnedMobRef`: it is in no level yet, so it is not an `EntityRef`, and only this
hook call can use it. Read it through the host (`kind`, `position`, `equipment`, `attribute_base`, `attribute_value`,
`tags`, `custom_name`) and change it through `event.edits`, which reaches only this mob: equipment (an API-owned `ItemStack`), an attribute's base or a permanent modifier, tags and the custom name. An edit applies at once, so the
reads and the next listener see it. Once the hook returns, the handle answers `UnspawnedMobError::Expired`, as does a
handle from another call passed to this call's editor.

```rust,ignore
fn finalize_spawn(&self, host: &dyn mod_api::Host, event: &mut mod_api::FinalizeSpawnEvent<'_>) -> mod_api::Flow {
    if event.mob.kind(host).is_ok_and(|kind| kind.path() == "zombie") {
        let health = mod_api::Identifier::parse("minecraft:max_health").unwrap();
        let _ = event.edits.set_attribute_base(event.mob, &health, 40.0);
        let _ = event.edits.add_tag(event.mob, "mymod:tough");
    }
    mod_api::Flow::Continue
}
```

Listeners run before finalization, as in NeoForge. Finalization's own rolls still follow them: equipment it rolls for a
slot replaces what a listener put there, and the attribute bonuses it adds apply on top of a listener's base. Return
`Flow::Cancel` to keep exactly what the listener left, since no finalization runs. Attribute edits reach the
attributes a mob keeps itself (max health, movement speed, follow range, attack damage, knockback resistance,
spawn reinforcements, jump strength, burning time, flying speed); the others answer `Unsupported` until the mob has
joined, when a `LivingRef` writes them. The ender dragon's handle answers its kind and position, and `Unsupported`
for the rest. A mob finalization itself creates (a chicken jockey's chicken, a zombie horse's rider) fires the hook
again with a handle of its own.

The hook still fires on replayed ticks with the read-only host, so the editor and `group` are the only things a
listener changes, and it must change them the same way every time: a pure function of the event.

`event.group` is the pack's `SpawnGroupData` as plain data (`NativeSpawnGroup`, variants named as vanilla saves them):
read it, change it, set it or clear it, and finalization reads what is left. Every site the hook fires at hands it
`None` today: the game does not yet thread group data from one member of a natural spawning pack to the next, and
chunk generation's animals share it only inside the generator, out of the hook's reach. Setting a group still reaches this
mob's finalization; carrying it on to the pack's next member comes with that threading.

See [the hook notes](../reference/hook-notes.md#finalize-spawn) for what each spawn site covers and what is left out (breeding, loads, worldgen structure mobs).

## Changing how large a spawn group grows

A natural spawn adds mobs of one type to a group until the group reaches the type's limit (`Mob.getMaxSpawnClusterSize`: 4 by default, more for horses, wolves and schooling fish, 1 for ghasts and pillagers). The entity part of the type changes that limit with `EntityPart::spawn_cluster_size`, which gets the read-only host, the mob just spawned, why and from what it spawned, and the limit vanilla would use. Return `Some(limit)` to replace it, or `None` to leave it to the next part and then to vanilla. The first part to answer wins, and the group stops growing once its size reaches the limit.

```rust,ignore
struct ZombiePacks;
#[mod_api::horizon_part]
impl mod_api::EntityPart for ZombiePacks {
    fn spawn_cluster_size(
        &self,
        _: &dyn mod_api::Host,
        _: mod_api::UnspawnedMobRef,
        _: mod_api::NativeSpawnReason,
        _: Option<mod_api::NativeSpawner>,
        vanilla: u32,
    ) -> Option<u32> {
        Some(vanilla * 2)
    }
}
```

Only natural spawning's packs ask it: vanilla has no group limit anywhere else. A type with no part keeps vanilla's limit and costs nothing. The mob is the same `UnspawnedMobRef` as `finalize_spawn`'s, finalized and not yet in a level, and the call runs on replayed ticks like `finalize_spawn`, so an answer must be a pure function of its inputs. Attach the part with `r.entity_part("minecraft:zombie", ZombiePacks)` as [entity parts](../concepts/trait-mods/023-entity-parts-and-mob-goals.md) describe.

## Keeping or removing a mob

`mob_despawn` fires once per tick for every mob in a loaded chunk, just before vanilla's `Mob.checkDespawn`, persistent mobs and mobs in peaceful mode included. `Allow` discards the mob, `Deny` keeps it, `Default` leaves vanilla's rules to decide. An answer other than `Default` skips all of vanilla's checks, so a `Deny` keeps a hostile mob in peaceful mode and neither answer takes the idle-despawn `nextInt(800)` draw.

```rust,ignore
fn mob_despawn(&self, _: &dyn mod_api::Host, event: &mut mod_api::MobDespawnEvent<'_>) {
    if event.kind == "minecraft:zombie" {
        *event.result = mod_api::NativeDespawn::Deny;
    }
}
```

The wither and the ender dragon override `checkDespawn` in vanilla, so the event is not fired for them.

## Adding or removing a custom spawner

The level's custom spawners (phantoms, patrols, cats, village sieges, the wandering trader) are a list a mod edits once, in `modify_custom_spawners`. A mod's own spawner is a key plus `custom_spawner_tick`, which runs every tick while mob spawning is on:

```rust,ignore
fn modify_custom_spawners(&self, _: &dyn mod_api::Host, event: &mut mod_api::ModifyCustomSpawnersEvent<'_>) {
    // no phantoms in this world
    event.spawners.remove(&mod_api::SpawnerEntry::Builtin(mod_api::SpawnerKind::Phantom));
    event.spawners.add(mod_api::SpawnerKey::new("mymod:rat_swarm").unwrap());
}

fn custom_spawner_tick(&self, host: &mut dyn mod_api::ServerHost, event: &mut mod_api::CustomSpawnerTickEvent<'_>) {
    if event.key.as_str() == "mymod:rat_swarm" && event.spawn_enemies {
        // spawn through `host`
    }
}
```

Every mod is asked about every key, so a mod ignores keys it does not own. See [the hook notes](../reference/hook-notes.md#custom-spawners).
