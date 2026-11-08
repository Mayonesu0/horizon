use super::*;
use crate::game::native_entity_refs::tests::with_game;
use horizon_client::join_seq::JoinSeq;
use horizon_mod_runtime::attachments::{AttachmentLifetime, Attachments, index::AttachmentIndex};

fn identity<'a>(
    game: &'a mut Game<'_>,
    target: Target,
) -> (&'a mut JoinSeq, &'a mut AttachmentLifetime) {
    macro_rules! fields {
        ($list:expr, $id:expr) => {{
            let entity = $list.iter_mut().find(|entity| entity.id == $id).unwrap();
            (&mut entity.join, &mut entity.attachment_lifetime)
        }};
    }
    match target {
        Target::Mob(id) => fields!(game.mobs, id),
        Target::Item(id) => fields!(game.dropped, id),
        Target::Stand(id) => fields!(game.armor_stands, id),
        Target::Frame(id) => fields!(game.item_frames, id),
        _ => unreachable!("fixture only admits the four decoder-replacement kinds"),
    }
}

fn reload_preserves_runtime_identity(kind: &str) {
    with_game(|game| {
        let position =
            |x: f64| Tag::List([x, 65.0, 8.5].map(|v| Tag::Double(v.to_bits())).to_vec());
        let raw = Tag::Compound(
            [
                ("id".into(), Tag::String(kind.into())),
                ("Pos".into(), position(8.5)),
                (
                    "Item".into(),
                    Tag::Compound(
                        [
                            ("id".into(), Tag::String("minecraft:stone".into())),
                            ("count".into(), Tag::Int(1)),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let target = game
            .spawn_entity_from_nbt(raw)
            .expect("fixture entity loads");
        match target {
            Target::Mob(id) => {
                let mob = game.mobs.iter_mut().find(|mob| mob.id == id).unwrap();
                mob.age = 17;
                mob.invulnerable = 9;
                mob.last_hurt = 2.0;
            }
            Target::Item(id) => {
                game.dropped
                    .iter_mut()
                    .find(|item| item.id == id)
                    .unwrap()
                    .bob_offset = 2.25
            }
            _ => {}
        }
        let mut map = Attachments::default();
        let index = std::sync::Arc::new(std::sync::Mutex::new(AttachmentIndex::default()));
        let (join, lifetime) = identity(game, target);
        let original_join = *join;
        assert!(original_join.is_joined());
        *lifetime = AttachmentLifetime::new(map.bind_index(&index, 1u32));
        let original_registration = lifetime.registration().unwrap();
        let mut tag = game.entity_nbt_in_level(target).unwrap();
        let Tag::List(mut coords) = tag.get("Pos").cloned().unwrap() else {
            panic!("saved position list")
        };
        let Tag::Double(x) = coords[0] else {
            panic!("saved position coordinate")
        };
        coords[0] = Tag::Double((f64::from_bits(x) + 1.0).to_bits());
        let moved = Tag::List(coords);
        set_key(&mut tag, "Pos", moved.clone());
        if matches!(target, Target::Frame(_)) {
            let Tag::IntArray(mut anchor) = tag.get("block_pos").cloned().unwrap() else {
                panic!("saved frame anchor")
            };
            anchor[0] += 1;
            set_key(&mut tag, "block_pos", Tag::IntArray(anchor));
        }
        game.apply_entity_nbt(target, tag).unwrap();
        assert_eq!(
            game.entity_nbt_in_level(target).unwrap().get("Pos"),
            Some(&moved)
        );
        match target {
            Target::Mob(id) => {
                let mob = game.mobs.iter().find(|mob| mob.id == id).unwrap();
                assert_eq!((mob.age, mob.invulnerable, mob.last_hurt), (17, 9, 2.0));
            }
            Target::Item(id) => assert_eq!(
                game.dropped
                    .iter()
                    .find(|item| item.id == id)
                    .unwrap()
                    .bob_offset,
                2.25
            ),
            _ => {}
        }
        let (join, lifetime) = identity(game, target);
        assert_eq!(*join, original_join, "{kind} changed its join on data load");
        assert!(
            lifetime
                .registration()
                .is_some_and(|current| current.same_registration(&original_registration)),
            "{kind} lost its original owner registration"
        );
    });
}

#[test]
#[ignore = "needs prepared resources"]
fn mob_data_load_keeps_runtime_identity() {
    reload_preserves_runtime_identity("minecraft:pig");
}

#[test]
#[ignore = "needs prepared resources"]
fn item_data_load_keeps_runtime_identity() {
    reload_preserves_runtime_identity("minecraft:item");
}

#[test]
#[ignore = "needs prepared resources"]
fn stand_data_load_keeps_runtime_identity() {
    reload_preserves_runtime_identity("minecraft:armor_stand");
}

#[test]
#[ignore = "needs prepared resources"]
fn frame_data_load_keeps_runtime_identity() {
    reload_preserves_runtime_identity("minecraft:item_frame");
}
