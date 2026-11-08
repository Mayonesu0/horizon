//! A server's players between dimensions.
//!
//! A server holds a level per dimension (game/world/levels.rs). A player who steps
//! into a Nether or End portal moves to the destination's level, which opens
//! if nobody was there: the portal plans and arrival search pick the exit,
//! and the player hears it as `ServerPlayer.teleport` into
//! another level: a Respawn packet keeping all data, then a teleport to the
//! exit once the destination has loaded around them.

use super::end::GatewayJob;
use super::nether_portal::{PLAYER_DELAY, Plan};
use super::respawn_anchor::RespawnConfig;
use super::respawn_position::Respawn;
use super::{Dim, Game, Mob, boats, lan::LanHost, minecarts};
use horizon_client::{lan::hub::TickCtx, world_spawn::PlayerSpawnFinder};
use horizon_core::{geometry::Vec3, position::BlockPos};
use horizon_protocol::packets::play::{self, Clientbound as C};

/// A trip under way: the player is in the destination's level, whose
/// chunks are loading around `target`.
pub(super) struct Trip {
    conn: u32,
    dim: Dim,
    /// The level the player came from: the vehicle it rides waits there
    /// until the exit is found.
    source: Dim,
    target: Vec3,
    /// Through an End portal.
    end: bool,
    /// The Nether portal plan, whose exit is found or built on arrival.
    plan: Option<Plan>,
    /// The vehicle the player rides, which travels with it
    /// (`Entity.teleportCrossDimension` carries the passengers).
    vehicle: Option<u32>,
    /// Through an End gateway, within the level: its exit, found or built
    /// on arrival.
    gateway: Option<GatewayJob>,
    /// A respawn at this point outside the Overworld, resolved through its
    /// block on arrival.
    respawn: Option<RespawnConfig>,
    /// A respawn at the world spawn, moved onto a free column within the
    /// respawn radius once the chunks around it have loaded.
    world_spawn: bool,
    /// The respawn point whose bed or anchor was gone, for a world-spawn
    /// respawn that replaces it (`TeleportTransition.missingRespawnBlock`).
    missing: Option<RespawnConfig>,
    /// The search for that column while a candidate's chunk loads.
    finder: Option<PlayerSpawnFinder>,
    /// Sent by a command: the player is already where it goes.
    placed: bool,
    /// The level the player left for another one: the mods are told they
    /// changed dimension when the trip ends (`tell_mods_changed_dimension`).
    pub(super) left: Option<Dim>,
}

/// A vehicle a player rides: an animal, a boat or a minecart. It travels
/// with the player through a portal (`Entity.teleportCrossDimension` carries
/// the passengers, and they ride the vehicle that is recreated in the other
/// dimension).
trait Rideable {
    fn id(&self) -> u32;
    /// The vehicle stands where its rider arrives, with the motion it crossed
    /// with (none for the kinds that keep none).
    fn arrive(&mut self, at: Vec3, yaw: f32, motion: Vec3);
}

impl Rideable for Mob {
    fn id(&self) -> u32 {
        self.id
    }
    fn arrive(&mut self, at: Vec3, yaw: f32, _motion: Vec3) {
        self.position = at;
        self.previous = at;
        self.velocity = Vec3::ZERO;
        self.yaw = yaw;
        self.previous_yaw = yaw;
        self.fall_distance = 0.0;
    }
}

impl Rideable for boats::Boat {
    fn id(&self) -> u32 {
        self.id
    }
    fn arrive(&mut self, at: Vec3, yaw: f32, _motion: Vec3) {
        self.position = at;
        self.previous = at;
        self.velocity = Vec3::ZERO;
        self.yaw = yaw;
        self.previous_yaw = yaw;
        self.delta_rotation = 0.0;
    }
}

impl Rideable for minecarts::Minecart {
    fn id(&self) -> u32 {
        self.id
    }
    fn arrive(&mut self, at: Vec3, yaw: f32, motion: Vec3) {
        self.position = at;
        self.previous = at;
        // `PositionMoveRotation.of(this)` takes the cart's `getKnownMovement`
        // (`OldMinecartBehavior` clamps it to 0.4 either way along x and z):
        // it rolls on at that speed instead of arriving at rest.
        self.velocity = Vec3::new(
            motion.x.clamp(-0.4, 0.4),
            motion.y,
            motion.z.clamp(-0.4, 0.4),
        );
        self.yaw = yaw;
        self.previous_yaw = yaw;
    }
}

/// The hub's view of the server for packets sent outside its own tick.
macro_rules! server_ctx {
    ($s:ident) => {{
        let spawn = $s.spawn.map_or([0.5, 80.0, 0.5], |s| [s.x, s.y, s.z]);
        let clock_rate = $s.network_clock_rate();
        TickCtx {
            world: &mut $s.world,
            entities: &[],
            next_entity: &mut $s.next_entity,
            spawn,
            difficulty: i32::from($s.ui.difficulty),
            difficulty_locked: $s.level.difficulty_locked,
            clock_rate,
            tick_rate: $s.clock.rate(),
            tick_frozen: $s.clock.frozen(),
            tick_steps: $s.clock.step_state(),
            collision: Some($s.collision),
            border: &$s.border,
            streaming: Some(&$s.streaming),
        }
    }};
}

/// The vehicle `id` leaves the level it is in, at the point its rider
/// arrives. It keeps its entity id, so the ride is never broken.
/// A guest leaving for another level leaves its fishing hook behind in the
/// old one, where vanilla's hook stays until its distance check ends it. A
/// guest's hook lives on the guest here, so it ends now: it must not act on
/// the blocks of the level the guest arrives in.
fn drop_hook(host: &mut LanHost, conn: u32) {
    if let Some(play) = host.play.get_mut(&conn) {
        play.hook = None;
    }
}

fn take_vehicle<
    T: Rideable
        + horizon_client::join_seq::Joined
        + horizon_mod_runtime::attachments::HasAttachmentLifetime,
    const DIRECT: bool,
>(
    list: &mut horizon_client::join_seq::JoinList<T, DIRECT>,
    id: u32,
    at: Vec3,
    yaw: f32,
    motion: Vec3,
) -> Option<T> {
    let i = list.iter().position(|v| v.id() == id)?;
    let mut v = list.remove_for_transfer(i);
    v.arrive(at, yaw, motion);
    v.set_join_seq(horizon_client::join_seq::JoinSeq::UNJOINED);
    Some(v)
}

/// `TeleportCommand` into another level for a server's player: where, and
/// the rotation it sets (none keeps the player's).
pub(super) struct LevelTeleport {
    pub conn: u32,
    pub dim: Dim,
    pub at: [f64; 3],
    pub rotation: Option<[f32; 2]>,
    /// `TeleportTransition.asPassenger`: the vehicle already moved with its tree.
    pub vehicle: Option<u32>,
}

/// A teleport a command made inside a level: told to the mods once it has
/// reached the hub (`native_player_teleported`).
pub(super) struct TeleportNote {
    pub conn: u32,
    pub uuid: u128,
    pub name: String,
    pub from: [f64; 3],
    pub to: [f64; 3],
    pub dimension: String,
}

/// A player's box: Player's standing dimensions.
const WIDTH: f64 = 0.6;
const HEIGHT: f64 = 1.8;

impl Trip {
    pub(super) fn is(&self, conn: u32) -> bool {
        self.conn == conn
    }
}

impl Game<'_> {
    pub(super) fn gateway_source_chunks(&self) -> Vec<(i32, i32)> {
        self.server_trips
            .iter()
            .filter(|t| t.dim == self.dim)
            .filter_map(|t| t.gateway.and_then(GatewayJob::source_chunk))
            .collect()
    }
    /// Whether `conn` is between two levels: the server leaves its body
    /// alone until it arrives.
    pub(super) fn on_server_trip(&self, conn: u32) -> bool {
        self.server_trips.iter().any(|t| t.conn == conn)
    }

    /// Whether a vehicle waits in `dim` for its rider to arrive elsewhere:
    /// that level stays open until it has moved.
    pub(super) fn vehicle_waits_in(&self, dim: Dim) -> bool {
        self.server_trips
            .iter()
            .any(|t| t.vehicle.is_some() && t.source == dim)
    }

    /// Per server tick in each level, after the guests' own: the arrivals of
    /// the trips into this level, then `Entity.handlePortal` for each player
    /// in it (Nether portals count up their transition time, End portals
    /// take them at once).
    pub(super) fn tick_server_travel(&mut self) {
        if self.lan.is_none() {
            return;
        }
        // Outside a server only the respawns at the world spawn wait for chunks.
        self.arrive_server_trips();
        if self.options.dedicated.is_none() {
            return;
        }
        self.carry_riders_through_portals();
        let Some(host) = self.lan.as_ref() else {
            return;
        };
        let conns: Vec<u32> = host
            .hub
            .guests
            .iter()
            .filter(|g| host.hub.here(g) && g.loaded && !g.dead && !g.spectator())
            .map(|g| g.conn)
            .collect();
        for conn in conns {
            if self.on_server_trip(conn) {
                continue;
            }
            // `Entity.portalProcess` and `portalCooldown` are the player's own.
            let Some(host) = self.lan.as_mut() else {
                return;
            };
            let mut portal = host.play.entry(conn).or_default().portal;
            std::mem::swap(&mut self.portals.player, &mut portal);
            self.handle_server_portal(conn);
            std::mem::swap(&mut self.portals.player, &mut portal);
            if let Some(host) = self.lan.as_mut() {
                host.play.entry(conn).or_default().portal = portal;
            }
        }
    }

    /// `ServerPlayerGameMode.useItemOn` for a spectator: a `Portal` block
    /// (Nether portal, End portal, End gateway) takes the player to its
    /// destination (`getPortalDestination`, `Entity.teleportToPortalDestination`)
    /// after the camera goes back to the player, or says `spectator.cannot_teleport`
    /// when it has none. Returns whether the block was a portal (`CONSUME`).
    pub(super) fn spectator_use_portal(&mut self, conn: u32, gi: usize, pos: [i32; 3]) -> bool {
        let name = self.block_name(pos);
        let name = name.trim_start_matches("minecraft:");
        if !matches!(name, "nether_portal" | "end_portal" | "end_gateway") {
            return false;
        }
        if self.on_server_trip(conn) {
            return true;
        }
        let Some(g) = self.lan.as_ref().map(|host| &host.hub.guests[gi]) else {
            return false;
        };
        let at = Vec3::new(g.position[0], g.position[1], g.position[2]);
        let seen_credits = g.seen_credits;
        // EndGatewayBlock.getPortalDestination: none without its block entity.
        let gateway_ok = name != "end_gateway"
            || self
                .world
                .block_entities
                .get(&(pos[0], pos[1], pos[2]))
                .is_some_and(|e| {
                    matches!(e, horizon_client::containers::BlockEntity::EndGateway(_))
                });
        if !gateway_ok {
            self.as_guest(conn, |g| {
                g.send_overlay_message(
                    horizon_client::text::Text::translate("spectator.cannot_teleport", &[])
                        .with_color(horizon_client::text::RED),
                );
            });
            return true;
        }
        if self.cmds.guest_cameras.contains_key(&conn) {
            self.guest_set_camera(gi, None);
        }
        match name {
            "nether_portal" => {
                let plan = self.portal_plan(
                    BlockPos::new(pos[0], pos[1], pos[2]),
                    at,
                    WIDTH as f32,
                    HEIGHT as f32,
                    true,
                );
                // Entity.teleportToPortalDestination: isAllowedToEnterPortal.
                if self.may_enter(plan.dim) {
                    // NetherPortalBlock.getExitPortal: a spectator never
                    // builds an exit portal, it gets no transition without one.
                    if !self.spectator_exit_exists(&plan) {
                        self.as_guest(conn, |g| {
                            g.send_overlay_message(
                                horizon_client::text::Text::translate(
                                    "spectator.cannot_teleport",
                                    &[],
                                )
                                .with_color(horizon_client::text::RED),
                            );
                        });
                        return true;
                    }
                    self.start_server_trip(conn, plan.dim, Some(plan), false);
                }
            }
            "end_portal" => {
                let from_end = self.dim == Dim::End;
                // Entity.canTeleport: out of the End only after the credits.
                if !from_end || seen_credits {
                    let target = if from_end { Dim::Overworld } else { Dim::End };
                    self.start_server_trip(conn, target, None, false);
                }
            }
            _ => self.start_gateway_trip(conn, pos),
        }
        true
    }

    /// Whether an exit portal already stands in the plan's dimension
    /// (`PortalForcer.findClosestPortalPosition`), searched without building.
    fn spectator_exit_exists(&mut self, plan: &Plan) -> bool {
        let source = self.dim;
        if self.enter_level(plan.dim).is_err() {
            return false;
        }
        let exists = self.find_exit_portal(plan).is_some();
        if let Err(e) = self.enter_level(source) {
            log::error!("could not reopen {}: {e}", source.id());
        }
        exists
    }

    /// Whether this guest's dimension change may start (`native_player_portal`).
    fn server_player_portal_allowed(&mut self, conn: u32, target: Dim, cause: &str) -> bool {
        let Some(g) = self.lan.as_ref().and_then(|host| host.hub.guest(conn)) else {
            return false;
        };
        let (player, name) = (g.uuid, g.name.clone());
        let (from, to) = (self.dim.id(), target.id());
        let mut cancel = false;
        self.native_player_portal(player, &name, from, to, cause, &mut cancel);
        !cancel
    }

    /// `Entity.handlePortal` for one player, whose portal state is in place.
    fn handle_server_portal(&mut self, conn: u32) {
        let Some(host) = self.lan.as_ref() else {
            return;
        };
        let Some(g) = host.hub.guest(conn) else {
            return;
        };
        let (at, invulnerable, sleeping, riding) = (
            Vec3::new(g.position[0], g.position[1], g.position[2]),
            g.creative(),
            g.sleeping,
            g.vehicle.is_some(),
        );
        // Entity.checkInsideBlocks: the Nether portal blocks the box is in.
        // `LivingEntity.canUsePortal` (#943): not a passenger — its vehicle
        // goes through and carries it (`carry_riders_through_portals`) — and
        // not asleep.
        if !riding
            && !sleeping
            && let Some(pos) = self.nether_portal_inside(at)
        {
            self.portals.player.set_inside(pos, PLAYER_DELAY);
        }
        // `ServerPlayer.showEndCredits`: the player stays out of the world while the
        // poem rolls, then PERFORM_RESPAWN takes it to its respawn point.
        let Some(host) = self.lan.as_ref() else {
            return;
        };
        if host.play.get(&conn).is_some_and(|play| play.won_game) {
            if host
                .play
                .get(&conn)
                .is_some_and(|play| play.respawn_requested)
            {
                if !self.server_player_portal_allowed(conn, Dim::Overworld, "end_portal") {
                    return;
                }
                let Some(host) = self.lan.as_mut() else {
                    return;
                };
                if let Some(play) = host.play.get_mut(&conn) {
                    play.respawn_requested = false;
                    play.won_game = false;
                }
                // PlayerList.respawn(keepAllPlayerData = true), CHANGED_DIMENSION:
                // PlayerEvent.Clone with `wasDeath` false.
                if let Some(g) = host.hub.guest(conn) {
                    let (uuid, name) = (g.uuid, g.name.clone());
                    self.native_player_clone(uuid, &name, false);
                }
                self.start_server_trip(conn, Dim::Overworld, None, true);
                self.portals.player.cooldown = 10;
            }
            return;
        }
        let (end_portal, gateway) = self.inside_end_portals(at, WIDTH, HEIGHT);
        if end_portal && !riding {
            // EndPortalBlock.entityInside: the first exit from the End shows the credits.
            let Some(host) = self.lan.as_mut() else {
                return;
            };
            if self.dim == Dim::End && !host.hub.guest(conn).is_some_and(|g| g.seen_credits) {
                let _ = host
                    .hub
                    .guests
                    .update_connection(conn, |g| g.seen_credits = true);
                host.play.entry(conn).or_default().won_game = true;
                // ClientboundGameEventPacket.WIN_GAME.
                host.hub.send_to(
                    conn,
                    &C::GameEvent(play::GameEvent {
                        event: 4,
                        param: 0.0,
                    }),
                );
                return;
            }
            if self.portals.player.cooldown == 0 {
                let target = if self.dim == Dim::End {
                    Dim::Overworld
                } else {
                    Dim::End
                };
                if !self.server_player_portal_allowed(conn, target, "end_portal") {
                    return;
                }
                self.start_server_trip(conn, target, None, false);
            }
            // Entity.setAsInsidePortal while cooling down restarts the cooldown.
            self.portals.player.cooldown = 10;
            return;
        }
        // EndGatewayBlock.entityInside: a gateway that is not cooling down
        // takes the player at once (no transition time) and cools down
        // (triggerCooldown), even for a player still on its own cooldown.
        if !riding
            && let Some(g) = gateway
            && self.gateway_available(g)
        {
            if self.portals.player.cooldown == 0 {
                let Some(player) = self.lan.as_ref().and_then(|host| host.hub.guest(conn)) else {
                    return;
                };
                let (id, name) = (player.uuid, player.name.clone());
                let mut cancel = false;
                self.native_player_teleport_end_gateway(id, &name, g, &mut cancel);
                if cancel {
                    return;
                }
                if !self.server_player_portal_allowed(conn, self.dim, "end_gateway") {
                    return;
                }
            }
            if self.gateway_enter(g) {
                if self.portals.player.cooldown == 0 {
                    self.start_gateway_trip(conn, g);
                }
                // setPortalCooldown: after the trip, or again while cooling down.
                self.portals.player.cooldown = 10;
                return;
            }
        }
        let rule = if invulnerable {
            horizon_client::gamerules::vanilla::PLAYERS_NETHER_PORTAL_CREATIVE_DELAY
        } else {
            horizon_client::gamerules::vanilla::PLAYERS_NETHER_PORTAL_DEFAULT_DELAY
        };
        let transition = self.ui.rules.int_at(rule).max(0);
        let Some(entry) = self
            .portals
            .player
            .handle(!sleeping, transition, PLAYER_DELAY)
        else {
            return;
        };
        let plan = self.portal_plan(entry, at, WIDTH as f32, HEIGHT as f32, true);
        if !self.may_enter(plan.dim) {
            return;
        }
        if !self.server_player_portal_allowed(conn, plan.dim, "nether_portal") {
            // The processor already reached its threshold. Forget this attempt
            // so a canceled trip must complete a fresh portal wait before retrying.
            self.portals.player.process = None;
            return;
        }
        self.start_server_trip(conn, plan.dim, Some(plan), false);
    }

    /// `Entity.handlePortal` of a vehicle server players ride, once through
    /// a portal (a vehicle's transition time is 0): the riders travel with
    /// it and ride it again on arrival (`Entity.teleportCrossDimension`), so
    /// a rider never waits for its own portal delay.
    fn carry_riders_through_portals(&mut self) {
        for (vehicle, entry) in self.take_carrying(self.dim) {
            let Some(plan) = self.vehicle_plan(vehicle, entry) else {
                continue;
            };
            if !self.may_enter(plan.dim) {
                continue;
            }
            let Some(host) = self.lan.as_ref() else {
                return;
            };
            let riders: Vec<u32> = host
                .hub
                .guests
                .iter()
                .filter(|g| g.vehicle == Some(vehicle) && !self.on_server_trip(g.conn))
                .map(|g| g.conn)
                .collect();
            // Entity.canTeleport: out of the End, every rider has seen the
            // credits.
            if plan.end
                && self.dim == Dim::End
                && riders
                    .iter()
                    .any(|c| !host.hub.guest(*c).is_some_and(|g| g.seen_credits))
            {
                continue;
            }
            // An End portal's exit is found as a player's (its own respawn
            // point out of the End).
            let (target, plan) = (plan.dim, (!plan.end).then_some(plan));
            for conn in riders {
                self.start_server_trip(conn, target, plan, false);
            }
        }
    }

    /// A Nether portal block a player's box is in.
    fn nether_portal_inside(&self, at: Vec3) -> Option<BlockPos> {
        let w = WIDTH / 2.0 - 1.0e-5;
        let lo = [at.x - w, at.y + 1.0e-5, at.z - w];
        let hi = [at.x + w, at.y + HEIGHT - 1.0e-5, at.z + w];
        for x in lo[0].floor() as i32..=hi[0].floor() as i32 {
            for y in lo[1].floor() as i32..=hi[1].floor() as i32 {
                for z in lo[2].floor() as i32..=hi[2].floor() as i32 {
                    if &*self.block_name([x, y, z]) == "minecraft:nether_portal" {
                        return Some(BlockPos::new(x, y, z));
                    }
                }
            }
        }
        None
    }

    /// The player moves to `target`'s level (opened if nobody was there)
    /// and is told it is there, at the approximate exit, while it loads. The
    /// level whose tick is running is put back in place afterwards.
    pub(super) fn start_server_trip(
        &mut self,
        conn: u32,
        target: Dim,
        plan: Option<Plan>,
        credits_respawn: bool,
    ) {
        let Some(host) = self.lan.as_mut() else {
            return;
        };
        let Some(i) = host.hub.guests.iter().position(|g| g.conn == conn) else {
            return;
        };
        if target != self.dim {
            drop_hook(host, conn);
        }
        let g = &host.hub.guests[i];
        let from = Vec3::new(g.position[0], g.position[1], g.position[2]);
        let yaw = g.yaw;
        // The vehicle the player rides travels with it: the player is its
        // passenger, and the vehicle is recreated in the other dimension,
        // where they ride it again (`Entity.teleportCrossDimension`). It
        // keeps its entity id, so the ride is never broken.
        let vehicle = g.vehicle.filter(|id| {
            self.mobs.iter().any(|m| m.id == *id)
                || self.boats.iter().any(|b| b.id == *id)
                || self.carts.iter().any(|c| c.id == *id)
        });
        let source = self.dim;
        let end = plan.is_none();
        // Coordinates scale by 8 between the Overworld and the Nether; an
        // End portal has fixed destinations.
        let scale = match (source, target) {
            (Dim::Overworld, Dim::Nether) | (Dim::End, Dim::Nether) => 1.0 / 8.0,
            (Dim::Nether, Dim::Overworld) => 8.0,
            _ => 1.0,
        };
        self.end_portal_travel = end;
        // Leaving the End: the player's own respawn point.
        let own = if end {
            self.as_guest(conn, |g| g.end_destination(target)).flatten()
        } else {
            None
        };
        let scaled = own
            .or_else(|| self.end_destination(target))
            .unwrap_or(Vec3::new(from.x * scale, from.y, from.z * scale));
        self.end_portal_travel = false;
        if let Err(e) = self.enter_level(target) {
            log::error!("could not open {}: {e}", target.id());
            return;
        }
        let destination = match plan {
            Some(plan) => self.arrival_centre(&plan),
            None => scaled,
        };
        let at = [destination.x, destination.y, destination.z];
        let ctx = server_ctx!(self);
        // Through a portal: the client waits on its loading screen until
        // the exit is found (`arrive_server_trips`). The cooldown is the one
        // Entity.handlePortal set before the teleport.
        if let Some(host) = self.lan.as_mut() {
            host.hub
                .change_level(i, at, yaw, None, true, PLAYER_DELAY, &ctx);
        }
        if credits_respawn && let Some(host) = self.lan.as_mut() {
            host.hub.guests.update(i, |g| {
                g.load_timeout = horizon_client::lan::guest::CLIENT_LOADED_TIMEOUT;
                g.loading = true;
            });
        }
        self.bind_player_attachments(conn, false);
        self.as_guest(conn, |g| {
            g.trigger_dimension(source, target, [from.x, from.y, from.z])
        });
        self.server_trips.push(Trip {
            conn,
            dim: target,
            source,
            target: destination,
            end,
            plan,
            vehicle,
            gateway: None,
            respawn: None,
            world_spawn: false,
            missing: None,
            finder: None,
            placed: false,
            left: Some(source),
        });
        if let Err(e) = self.enter_level(source) {
            log::error!("could not reopen {}: {e}", source.id());
        }
    }

    /// A respawned player waits at its point outside the Overworld for the
    /// level to load around it.
    pub(super) fn respawn_trip(&mut self, conn: u32, config: RespawnConfig) {
        let [x, y, z] = config.pos;
        self.server_trips.push(Trip {
            conn,
            dim: config.dim,
            source: self.dim,
            target: Vec3::new(f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5),
            end: false,
            plan: None,
            vehicle: None,
            gateway: None,
            respawn: Some(config),
            world_spawn: false,
            missing: None,
            finder: None,
            placed: false,
            left: None,
        });
    }

    /// `findRespawnAndUseSpawnBlock` for the player in place, at its point
    /// in the level in place: where it stands up, facing the anchor (with
    /// the deplete sound) or the bed; else the point is forgotten, and the
    /// caller tells the player (`Hub::missing_respawn_block`).
    pub(super) fn resolve_server_respawn(&mut self, config: RespawnConfig) -> Option<(Vec3, f32)> {
        let [x, y, z] = config.pos;
        let anchor = self.is_anchor(self.world.block(x, y, z));
        match self.find_respawn_and_use(config, true) {
            Ok(Some((at, yaw))) => {
                if !config.forced && anchor {
                    let [x, y, z] = config.pos.map(f64::from);
                    self.audio.play(
                        "minecraft:block.respawn_anchor.deplete",
                        Some([x, y, z]),
                        1.0,
                        1.0,
                    );
                }
                // PlayerList.respawn: the block was found, so the new player
                // copies the respawn point (`copyRespawnPosition`). A cancel
                // leaves it with none.
                if self.spawn_point_change(config.dim, Some(config.pos), config.forced) {
                    self.respawn_config = None;
                }
                Some((at, yaw))
            }
            Ok(None) | Err(_) => {
                // The block is gone: `PlayerList.respawn` skips
                // `copyRespawnPosition`, so the new player has no point and
                // no `setRespawnPosition` call is made.
                self.respawn_config = None;
                None
            }
        }
    }

    /// TeleportTransition.createDefault and missingRespawnBlock: the player
    /// waits at the world spawn for its chunks, then PlayerSpawnFinder puts
    /// it on a free column within the respawn radius.
    pub(super) fn world_spawn_trip(&mut self, conn: u32, missing: Option<RespawnConfig>) {
        let Some([x, y, z]) = self.level.world_spawn else {
            return;
        };
        self.server_trips.retain(|t| !t.is(conn));
        self.server_trips.push(Trip {
            conn,
            dim: Dim::Overworld,
            source: self.dim,
            target: Vec3::new(f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5),
            end: false,
            plan: None,
            vehicle: None,
            gateway: None,
            respawn: None,
            world_spawn: true,
            missing,
            finder: None,
            placed: false,
            left: None,
        });
    }

    /// TeleportTransition.missingRespawnBlock: the world spawn, in the
    /// Overworld.
    fn respawn_at_world_spawn(&mut self, conn: u32, missing: RespawnConfig) {
        let at = self.level.world_spawn.map_or(
            self.spawn.map_or([0.5, 80.0, 0.5], |s| [s.x, s.y, s.z]),
            |[x, y, z]| [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5],
        );
        self.level_teleports.push(LevelTeleport {
            conn,
            dim: Dim::Overworld,
            at,
            rotation: None,
            vehicle: None,
        });
        self.run_level_teleports();
        self.world_spawn_trip(conn, Some(missing));
    }

    /// `TheEndGatewayBlockEntity.getPortalPosition` and the teleport within
    /// the End (keeping the player's motion and look): straight to an exact
    /// exit, else once the destination has loaded around the player, who
    /// waits there (its exit is found among those blocks, or built).
    fn start_gateway_trip(&mut self, conn: u32, gateway: [i32; 3]) {
        let Some(i) = self
            .lan
            .as_ref()
            .and_then(|host| host.hub.guests.iter().position(|g| g.conn == conn))
        else {
            return;
        };
        let (job, destination) = self.gateway_job(gateway);
        let at = [destination.x, destination.y, destination.z];
        // Relative.union(DELTA, ROTATION).
        if let Some(host) = self.lan.as_mut() {
            host.hub.teleport_keeping_motion(i, at);
        }
        if let GatewayJob::Known { exact: true, .. } = job {
            return;
        }
        self.server_trips.push(Trip {
            conn,
            dim: self.dim,
            source: self.dim,
            target: destination,
            end: false,
            plan: None,
            vehicle: None,
            gateway: Some(job),
            respawn: None,
            world_spawn: false,
            missing: None,
            finder: None,
            placed: false,
            left: None,
        });
    }

    /// The trips into the level in place whose destination has loaded
    /// around their target: the exit (the Nether portal found or built, the
    /// End platform, the Overworld's spawn column) and the teleport there.
    fn arrive_server_trips(&mut self) {
        let mut k = 0;
        while k < self.server_trips.len() {
            let trip = &self.server_trips[k];
            let Some(host) = self.lan.as_ref() else {
                return;
            };
            let Some(i) = host.hub.guests.iter().position(|g| g.conn == trip.conn) else {
                // They left on the way.
                self.server_trips.remove(k);
                continue;
            };
            let (cx, cz) = (
                (trip.target.x.floor() as i32) >> 4,
                (trip.target.z.floor() as i32) >> 4,
            );
            if trip.dim != self.dim
                || !(-2..=2).all(|dz| (-2..=2).all(|dx| self.streaming.is_lit(cx + dx, cz + dz)))
                || trip
                    .gateway
                    .and_then(GatewayJob::source_chunk)
                    .is_some_and(|c| !self.world.chunks.contains_key(&c))
            {
                k += 1;
                continue;
            }
            let mut trip = self.server_trips.remove(k);
            if trip.placed {
                self.tell_mods_changed_dimension(trip.conn, trip.left, trip.dim);
                continue;
            }
            // A portal's trip (`start_server_trip`): its client waits to land.
            let portal = trip.end || trip.plan.is_some();
            self.resolve_arrivals();
            let Some(host) = self.lan.as_ref() else {
                return;
            };
            let yaw = host.hub.guests[i].yaw;
            let (position, yaw) = if let Some(config) = trip.respawn {
                let found = self.as_guest(trip.conn, |g| g.resolve_server_respawn(config));
                match found {
                    Some(Some(found)) => found,
                    _ => {
                        if let Some(host) = self.lan.as_mut() {
                            host.hub.missing_respawn_block(trip.conn);
                        }
                        self.respawn_at_world_spawn(trip.conn, config);
                        // The move to the world spawn ends the player's move
                        // from the level they left.
                        if let Some(left) = trip.left
                            && let Some(t) = self.server_trips.iter_mut().find(|t| t.is(trip.conn))
                        {
                            t.left = Some(left);
                        }
                        continue;
                    }
                }
            } else if trip.world_spawn {
                // PlayerSpawnFinder.findSpawn around the world spawn.
                let suggestion =
                    [trip.target.x, trip.target.y, trip.target.z].map(|v| v.floor() as i32);
                match self.search_spawn(suggestion, &mut trip.finder) {
                    // The respawn already put the player on this column:
                    // vanilla teleports once, so no second move.
                    Ok(at)
                        if self.lan.as_ref().is_some_and(|host| {
                            host.hub.guests[i].position == [at.x, at.y, at.z]
                        }) =>
                    {
                        self.tell_mods_changed_dimension(trip.conn, trip.left, trip.dim);
                        // PlayerRespawnPositionEvent: the column is the point.
                        let pitch = self.lan.as_ref().map_or(0.0, |h| h.hub.guests[i].pitch);
                        let found = Respawn {
                            dim: trip.dim,
                            at,
                            yaw,
                            pitch,
                        };
                        if let Some(d) = self.respawn_position(trip.conn, found, trip.missing) {
                            self.apply_respawn_copy(trip.conn, &d, trip.missing);
                            if d.moved && d.to.dim != trip.dim {
                                self.respawn_into_other_level(trip.conn, d.to);
                            } else if d.moved
                                && let Some(host) = self.lan.as_mut()
                            {
                                host.hub.guests.update(i, |g| {
                                    (g.yaw, g.pitch) = (d.to.yaw, d.to.pitch);
                                    g.new_body(true);
                                });
                                host.hub
                                    .teleport(i, Some([d.to.at.x, d.to.at.y, d.to.at.z]));
                            }
                        }
                        continue;
                    }
                    Ok(at) => (at, yaw),
                    Err(chunk) => {
                        self.spawn_wait.push(chunk);
                        self.server_trips.insert(k, trip);
                        k += 1;
                        continue;
                    }
                }
            } else if let Some(job) = trip.gateway {
                match self.resolve_gateway(job, trip.target) {
                    Ok(at) => (at, yaw),
                    Err(e) => {
                        log::error!("End gateway exit: {e}");
                        (trip.target, yaw)
                    }
                }
            } else if trip.end {
                let guest_arrival =
                    self.as_guest(trip.conn, |g| g.end_arrival(trip.target, &mut trip.finder));
                let arrival = match guest_arrival {
                    Some(arrival) => arrival,
                    None => self.end_arrival(trip.target, &mut trip.finder),
                };
                match arrival {
                    Ok(Some((at, turned))) => {
                        // EndPortalBlock.getPortalDestination: PLACE_PORTAL_TICKET.
                        self.place_portal_ticket(Self::chunk_of(at));
                        (at, turned.unwrap_or(yaw))
                    }
                    Ok(None) => {
                        self.server_trips.insert(k, trip);
                        k += 1;
                        continue;
                    }
                    Err(e) => {
                        log::error!("End arrival: {e}");
                        (trip.target, yaw)
                    }
                }
            } else {
                let exit = trip
                    .plan
                    .as_ref()
                    .and_then(|plan| self.nether_exit_portal(plan));
                match (trip.plan, exit) {
                    (Some(plan), Some(exit)) => {
                        let (at, rotation) = self.arrival(&plan, exit);
                        (at, yaw + rotation)
                    }
                    _ => (trip.target, yaw),
                }
            };
            // PlayerRespawnPositionEvent: a deferred respawn's point is found.
            let mut pitch = None;
            let (position, yaw) = if trip.respawn.is_some() || trip.world_spawn {
                let pitch_now = self.lan.as_ref().map_or(0.0, |h| h.hub.guests[i].pitch);
                let found = Respawn {
                    dim: trip.dim,
                    at: position,
                    yaw,
                    pitch: pitch_now,
                };
                match self.respawn_position(trip.conn, found, trip.missing) {
                    Some(d) => {
                        self.apply_respawn_copy(trip.conn, &d, trip.missing);
                        if d.to.dim != trip.dim {
                            self.tell_mods_changed_dimension(trip.conn, trip.left, trip.dim);
                            self.respawn_into_other_level(trip.conn, d.to);
                            continue;
                        }
                        pitch = Some(d.to.pitch);
                        (d.to.at, d.to.yaw)
                    }
                    None => (position, yaw),
                }
            } else {
                (position, yaw)
            };
            // The vehicle the player rode comes with it, and waits for it
            // where it arrives (`Entity.teleportCrossDimension` carries the
            // passengers).
            if let Some(vehicle) = trip.vehicle {
                self.arrive_vehicle(trip.source, vehicle, position, yaw);
            }
            // Player.getDimensionChangingDelay.
            let Some(host) = self.lan.as_mut() else {
                return;
            };
            host.play.entry(trip.conn).or_default().portal.cooldown = 10;
            host.hub.guests.update(i, |g| {
                g.yaw = yaw;
                if let Some(pitch) = pitch {
                    g.pitch = pitch;
                }
                // A new body at the exit: no fall distance or motion carried over,
                // but the player's attributes (ServerPlayer.teleport keeps the entity).
                g.new_body(true);
            });
            let at = [position.x, position.y, position.z];
            if portal {
                host.hub.land(i, at);
            } else {
                host.hub.teleport(i, Some(at));
            }
            self.tell_mods_changed_dimension(trip.conn, trip.left, trip.dim);
        }
    }

    /// The vehicle the player rode arrives with it, at the point the player
    /// arrives: it keeps its entity id, so the ride is never broken
    /// (`Entity.teleportCrossDimension` recreates the vehicle in the other
    /// dimension, and its passengers ride it again).
    fn arrive_vehicle(&mut self, source: Dim, id: u32, at: Vec3, yaw: f32) {
        let motion = self
            .portals
            .carried_motion
            .remove(&id)
            .unwrap_or(Vec3::ZERO);
        let Some(parked) = self.levels.parked_mut(source) else {
            return;
        };
        if let Some(mut b) = take_vehicle(&mut parked.boats, id, at, yaw, motion) {
            self.rebind_transferred_entity(source, id, &mut b);
            self.boats.push(b);
        } else if let Some(mut c) = take_vehicle(&mut parked.carts, id, at, yaw, motion) {
            self.rebind_transferred_entity(source, id, &mut c);
            self.carts.push(c);
        } else if let Some(mut m) = take_vehicle(&mut parked.mobs, id, at, yaw, motion) {
            self.rebind_transferred_entity(source, id, &mut m);
            self.mobs.push(m);
        } else {
            return;
        }
        // Its memory (its UUID and the compound it was loaded with) moves
        // with it, so it is saved in this level as it was in the other.
        self.move_entity_memory(source, id);
        // The vehicle that came through a portal does not go back through it
        // for a while (`Entity.setPortalCooldown`).
        self.set_portal_cooldown(id);
    }

    /// Tells the mods of the teleports a command made inside a level, with
    /// each player's state swapped in: where the move has reached the hub.
    pub(super) fn flush_teleport_notes(&mut self) {
        for note in std::mem::take(&mut self.teleport_notes) {
            let TeleportNote {
                conn,
                uuid,
                name,
                from,
                to,
                dimension,
            } = note;
            // In place when it is their turn already (as_guest finds them swapped in).
            if self.lan_acting == Some(conn) {
                self.native_player_teleported(uuid, &name, from, to, &dimension);
            } else {
                self.as_guest(conn, |g| {
                    g.native_player_teleported(uuid, &name, from, to, &dimension);
                });
            }
        }
    }

    /// The players the command just run sends to another level
    /// (`ServerPlayer.teleport` from TeleportCommand): a Respawn packet
    /// keeping all data and the teleport, with no portal sound. The server
    /// leaves the body alone until the level has loaded around it.
    pub(super) fn run_level_teleports(&mut self) {
        if self.lan.is_none() {
            return;
        }
        for t in std::mem::take(&mut self.level_teleports) {
            let Some(host) = self.lan.as_mut() else {
                return;
            };
            let Some(i) = host.hub.guests.iter().position(|g| g.conn == t.conn) else {
                continue;
            };
            let source = self.dim;
            if t.dim != source {
                drop_hook(host, t.conn);
            }
            if let Err(e) = self.enter_level(t.dim) {
                log::error!("could not open {}: {e}", t.dim.id());
                continue;
            }
            let Some(host) = self.lan.as_mut() else {
                return;
            };
            let (uuid, name) = (host.hub.guests[i].uuid, host.hub.guests[i].name.clone());
            let (yaw, pitch, origin, passenger_body) = host.hub.guests.update(i, |g| {
                let [yaw, pitch] = t.rotation.unwrap_or([g.yaw, g.pitch]);
                let origin = g.position;
                // A new body there: no fall distance or motion carried over, but
                // the player's attributes (ServerPlayer.teleport keeps the entity).
                let passenger_body = if t.vehicle.is_some() {
                    g.motion.take()
                } else {
                    g.new_body(true);
                    None
                };
                (yaw, pitch, origin, passenger_body)
            });
            let cooldown = host.play.get(&t.conn).map_or(0, |p| p.portal.cooldown);
            let ctx = server_ctx!(self);
            if let Some(host) = self.lan.as_mut() {
                host.hub
                    .change_level(i, t.at, yaw, Some(pitch), false, cooldown, &ctx);
                host.hub.guests.update(i, |g| {
                    g.vehicle = t.vehicle;
                    if let Some(mut body) = passenger_body {
                        body.position = Vec3::new(t.at[0], t.at[1], t.at[2]);
                        g.motion = Some(body);
                    }
                });
            }
            self.bind_player_attachments(t.conn, false);
            self.as_guest(t.conn, |g| {
                g.trigger_dimension(source, t.dim, origin);
                // The mods hear the teleport into another level, with the
                // player's state swapped in.
                g.native_player_teleported(uuid, &name, origin, t.at, t.dim.id());
            });
            self.server_trips.retain(|trip| !trip.is(t.conn));
            self.server_trips.push(Trip {
                conn: t.conn,
                dim: t.dim,
                source,
                target: Vec3::new(t.at[0], t.at[1], t.at[2]),
                end: false,
                plan: None,
                // A command teleports the player alone: it is not a
                // passenger any more (`ServerPlayer.teleport`).
                vehicle: None,
                gateway: None,
                respawn: None,
                world_spawn: false,
                missing: None,
                finder: None,
                placed: true,
                left: Some(source),
            });
            if let Err(e) = self.enter_level(source) {
                log::error!("could not reopen {}: {e}", source.id());
            }
        }
    }

    /// `native_player_changed_dimension` for a guest who has ended a move
    /// from the level `left` to the one in place, `to`, with their state
    /// swapped in so that the helpers reach them. Nothing is heard when they
    /// did not leave a level, or ended where they started.
    pub(super) fn tell_mods_changed_dimension(&mut self, conn: u32, left: Option<Dim>, to: Dim) {
        let Some(from) = left.filter(|from| *from != to) else {
            return;
        };
        let Some((uuid, name)) = self
            .lan
            .as_ref()
            .and_then(|host| host.hub.guest(conn))
            .map(|g| (g.uuid, g.name.clone()))
        else {
            return;
        };
        self.as_guest(conn, |g| {
            g.native_player_changed_dimension(uuid, &name, from.id(), to.id());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mod_api::ServerHost;
    use std::sync::Arc;

    #[test]
    #[ignore = "needs prepared resources"]
    fn arriving_vehicle_is_announced_once_after_destination_entities() {
        use mod_api::{self, EntityJoinLevelEvent, Flow, Hooks};
        use std::sync::Mutex;
        struct Observer(Arc<Mutex<Vec<u32>>>);
        #[mod_api::__subscriptions]
        impl Hooks for Observer {
            fn entity_join_level(
                &self,
                _: &mut dyn ServerHost,
                event: &mut EntityJoinLevelEvent<'_>,
            ) -> Flow {
                self.0.lock().unwrap().push(event.entity);
                Flow::Continue
            }
        }
        let joins = Arc::new(Mutex::new(Vec::new()));
        let mods: mod_api::Mods = vec![mod_api::ModEntry::new(
            "arrival-observer",
            Box::new(Observer(joins.clone())),
        )]
        .into();
        mod_api::testing::with_mods(mods, || {
            super::super::native_entity_refs::tests::with_game(|game| {
                game.native_joins_skip();
                let base = game.next_entity;
                game.next_entity += 2;
                game.carts.push(minecarts::Minecart::new(
                    base + 1,
                    minecarts::CartKind::Rideable,
                    Vec3::ZERO,
                ));
                let source = game.dim;
                game.enter_level(Dim::Nether).unwrap();
                game.carts.push(minecarts::Minecart::new(
                    base + 2,
                    minecarts::CartKind::Rideable,
                    Vec3::ZERO,
                ));
                game.native_joins_flush(false, 0);
                game.arrive_vehicle(source, base + 1, Vec3::ZERO, 0.0);
                game.native_joins_flush(false, 0);
                game.native_joins_flush(false, 0);
                assert_eq!(*joins.lock().unwrap(), vec![base + 2, base + 1]);
            });
        });
    }

    /// A minecart a player rides through a portal rolls on at the motion it
    /// crossed with, clamped to 0.4 along x and z (`PositionMoveRotation.of`
    /// takes `OldMinecartBehavior.getKnownMovement`), not at rest. Vanilla
    /// 26.3 sent the client a cart arriving with motion (0.4, 0, 0).
    #[test]
    fn a_minecart_a_player_rides_keeps_its_clamped_motion_through_a_portal() {
        let mut carts: horizon_client::join_seq::JoinList<_> = [minecarts::Minecart::new(
            3,
            minecarts::CartKind::Rideable,
            Vec3::new(9.9, 100.0, 4.5),
        )]
        .into_iter()
        .collect();
        let at = Vec3::new(1.5, 100.0, 0.5);
        let cart = take_vehicle(&mut carts, 3, at, 0.0, Vec3::new(1.6, -0.1, -0.7))
            .expect("the cart travels");
        assert_eq!(cart.position, at);
        assert_eq!(cart.velocity, Vec3::new(0.4, -0.1, -0.4));
    }

    /// The vehicle a player rides travels with it through a portal: it
    /// leaves the level the player came from, keeps its entity id (so the
    /// ride is never broken) and stands where its rider arrives
    /// (`Entity.teleportCrossDimension` carries the passengers, and they
    /// ride the vehicle that is recreated in the other dimension).
    #[test]
    fn the_vehicle_a_player_rides_travels_with_it() {
        let mut boats: horizon_client::join_seq::JoinList<_> = [boats::Boat::new(
            7,
            Arc::from("oak"),
            Vec3::new(0.0, 64.0, 0.0),
            0.0,
        )]
        .into_iter()
        .collect();
        let mut carts = horizon_client::join_seq::JoinList::<minecarts::Minecart>::default();
        let mut mobs = horizon_client::join_seq::JoinList::<Mob>::default();
        let at = Vec3::new(1.5, 65.0, -2.5);
        let boat = take_vehicle(&mut boats, 7, at, 90.0, Vec3::ZERO)
            .expect("the vehicle the player rides travels with it");
        assert!(
            boats.is_empty(),
            "the vehicle left the level the player came from"
        );
        assert_eq!(boat.id, 7, "the vehicle keeps its entity id");
        assert_eq!(boat.position, at, "it stands where its rider arrives");
        assert_eq!(boat.yaw, 90.0, "it looks where its rider looks");
        assert_eq!(boat.velocity, Vec3::ZERO, "it comes with no motion");
        // A vehicle that is not in the level does not travel.
        assert!(take_vehicle(&mut carts, 9, at, 0.0, Vec3::ZERO).is_none());
        assert!(carts.is_empty());
        assert!(take_vehicle(&mut mobs, 9, at, 0.0, Vec3::ZERO).is_none());
        assert!(mobs.is_empty());
    }
}

#[cfg(test)]
mod transfer_lifetime_tests {
    use super::*;
    use horizon_client::join_seq::Joined;
    use horizon_mod_runtime::attachments::{
        AttachmentLifetime, AttachmentType, Attachments, HasAttachmentLifetime, Holder,
        index::AttachmentIndex,
    };
    use std::sync::{Arc, Mutex};

    static VALUE: AttachmentType<i32> = AttachmentType {
        namespace: "test",
        name: "transfer",
        default: || 0,
        codec: None,
        copy_on_death: false,
        sync: false,
    };

    fn removal<T: Rideable + horizon_client::join_seq::Joined + HasAttachmentLifetime>(
        mut owner: T,
    ) {
        let index = Arc::new(Mutex::new(AttachmentIndex::default()));
        let mut map = Attachments::default();
        VALUE.set(&mut map, Holder::Entity(owner.id()), 17).unwrap();
        owner.attachment_lifetime().bind(&mut map, &index, 42u32);
        let mut list: horizon_client::join_seq::JoinList<T> = [owner].into_iter().collect();
        let id = list[0].id();
        let key = ("test".into(), "transfer".into());
        let retained = index.lock().unwrap().snapshot(&key);
        let moved = take_vehicle(&mut list, id, Vec3::ZERO, 0.0, Vec3::ZERO).unwrap();
        assert!(list.is_empty());
        assert!(index.lock().unwrap().snapshot(&key).is_empty());
        VALUE.set(&mut map, Holder::Entity(id), 18).unwrap();
        assert!(index.lock().unwrap().snapshot(&key).is_empty());
        assert_eq!(retained[0].entry.read(&VALUE), Ok(&17));
        drop(moved);
    }

    #[test]
    fn transfer_removal_retires_before_returning_the_vehicle() {
        removal(minecarts::Minecart::new(
            71,
            minecarts::CartKind::Rideable,
            Vec3::ZERO,
        ));
        removal(boats::Boat::new(72, Arc::from("oak"), Vec3::ZERO, 0.0));
    }

    #[test]
    #[ignore = "needs prepared resources"]
    fn transfer_arrival_rebinds_only_bound_owners_with_the_same_id() {
        for kind in ["minecraft:minecart", "minecraft:oak_boat", "minecraft:pig"] {
            for bound in [true, false] {
                super::super::native_entity_refs::tests::with_game(|game| {
                    game.tickets.update(None);
                    let target = game
                        .summon_with_nbt(kind, [0.5, 70.0, 0.5], "{}")
                        .unwrap()
                        .unwrap();
                    let id = match target {
                        super::super::commands::Target::Cart(id)
                        | super::super::commands::Target::Boat(id)
                        | super::super::commands::Target::Mob(id) => id,
                        _ => panic!("unexpected vehicle target"),
                    };
                    let old_join = match kind {
                        "minecraft:minecart" => game.carts[0].join_seq(),
                        "minecraft:oak_boat" => game.boats[0].join_seq(),
                        _ => game.mobs[0].join_seq(),
                    };
                    assert!(old_join.is_joined());
                    let uuid = game.uuid_for(id);
                    let source = game.dim;
                    let source_ref = game.entity(id).unwrap();
                    let source_index = game.attachment_indexes.entry(source).or_default().clone();
                    let mut map = Attachments::default();
                    VALUE.set(&mut map, Holder::Entity(id), 17).unwrap();
                    let old = if bound {
                        Some(map.bind_index(
                            &source_index,
                            mod_api::AttachmentHolder::Entity(source_ref),
                        ))
                    } else {
                        None
                    };
                    macro_rules! slot {
                        ($list:ident) => {
                            game.$list.update(0, |entity| {
                                if let Some(old) = &old {
                                    *entity.attachment_lifetime() =
                                        AttachmentLifetime::new(old.clone());
                                }
                            })
                        };
                    }
                    match kind {
                        "minecraft:minecart" => slot!(carts),
                        "minecraft:oak_boat" => slot!(boats),
                        _ => slot!(mobs),
                    }
                    let key = ("test".into(), "transfer".into());
                    let retained = source_index.lock().unwrap().snapshot(&key);
                    let mut stale_map = map.clone();
                    game.entity_attachments.insert(uuid, map);
                    game.enter_level(Dim::Nether).unwrap();
                    let destination_ref =
                        mod_api::EntityRef::new(game.level(game.dim.id()).unwrap().epoch(), id);
                    assert_eq!(source_ref, destination_ref);
                    game.arrive_vehicle(source, id, Vec3::ZERO, 0.0);
                    macro_rules! transferred {
                        ($list:ident) => {{
                            let parked = game.levels.parked_mut(source).unwrap();
                            assert!(
                                !parked
                                    .$list
                                    .iter()
                                    .any(|e| e.id == id || e.join_seq() == old_join)
                            );
                            let arrived = game.$list.iter().find(|e| e.id == id).unwrap();
                            assert!(arrived.join_seq().is_joined());
                            assert_ne!(arrived.join_seq(), old_join);
                            assert!(!game.$list.iter().any(|e| e.join_seq() == old_join));
                        }};
                    }
                    match kind {
                        "minecraft:minecart" => transferred!(carts),
                        "minecraft:oak_boat" => transferred!(boats),
                        _ => transferred!(mobs),
                    }
                    assert!(source_index.lock().unwrap().snapshot(&key).is_empty());
                    let destination_index =
                        game.attachment_indexes.entry(game.dim).or_default().clone();
                    let rows = destination_index.lock().unwrap().snapshot(&key);
                    assert_eq!(rows.len(), usize::from(bound));
                    if let Some(old) = old {
                        let current = match kind {
                            "minecraft:minecart" => game
                                .carts
                                .update(0, |e| e.attachment_lifetime().registration().unwrap()),
                            "minecraft:oak_boat" => game
                                .boats
                                .update(0, |e| e.attachment_lifetime().registration().unwrap()),
                            _ => game.mobs[0].attachment_lifetime.registration().unwrap(),
                        };
                        assert!(!old.same_registration(&current));
                        assert_eq!(
                            rows[0].holder,
                            mod_api::AttachmentHolder::Entity(destination_ref)
                        );
                        assert_eq!(rows[0].entry.read(&VALUE), Ok(&17));
                        assert!(!old.unregister());
                        drop(AttachmentLifetime::new(old));
                        VALUE.set(&mut stale_map, Holder::Entity(id), 99).unwrap();
                        assert_eq!(
                            destination_index.lock().unwrap().snapshot(&key)[0]
                                .entry
                                .read(&VALUE),
                            Ok(&17)
                        );
                        assert_eq!(retained[0].entry.read(&VALUE), Ok(&17));
                        for index in [&source_index, &destination_index] {
                            assert_eq!(index.lock().unwrap().validate_row(&retained[0]),
                                Err(horizon_mod_runtime::attachments::AttachmentRowError::StaleHolder));
                        }
                        assert_eq!(
                            destination_index.lock().unwrap().validate_row(&rows[0]),
                            Ok(())
                        );
                    }
                    assert_eq!(game.persist.uuids.get(&id), Some(&uuid));
                });
            }
        }
    }
}

#[cfg(test)]
mod credits_respawn_tests {
    use crate::game::{Audio, Game, Options, Progress, Settings, load_resources};
    use horizon_client::lan::{Joined, NetConfig, PlayerSender, guest::Guest, hub::HubConfig};
    use horizon_protocol::packets::configuration::ClientInformation;
    use std::sync::{Arc, atomic::AtomicI32, mpsc};

    /// `ServerGamePacketListenerImpl.handleClientCommand(PERFORM_RESPAWN)` for a
    /// player who won the game: `restartClientLoadTimerAfterRespawn`, so the
    /// respawned player is not loaded (its actions wait, `isInvulnerableTo`
    /// spares it) until the client says so or 60 ticks pass.
    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn the_credits_respawn_waits_for_the_client_to_load() {
        let root = std::path::PathBuf::from(std::env::var_os("HORIZON_TEST_RESOURCES").unwrap());
        let settings = Settings::default();
        let loaded = load_resources(
            horizon_assets::resource_source::ResourceRoot::directory(root.clone()),
            Vec::new(),
            None,
            &root,
            &settings,
            std::time::Instant::now(),
            false,
            crate::game::execution_role::ExecutionRole::IntegratedServer,
            &Progress::new(),
            &mut |_, _| unreachable!(),
        )
        .unwrap();
        let mut options = Options::parse(std::iter::empty()).unwrap().unwrap();
        options.resources = root.clone();
        options.execution_role = crate::game::execution_role::ExecutionRole::IntegratedServer;
        let mut game = Game::new(
            &loaded.resources,
            loaded.pack,
            loaded.gui.unwrap(),
            options,
            settings,
            Audio::silent(&horizon_assets::resource_source::ResourceSource::directory(
                root.clone(),
            )),
            None,
        )
        .unwrap();
        let scratch = std::env::temp_dir().join(format!(
            "horizon-credits-respawn-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        game.world_root = scratch.clone();
        game.level.world_spawn = Some([0, 100, 0]);
        game.lan_host(
            super::super::lan::LanSettings {
                port: 0,
                guest_commands: true,
                force_game_mode: false,
            },
            NetConfig {
                announce: false,
                ..NetConfig::loopback(0, "credits-respawn-test".into(), 2)
            },
            HubConfig {
                default_game_mode: 0,
                guest_commands: true,
                force_game_mode: false,
                hardcore: false,
                view_distance: 2,
                simulation_distance: 2,
                dimension: "minecraft:overworld".into(),
                allow_flight: false,
                seed: 0,
                max_players: 2,
                integrated: false,
                levels: Default::default(),
            },
        )
        .unwrap();
        let (tx, _rx) = mpsc::channel();
        let mut guest = Guest::new(
            Joined {
                conn: 2,
                uuid: 330,
                name: "CreditsGuest".into(),
                properties: Vec::new(),
                info: ClientInformation::new(2),
                brand: "test".into(),
                sender: PlayerSender::new(tx, Arc::default()),
                latency: Arc::new(AtomicI32::new(0)),
                address: "127.0.0.1:25565".parse().unwrap(),
                expected: false,
            },
            2,
            [0.5, 100.0, 0.5],
            2,
        );
        guest.dimension = "minecraft:overworld".into();
        guest.placed = true;
        guest.loaded = true;
        guest.loading = false;
        guest.load_timeout = 0;
        let mut host = game.lan.take().unwrap();
        host.hub.guests.push(guest);
        let play = host.play.entry(2).or_default();
        play.won_game = true;
        play.respawn_requested = true;
        assert!(host.hub.guests[0].client_loaded());

        game.lan = Some(host);
        game.handle_server_portal(2);
        let host = game.lan.take().unwrap();
        assert!(!host.play[&2].won_game, "the credits are over");
        let guest = &host.hub.guests[0];
        assert_eq!(guest.load_timeout, 60);
        assert!(guest.loading);
        assert!(!guest.client_loaded());
        drop(host);
        drop(game);
        std::fs::remove_dir_all(scratch).unwrap();
    }
}
