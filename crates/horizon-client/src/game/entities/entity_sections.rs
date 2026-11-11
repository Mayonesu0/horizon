//! Which section each lookup member is in, and since when: vanilla's
//! `PersistentEntitySectionManager.Callback.onMove` takes an entity that
//! crossed into another section out of the old one and adds it at the end of
//! the new one, so a chunk stores (`storeChunkSections`) and reloads it there.
use super::{
    Game,
    entity::{Entity, entity_lists_of, for_each_entity},
};
use horizon_client::join_seq::JoinSeq;
use horizon_core::geometry::Vec3;

/// `SectionPos.of(entity.blockPosition())`.
pub(in crate::game) fn section_of(at: Vec3) -> [i32; 3] {
    [at.x, at.y, at.z].map(|v| (v.floor() as i32) >> 4)
}

impl Game<'_> {
    /// Note the section of every member of the level being ticked that can
    /// move, after its entities ticked, and before a save stores its chunks.
    /// Hanging entities, fangs, spawners, clouds and knots never leave theirs.
    pub(in crate::game) fn note_entity_sections(&mut self) {
        let mut table = self.resident_entities.table.borrow_mut();
        table.note_sections(|pass| {
            for_each_entity!(entity_lists_of!(self), e => {
                pass.note(e.id(), section_of(e.position()));
            });
            for o in self.others.iter() {
                pass.note(o.id, section_of(o.position));
            }
            for c in self.crystals.iter() {
                pass.note(c.id, section_of(c.position));
            }
            for c in self.cushions.iter() {
                pass.note(c.id, section_of(c.position));
            }
            if let Some(d) = self.dragon.as_ref() {
                pass.note(d.id, section_of(d.position));
            }
        });
    }

    /// Each lookup member's place in its section, sections noted first.
    pub(in crate::game) fn section_entries(&mut self) -> impl Fn(u32) -> Option<JoinSeq> + '_ {
        self.note_entity_sections();
        self.resident_entities.table.get_mut().section_entry_order()
    }
}
