//! An entity's invulnerability for native server mods, through an
//! `EntityRef` (docs/modding/src/how-to/native-mods.md, "Entity and level handles"): the
//! `Invulnerable` flag (`Entity.permanentlyInvulnerable`) and
//! `LivingEntity.damageCooldownTime`, the ticks after a hit in which a
//! weaker one does only the difference. Horizon has no counterpart yet of
//! 26.3's own `Entity.invulnerableTime` timer, so neither is offered. Reads
//! return None and writes false for a stale handle, on a client, and for a
//! kind Horizon keeps no such state for.
use super::{Game, commands::Target};
use horizon_client::mobs::Damage;
use mod_api::EntityRef;

impl Game<'_> {
    /// A player's body, where it is kept now.
    pub(super) fn native_player_body(
        &self,
        t: Target,
    ) -> Option<&horizon_client::motion::PlayerMotion> {
        match t {
            Target::Player => self.player.as_ref(),
            Target::Guest(conn) => self.lan.as_ref()?.hub.guest(conn)?.motion.as_deref(),
            _ => None,
        }
    }
    fn native_player_body_mut(
        &mut self,
        t: Target,
    ) -> Option<&mut horizon_client::motion::PlayerMotion> {
        match t {
            Target::Player => self.player.as_mut(),
            Target::Guest(conn) => self.lan.as_mut()?.hub.guests.motion_mut(conn),
            _ => None,
        }
    }

    /// `Entity.isPermanentlyInvulnerable`: the `Invulnerable` flag, which
    /// only damage that bypasses invulnerability (the void, `/kill`) or a
    /// creative player gets past. Mobs keep it; Horizon keeps no such flag
    /// for players or other kinds yet, so they read None.
    pub(super) fn entity_permanently_invulnerable(&self, entity: EntityRef) -> Option<bool> {
        match self.native_entity_target(entity)? {
            Target::Mob(id) => Some(self.mobs.iter().find(|m| m.id == id)?.invulnerable_flag),
            _ => None,
        }
    }
    /// `Entity.setInvulnerable`: sets a mob's `Invulnerable` flag, saved
    /// with it. False for the other kinds.
    pub(super) fn entity_set_invulnerable(
        &mut self,
        entity: EntityRef,
        invulnerable: bool,
    ) -> bool {
        let Some(Target::Mob(id)) = self.native_entity_target(entity) else {
            return false;
        };
        let Some(m) = self.mobs.iter_mut().find(|m| m.id == id) else {
            return false;
        };
        m.invulnerable_flag = invulnerable;
        true
    }
    /// `Entity.hurtServer` of `amount` from a source of `damage_type`
    /// (`minecraft:generic`'s shape), `direct` the entity that dealt it (a
    /// shot arrow) and `causing` the one responsible for it (its shooter):
    /// the engine's own hurt paths take it — the damage type, the attacker,
    /// armor, the cooldown and every hurt hook kept, the damage event the
    /// hit already broadcasts — by kind: the player's and mobs' for the
    /// living, the item, dragon and crystal branches for the damageable
    /// rest. True when the damage was taken. None for a stale handle, false
    /// for a kind that cannot be hurt or an unregistered damage type. Stale
    /// direct/causing handles are treated as absent sources.
    pub(super) fn entity_hurt(
        &mut self,
        entity: EntityRef,
        damage_type: &str,
        amount: f32,
        direct: Option<EntityRef>,
        causing: Option<EntityRef>,
    ) -> Option<bool> {
        let target = self.native_entity_target(entity)?;
        let direct = direct.and_then(|e| self.native_entity_target(e));
        let causing = causing.and_then(|e| self.native_entity_target(e));
        // `DamageSource(type, cause, at)`: the source position the knockback
        // pushes away from is the direct entity's.
        let from = direct.and_then(|t| self.target_position(t));
        Some(self.hurt_target(target, amount, damage_type, direct, causing, from))
    }

    /// `Entity.isInvulnerableTo` for a source of `damage_type`: #842's own
    /// decision, the guard every mob damage hook runs after — dead or
    /// removed, fire immunity, or the `Invulnerable` flag nothing but
    /// `#bypasses_invulnerability` gets past. None for a stale handle or a
    /// kind with no such decision (the player's paths answer their refusals
    /// at hurt time).
    pub(super) fn entity_invulnerable_to_damage(
        &self,
        entity: EntityRef,
        damage_type: &str,
    ) -> Option<bool> {
        let Target::Mob(id) = self.native_entity_target(entity)? else {
            return None;
        };
        let m = self.mobs.iter().find(|m| m.id == id)?;
        let source = Damage::of(Self::damage_kind(&self.motion.health.damage, damage_type));
        Some(m.refuses_damage(&source))
    }

    /// Whether `source`'s damage may hit `entity` through their teams
    /// (`Player.canHarmPlayer`, `Team.allowFriendlyFire`,
    /// `DamageSource.doTeamsAllowDamage`): allies without friendly fire
    /// cannot. Entities without a team always can. None for a stale handle.
    pub(super) fn entity_entities_team_damage_allowed(
        &self,
        entity: EntityRef,
        source: EntityRef,
    ) -> Option<bool> {
        let own = self.native_target_team(self.native_entity_target(entity)?)?;
        let their = self.native_target_team(self.native_entity_target(source)?)?;
        Some(horizon_client::scoreboard::teams_allow_damage(own, their))
    }

    /// The team an entity is on: a mob's own or its tamed owner's, the
    /// acting player's, a guest's by name.
    pub(super) fn native_target_team(
        &self,
        t: Target,
    ) -> Option<Option<&horizon_client_core::scoreboard::Team>> {
        Some(match t {
            Target::Player => self.player_team(),
            Target::Guest(conn) => self
                .score
                .board
                .players_team(&self.command_guest(conn)?.name),
            Target::Mob(id) => self.mob_team(id),
            _ => None,
        })
    }

    /// `Entity.isInvulnerableToPiercingWeapon` (`isInvulnerable`): the
    /// `Invulnerable` flag; the temporary window `Entity.invulnerableTime`
    /// opens does not exist yet (#916), so no hit answers through it. None
    /// for a stale handle or a kind without the flag.
    pub(super) fn entity_invulnerable_to_piercing_weapon(&self, entity: EntityRef) -> Option<bool> {
        match self.native_entity_target(entity)? {
            Target::Mob(id) => Some(self.mobs.iter().find(|m| m.id == id)?.invulnerable_flag),
            _ => None,
        }
    }

    /// `LivingEntity.damageCooldownTime`: 20 after a hit, counting down;
    /// while it is above 10 a hit does only what it deals beyond the last
    /// one. Mobs and players.
    pub(super) fn entity_damage_cooldown_time(&self, entity: EntityRef) -> Option<i32> {
        match self.native_entity_target(entity)? {
            Target::Mob(id) => Some(self.mobs.iter().find(|m| m.id == id)?.invulnerable),
            t => Some(self.native_player_body(t)?.health.hurt.cooldown),
        }
    }
    /// Sets `LivingEntity.damageCooldownTime`, as mods do to let rapid hits
    /// through (0). Mobs and players.
    pub(super) fn entity_set_damage_cooldown_time(
        &mut self,
        entity: EntityRef,
        ticks: i32,
    ) -> bool {
        let Some(t) = self.native_entity_target(entity) else {
            return false;
        };
        match t {
            Target::Mob(id) => match self.mobs.iter_mut().find(|m| m.id == id) {
                Some(m) => {
                    m.invulnerable = ticks;
                    true
                }
                None => false,
            },
            t => match self.native_player_body_mut(t) {
                Some(p) => {
                    p.health.hurt.cooldown = ticks;
                    true
                }
                None => false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::native_entity_refs::tests::with_game;
    use horizon_client::mobs::Mob;
    use horizon_core::geometry::Vec3;
    use mod_api::{EntityRef, NativeWorldEpoch};

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn review_unknown_damage_type_is_refused() {
        with_game(|game| {
            let pig = game.res.mob_types().get("minecraft:pig").unwrap().clone();
            game.mobs.push(Mob::new(300001, pig, Vec3::ZERO, 0.0));
            let victim = game.entity(300001).unwrap();
            assert_eq!(
                victim.hurt(game, "missing:unknown", 5.0, None, None),
                Some(false)
            );
            assert_eq!(game.mobs[0].health, 10.0);
        });
    }

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn review_fire_immune_query() {
        with_game(|game| {
            let blaze = game.res.mob_types().get("minecraft:blaze").unwrap().clone();
            game.mobs.push(Mob::new(300001, blaze, Vec3::ZERO, 0.0));
            let victim = game.entity(300001).unwrap();
            assert_eq!(
                victim.invulnerable_to_damage(game, "minecraft:in_fire"),
                Some(true)
            );
        });
    }

    struct SourceProbe(std::sync::Arc<std::sync::Mutex<Vec<(i32, i32)>>>);
    #[mod_api::__subscriptions]
    impl mod_api::Hooks for SourceProbe {
        fn living_incoming_damage(
            &self,
            _: &mut dyn mod_api::LevelRandomHost,
            event: &mut mod_api::LivingIncomingDamageEvent<'_>,
        ) -> mod_api::Flow {
            self.0
                .lock()
                .unwrap()
                .push((event.source.direct, event.source.causing));
            mod_api::Flow::Continue
        }
    }

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn review_direct_entity_preserved() {
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mods: mod_api::Mods = vec![mod_api::ModEntry::new(
            "probe",
            Box::new(SourceProbe(heard.clone())),
        )]
        .into();
        mod_api::testing::with_mods(mods, || {
            with_game(|game| {
                let pig = game.res.mob_types().get("minecraft:pig").unwrap().clone();
                for id in 300001..=300003 {
                    game.mobs.push(Mob::new(id, pig.clone(), Vec3::ZERO, 0.0));
                }
                let victim = game.entity(300001).unwrap();
                let direct = game.entity(300002).unwrap();
                let causing = game.entity(300003).unwrap();
                for damage_type in [
                    "minecraft:arrow",
                    "minecraft:mob_attack",
                    "minecraft:generic",
                ] {
                    assert!(game.entity_set_damage_cooldown_time(victim, 0));
                    assert_eq!(
                        victim.hurt(game, damage_type, 1.0, Some(direct), Some(causing)),
                        Some(true)
                    );
                }
                let mut player =
                    horizon_client::motion::PlayerMotion::new(game.motion, Vec3::ZERO).unwrap();
                player.entity_id = 7;
                game.player = Some(player);
                game.lan_acting_uuid = Some(1);
                game.lan_acting_entity = Some(7);
                game.set_game_mode(horizon_client::command::args::GameMode::Survival);
                let victim = game.entity(7).unwrap();
                assert_eq!(
                    victim.hurt(game, "minecraft:arrow", 1.0, Some(direct), Some(causing)),
                    Some(true)
                );
            })
        });
        assert_eq!(*heard.lock().unwrap(), vec![(300002, 300003); 4]);
    }

    /// A listening probe counting the incoming-damage hooks a hit fires.
    struct Probe(std::sync::Arc<std::sync::Mutex<Vec<u32>>>);
    #[mod_api::__subscriptions]
    impl mod_api::Hooks for Probe {
        fn living_incoming_damage(
            &self,
            _: &mut dyn mod_api::LevelRandomHost,
            event: &mut mod_api::LivingIncomingDamageEvent<'_>,
        ) -> mod_api::Flow {
            self.0.lock().unwrap().push(event.entity as u32);
            mod_api::Flow::Continue
        }
    }

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn hurt_routes_to_the_engines_damage_paths() {
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mods: mod_api::Mods = vec![mod_api::ModEntry::new(
            "probe",
            Box::new(Probe(heard.clone())),
        )]
        .into();
        mod_api::testing::with_mods(mods, || {
            with_game(|game| {
                let pig = game.res.mob_types().get("minecraft:pig").unwrap().clone();
                game.mobs.push(Mob::new(300001, pig, Vec3::ZERO, 0.0));
                let victim = game.entity(300001).unwrap();
                // An accepted hit: taken, the health drops, the damage hooks
                // the engine's own path fires are the ones that fire.
                assert_eq!(
                    victim.hurt(game, "minecraft:generic", 5.0, None, None),
                    Some(true)
                );
                assert_eq!(
                    game.mobs[0].health, 5.0,
                    "the pig took it (10 max health, no armor)"
                );
                // The cooldown: a weaker hit inside the window is refused
                // (`LivingEntity.hurtServer`'s `damageCooldownTime` rule).
                assert_eq!(
                    victim.hurt(game, "minecraft:generic", 1.0, None, None),
                    Some(false)
                );
                assert_eq!(game.mobs[0].health, 5.0, "the window held");
                // The flag refuses all but what bypasses it, and the query
                // answers the same decision the hit ran.
                assert!(game.entity_set_invulnerable(victim, true));
                assert_eq!(
                    victim.hurt(game, "minecraft:generic", 5.0, None, None),
                    Some(false)
                );
                assert_eq!(
                    victim.invulnerable_to_damage(game, "minecraft:generic"),
                    Some(true)
                );
                assert_eq!(
                    victim.invulnerable_to_damage(game, "minecraft:out_of_world"),
                    Some(false),
                    "out of world bypasses the flag"
                );
                assert_eq!(
                    victim.hurt(game, "minecraft:out_of_world", 100.0, None, None),
                    Some(true),
                    "and so does the hit"
                );
                assert!(game.mobs[0].health <= 0.0);
                // The piercing query follows the flag.
                assert_eq!(victim.invulnerable_to_piercing_weapon(game), Some(true));
                // A stale handle reads None and hurts nothing.
                let stale = EntityRef::new(NativeWorldEpoch::new(victim.epoch().get() + 1), 300001);
                assert_eq!(stale.hurt(game, "minecraft:generic", 5.0, None, None), None);
                assert_eq!(
                    stale.invulnerable_to_damage(game, "minecraft:generic"),
                    None
                );
                assert_eq!(stale.invulnerable_to_piercing_weapon(game), None);
            });
        });
        // Every hit that entered the engine's pipeline fired its incoming
        // hook once — the accepted hit, the weaker one the cooldown window
        // refused (the window is checked inside the pipeline) and the
        // flag-bypassing kill — while the flag-refused one never entered.
        assert_eq!(*heard.lock().unwrap(), vec![300001, 300001, 300001]);
    }

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn teams_allow_damage_without_a_team() {
        with_game(|game| {
            let zombie = game
                .res
                .mob_types()
                .get("minecraft:zombie")
                .unwrap()
                .clone();
            game.mobs
                .push(Mob::new(300001, zombie.clone(), Vec3::ZERO, 0.0));
            game.mobs.push(Mob::new(300002, zombie, Vec3::ZERO, 0.0));
            let a = game.entity(300001).unwrap();
            let b = game.entity(300002).unwrap();
            // No team on either side: damage is allowed, and a mob without
            // the flag answers the piercing query false.
            assert_eq!(a.entities_team_damage_allowed(game, b), Some(true));
            assert_eq!(a.invulnerable_to_piercing_weapon(game), Some(false));
            let stale = EntityRef::new(NativeWorldEpoch::new(a.epoch().get() + 1), 300001);
            assert_eq!(stale.entities_team_damage_allowed(game, b), None);
        });
    }

    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn a_mob_keeps_its_flag_and_damage_cooldown() {
        with_game(|game| {
            let zombie = game
                .res
                .mob_types()
                .get("minecraft:zombie")
                .unwrap()
                .clone();
            game.mobs.push(Mob::new(12, zombie, Vec3::ZERO, 0.0));
            let mob = game.entity(12).unwrap();
            assert_eq!(game.entity_permanently_invulnerable(mob), Some(false));
            assert!(game.entity_set_invulnerable(mob, true));
            assert_eq!(game.entity_permanently_invulnerable(mob), Some(true));
            assert!(game.mobs[0].invulnerable_flag);
            assert_eq!(game.entity_damage_cooldown_time(mob), Some(0));
            assert!(game.entity_set_damage_cooldown_time(mob, 20));
            assert_eq!(game.mobs[0].invulnerable, 20);
            assert_eq!(game.entity_damage_cooldown_time(mob), Some(20));
            let stale = EntityRef::new(NativeWorldEpoch::new(mob.epoch().get() + 1), 12);
            assert_eq!(game.entity_permanently_invulnerable(stale), None);
            assert!(!game.entity_set_invulnerable(stale, false));
            assert_eq!(game.entity_damage_cooldown_time(stale), None);
            assert!(!game.entity_set_damage_cooldown_time(stale, 0));
            assert!(game.mobs[0].invulnerable_flag);
        });
    }
}
