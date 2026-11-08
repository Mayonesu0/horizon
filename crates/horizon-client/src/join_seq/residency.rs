//! Structural events from the numbered entity owners of one game instance.
//! Bindings ride with storage when dimensions are parked; no event holds Game.
use super::JoinSeq;
use mod_api::DimensionKey;
use std::{
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[path = "residency/table.rs"]
pub mod table;

/// Authoritative entity storage families, excluding the dragon's eight parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Mob,
    Item,
    Orb,
    Projectile,
    Stand,
    Frame,
    Painting,
    Boat,
    Cart,
    Other,
    Fangs,
    Crystal,
    Dragon,
    Tnt,
    Falling,
    Rocket,
    Trident,
    Spawner,
    Cloud,
    Cushion,
}

/// The dimension and family owning a list or singleton.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Owner {
    pub dimension: DimensionKey,
    pub kind: Kind,
}

/// Existing runtime identity, not a new residency token or attachment slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub id: NonZeroU32,
    pub join: JoinSeq,
}
impl Identity {
    /// A pending id of zero cannot name an entity in the lookup table.
    pub fn new(id: u32, join: JoinSeq) -> Option<Self> {
        Some(Self {
            id: NonZeroU32::new(id)?,
            join,
        })
    }
}

/// A structural operation; state-only writes emit nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Install(Identity),
    Remove(Identity),
    Extract(Identity),
    PutBack(Identity),
    Lifted,
    Restored,
}

/// One operation on one authoritative owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub owner: Owner,
    pub change: Change,
}

#[derive(Debug, Default)]
struct Pending {
    events: Mutex<Vec<Event>>,
    ready: AtomicBool,
    retired: AtomicBool,
}

/// Shared only by the owners of one game instance and its parked dimensions.
/// Cloning shares the sink, never an entity or a reference back to the game.
#[derive(Clone, Debug, Default)]
pub struct Journal(Arc<Pending>);
impl Journal {
    /// Record an operation at the storage boundary, in mutation order.
    pub fn push(&self, event: Event) {
        let mut events = self.0.events.lock().unwrap();
        if self.0.retired.load(Ordering::Relaxed) {
            return;
        }
        debug_assert!(events.len() < 1_000_000, "entity journal was not drained");
        events.push(event);
        self.0.ready.store(true, Ordering::Release);
    }

    /// Exchange the pending batch with an empty reusable scratch buffer.
    /// Consumers run after releasing the lock and can safely produce events.
    /// An idle drain takes no mutex and performs no allocation.
    pub fn drain_into(&self, scratch: &mut Vec<Event>) {
        assert!(scratch.is_empty(), "previous entity batch was not consumed");
        if !self.0.ready.load(Ordering::Acquire) {
            return;
        }
        let mut events = self.0.events.lock().unwrap();
        std::mem::swap(&mut *events, scratch);
        self.0.ready.store(false, Ordering::Release);
    }

    /// Retire this epoch and all of its producer clones, discarding its queue.
    /// The fresh sink is the new epoch identity; old producers cannot append to
    /// either queue. Rebind live owners to fresh clones after a table reset.
    pub fn reset_epoch(&mut self) {
        let next = Self::default();
        {
            let mut events = self.0.events.lock().unwrap();
            self.0.retired.store(true, Ordering::Relaxed);
            *events = Vec::new();
            self.0.ready.store(false, Ordering::Release);
        }
        *self = next;
    }

    /// Forget queued operations without retiring connected producers.
    /// This is a queue clear, not a table/epoch reset barrier.
    pub fn clear(&self) {
        let mut events = self.0.events.lock().unwrap();
        events.clear();
        self.0.ready.store(false, Ordering::Release);
    }
}

/// Storage-local identity accessor and journal sink, never a Game reference.
#[derive(Debug)]
pub(super) struct Binding<T> {
    journal: Journal,
    owner: Owner,
    identity: fn(&T) -> (u32, JoinSeq),
}
impl Journal {
    pub(super) fn bind<T>(&self, owner: Owner, identity: fn(&T) -> (u32, JoinSeq)) -> Binding<T> {
        Binding {
            journal: self.clone(),
            owner,
            identity,
        }
    }
}
impl<T> Clone for Binding<T> {
    fn clone(&self) -> Self {
        Self {
            journal: self.journal.clone(),
            owner: self.owner,
            identity: self.identity,
        }
    }
}
impl<T> Binding<T> {
    pub(super) fn same(&self, other: &Self) -> bool {
        self.owner == other.owner && Arc::ptr_eq(&self.journal.0, &other.journal.0)
    }
    pub(super) fn marker(&self, change: Change) {
        self.journal.push(Event {
            owner: self.owner,
            change,
        });
    }
    pub(super) fn identity(&self, id: u32, join: JoinSeq, change: fn(Identity) -> Change) {
        if let Some(identity) = Identity::new(id, join) {
            self.marker(change(identity));
        }
    }
    pub(super) fn read(&self, entity: &T) -> (u32, JoinSeq) {
        (self.identity)(entity)
    }
    pub(super) fn entity(&self, entity: &T, change: fn(Identity) -> Change) {
        let (id, join) = self.read(entity);
        self.identity(id, join, change);
    }
    pub(super) fn all(&self, entities: &[T], change: fn(Identity) -> Change) {
        for entity in entities {
            self.entity(entity, change);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(dimension: DimensionKey, kind: Kind, change: Change) -> Event {
        Event {
            owner: Owner { dimension, kind },
            change,
        }
    }

    #[test]
    fn drains_keep_cross_dimension_order_and_events_produced_by_consumers() {
        let journal = Journal::default();
        let other_dimension = journal.clone();
        let owner = Identity::new(7, JoinSeq::next()).unwrap();
        let install = event(DimensionKey::OVERWORLD, Kind::Other, Change::Install(owner));
        let extract = event(DimensionKey::NETHER, Kind::Falling, Change::Extract(owner));
        let terminal = event(DimensionKey::NETHER, Kind::Falling, Change::PutBack(owner));
        journal.push(install);
        other_dimension.push(extract);
        let mut scratch = Vec::new();
        journal.drain_into(&mut scratch);
        assert_eq!(scratch, [install, extract]);
        // A consumer emitting an event cannot deadlock or lose that next batch.
        for _ in scratch.drain(..) {
            other_dimension.push(terminal);
        }
        journal.drain_into(&mut scratch);
        assert_eq!(scratch, [terminal, terminal]);
        scratch.clear();
        let capacity = scratch.capacity();
        journal.drain_into(&mut scratch);
        assert!(scratch.is_empty());
        assert_eq!(scratch.capacity(), capacity);
    }

    #[test]
    fn reset_discards_old_operations_without_orphaning_parked_producers() {
        let journal = Journal::default();
        let parked = journal.clone();
        let old = event(DimensionKey::OVERWORLD, Kind::Mob, Change::Lifted);
        let new = event(DimensionKey::END, Kind::Dragon, Change::Restored);
        journal.push(old);
        journal.clear();
        let mut scratch = Vec::new();
        journal.drain_into(&mut scratch);
        assert!(scratch.is_empty());
        parked.push(new);
        journal.drain_into(&mut scratch);
        assert_eq!(scratch, [new]);
    }

    fn assert_idle_drain_does_not_wait_for_the_pending_mutex(journal: Journal) {
        use std::{sync::mpsc, time::Duration};
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = journal.clone();
        let holder = std::thread::spawn(move || {
            let _guard = holder.0.events.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        locked_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let drainer = std::thread::spawn(move || {
            let mut scratch = Vec::new();
            journal.drain_into(&mut scratch);
            done_tx.send(scratch).unwrap();
        });
        let result = done_rx.recv_timeout(Duration::from_secs(2));
        // Release and join before asserting, including on the timeout path.
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        drainer.join().unwrap();
        assert!(result.is_ok(), "idle drain blocked on the pending mutex");
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn idle_drain_after_consuming_a_batch_does_not_lock() {
        let journal = Journal::default();
        journal.push(event(DimensionKey::OVERWORLD, Kind::Mob, Change::Lifted));
        let mut scratch = Vec::new();
        journal.drain_into(&mut scratch);
        assert_eq!(scratch.len(), 1);
        scratch.clear();
        assert_idle_drain_does_not_wait_for_the_pending_mutex(journal);
    }

    #[test]
    fn idle_drain_after_clear_does_not_lock() {
        let journal = Journal::default();
        journal.push(event(DimensionKey::OVERWORLD, Kind::Mob, Change::Lifted));
        journal.clear();
        assert_idle_drain_does_not_wait_for_the_pending_mutex(journal);
    }

    #[test]
    fn fresh_idle_drain_does_not_lock() {
        assert_idle_drain_does_not_wait_for_the_pending_mutex(Journal::default());
    }

    #[test]
    fn epoch_reset_discards_pending_and_rejects_a_late_parked_producer() {
        let mut journal = Journal::default();
        let stale = journal.clone();
        let old = event(DimensionKey::NETHER, Kind::Mob, Change::Lifted);
        stale.push(old);
        journal.reset_epoch();
        assert!(!Arc::ptr_eq(&journal.0, &stale.0));
        assert!(stale.0.events.lock().unwrap().is_empty());
        assert_idle_drain_does_not_wait_for_the_pending_mutex(stale.clone());
        // The old server/dimension resumes only after the reset has completed.
        std::thread::spawn({
            let stale = stale.clone();
            move || {
                for _ in 0..1000 {
                    stale.push(old);
                }
            }
        })
        .join()
        .unwrap();
        let mut scratch = Vec::new();
        stale.drain_into(&mut scratch);
        assert!(
            scratch.is_empty(),
            "stale producers must not accumulate events"
        );
        journal.drain_into(&mut scratch);
        assert!(
            scratch.is_empty(),
            "reset leaked a previous epoch into the table"
        );
        let fresh = journal.clone();
        let new = event(DimensionKey::END, Kind::Dragon, Change::Restored);
        fresh.push(new);
        journal.drain_into(&mut scratch);
        assert_eq!(scratch, [new]);
    }

    #[test]
    fn retirement_is_rechecked_after_a_blocked_push_takes_the_mutex() {
        use std::{sync::mpsc, time::Duration};
        let stale = Journal::default();
        let guard = stale.0.events.lock().unwrap();
        let pusher = stale.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let pusher = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            pusher.push(event(DimensionKey::NETHER, Kind::Mob, Change::Lifted));
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // Give an incorrect pre-lock load time to observe retired=false.
        // Correct code cannot read retirement until this guard is released.
        std::thread::sleep(Duration::from_millis(100));
        stale.0.retired.store(true, Ordering::Relaxed);
        drop(guard);
        pusher.join().unwrap();
        assert!(stale.0.events.lock().unwrap().is_empty());
        assert!(!stale.0.ready.load(Ordering::Acquire));
    }

    #[test]
    fn repeated_epoch_resets_never_reactivate_an_older_producer() {
        let mut journal = Journal::default();
        let first = journal.clone();
        journal.reset_epoch();
        let second = journal.clone();
        journal.reset_epoch();
        let value = event(DimensionKey::OVERWORLD, Kind::Other, Change::Restored);
        first.push(value);
        second.push(value);
        let mut scratch = Vec::new();
        for retired in [first, second] {
            retired.drain_into(&mut scratch);
            assert!(scratch.is_empty());
        }
        journal.push(value);
        journal.drain_into(&mut scratch);
        assert_eq!(scratch, [value]);
    }

    #[test]
    fn concurrent_producers_are_delivered_once_in_each_producers_order() {
        let journal = Journal::default();
        let threads: Vec<_> = [DimensionKey::OVERWORLD, DimensionKey::NETHER]
            .into_iter()
            .map(|dimension| {
                let producer = journal.clone();
                std::thread::spawn(move || {
                    for id in 1..=200 {
                        producer.push(event(
                            dimension,
                            Kind::Mob,
                            Change::Install(Identity::new(id, JoinSeq::next()).unwrap()),
                        ));
                    }
                })
            })
            .collect();
        let mut received = Vec::new();
        let mut scratch = Vec::new();
        while threads.iter().any(|t| !t.is_finished()) {
            journal.drain_into(&mut scratch);
            received.append(&mut scratch);
            std::thread::yield_now();
        }
        for thread in threads {
            thread.join().unwrap();
        }
        journal.drain_into(&mut scratch);
        received.append(&mut scratch);
        for dimension in [DimensionKey::OVERWORLD, DimensionKey::NETHER] {
            let ids: Vec<_> = received
                .iter()
                .filter_map(|e| {
                    if let Change::Install(entity) = e.change
                        && e.owner.dimension == dimension
                    {
                        Some(entity.id.get())
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(ids, (1..=200).collect::<Vec<_>>());
        }
    }
}
