//! Section entry order (`EntitySection` after `Callback.onMove`) and the
//! admission of an entity given its id after it spawned.
use super::*;
use crate::join_seq::residency::Kind;

const HOME: Owner = Owner {
    dimension: DimensionKey::OVERWORLD,
    kind: Kind::Mob,
};
const LOW: [i32; 3] = [0, 4, 0];
const HIGH: [i32; 3] = [0, 5, 0];

fn emit(journal: &Journal, change: Change) {
    journal.push(Event {
        owner: HOME,
        change,
    });
}

/// Admit `id` now, as a loaded entity joins the lookup.
fn admit(journal: &Journal, id: u32) -> Identity {
    let identity = Identity::new(id, JoinSeq::next()).unwrap();
    emit(journal, Change::Install(identity));
    identity
}

/// One section pass noting these members' sections.
fn pass<const N: usize>(table: &mut EntityTable, members: [(u32, [i32; 3]); N]) {
    table.note_sections(|pass| {
        for (id, section) in members {
            pass.note(id, section);
        }
    });
}

/// The ids in one section, in the order that section keeps them.
fn section(table: &mut EntityTable, ids: &[u32]) -> Vec<u32> {
    let entry = table.section_entry_order();
    let mut ids: Vec<_> = ids.iter().map(|&id| (entry(id).unwrap(), id)).collect();
    ids.sort();
    ids.into_iter().map(|(_, id)| id).collect()
}

#[test]
fn a_mover_enters_the_end_of_its_new_section_and_keeps_its_lookup_place() {
    let journal = Journal::default();
    let mut table = EntityTable::new(journal.clone());
    for id in [1, 2, 3] {
        admit(&journal, id);
    }
    // 4 joins section HIGH directly, before anyone moves there.
    admit(&journal, 4);
    pass(&mut table, [(1, LOW), (2, LOW), (3, LOW), (4, HIGH)]);
    pass(&mut table, [(1, LOW), (2, HIGH), (3, LOW), (4, HIGH)]);
    pass(&mut table, [(1, HIGH), (2, HIGH), (3, LOW), (4, HIGH)]);
    assert_eq!(section(&mut table, &[1, 2, 4]), vec![4, 2, 1]);
    assert_eq!(section(&mut table, &[3]), vec![3]);
    // Moving back is entering again: the end of the section it left.
    pass(&mut table, [(1, HIGH), (2, LOW), (3, LOW), (4, HIGH)]);
    assert_eq!(section(&mut table, &[2, 3]), vec![3, 2]);
    // The lookup is a different list: a section move never reorders it.
    assert_eq!(table.level_entities(HOME.dimension), vec![1, 2, 3, 4]);
}

#[test]
fn movers_of_one_pass_enter_in_lookup_order_and_a_put_back_keeps_its_place() {
    let journal = Journal::default();
    let mut table = EntityTable::new(journal.clone());
    let first = admit(&journal, 9);
    admit(&journal, 5);
    admit(&journal, 7);
    pass(&mut table, [(9, LOW), (5, LOW), (7, HIGH)]);
    // Listed against lookup order: 5 and 9 still enter HIGH as 9, then 5.
    pass(&mut table, [(5, HIGH), (9, HIGH), (7, HIGH)]);
    assert_eq!(section(&mut table, &[5, 7, 9]), vec![7, 9, 5]);
    // A temporary extraction is not a move.
    emit(&journal, Change::Extract(first));
    emit(&journal, Change::PutBack(first));
    assert_eq!(section(&mut table, &[5, 7, 9]), vec![7, 9, 5]);
    // Removal and re-admission is a new entity at the end (`addEntity`).
    emit(&journal, Change::Remove(first));
    emit(&journal, Change::Install(first));
    pass(&mut table, [(9, HIGH)]);
    assert_eq!(section(&mut table, &[5, 7, 9]), vec![7, 5, 9]);
    assert_eq!(table.level_entities(HOME.dimension), vec![5, 7, 9]);
}

#[test]
fn an_id_given_after_spawning_admits_the_entity_where_it_spawned() {
    let journal = Journal::default();
    let mut table = EntityTable::new(journal.clone());
    // An arrow joins its storage with no id yet (`addFreshEntity`), then a
    // chunk loads a zombie, then the arrow gets its id.
    let arrow = Identity::new(30, JoinSeq::next()).unwrap();
    admit(&journal, 20);
    emit(&journal, Change::Spawned(arrow));
    assert_eq!(table.level_entities(HOME.dimension), vec![30, 20]);
    assert_eq!(section(&mut table, &[20, 30]), vec![30, 20]);
    // Only a new member: an id reported again keeps the admission it has.
    emit(&journal, Change::Remove(arrow));
    admit(&journal, 21);
    emit(&journal, Change::Install(arrow));
    emit(&journal, Change::Spawned(arrow));
    assert_eq!(table.level_entities(HOME.dimension), vec![20, 21, 30]);
}
