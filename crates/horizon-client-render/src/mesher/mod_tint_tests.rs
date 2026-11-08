use super::*;
use horizon_assets::resource_source::ResourceRoot;
use mod_api::{
    BlockAndTintGetter, BlockState as ModState, BlockTintSource, TintLayer, VanillaTint,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

fn resources() -> Option<std::path::PathBuf> {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.local/horizon-client/26.3");
    root.join("horizon-resources.json").exists().then_some(root)
}

/// A source whose three colours differ, so each reader shows which one it asked.
struct Three;
impl BlockTintSource for Three {
    fn color(&self, _: &ModState) -> u32 {
        0x11_2233
    }
    fn color_in_world(&self, _: &ModState, _: &dyn BlockAndTintGetter, [x, _, _]: [i32; 3]) -> u32 {
        0x44_0000 | x as u32
    }
    fn color_as_terrain_particle(
        &self,
        _: &ModState,
        _: &dyn BlockAndTintGetter,
        _: [i32; 3],
    ) -> u32 {
        0x77_8899
    }
}

/// `BlockColors.register`: a registered block's layers replace vanilla's, a vanilla source in a
/// mod's list colours as it does on vanilla's own blocks, and each reader asks the source's method
/// vanilla asks: `color` without a level, `colorInWorld` for a block drawn in it,
/// `colorAsTerrainParticle` for its particles. A face past the list is untinted.
#[test]
fn registered_layers_replace_vanilla_and_each_reader_asks_its_method() {
    let Some(root) = resources() else {
        return;
    };
    let assets = Assets::load(&root).unwrap();
    let render = assets
        .reload(&ResourceRoot::directory(root.clone()))
        .unwrap();
    let facts = BlockFacts::load(&root, assets.registry).unwrap();
    let visibility = crate::models::load_visibility(&root, assets.registry).unwrap();
    let mut tables = MeshTables::new(&assets, &render, &facts, visibility).unwrap();
    let biomes = Biomes::load(&ResourceRoot::directory(root.clone())).unwrap();
    let state = |name| assets.registry.default_state(name).unwrap();
    let three: Arc<dyn BlockTintSource> = Arc::new(Three);
    let layers = BTreeMap::from([
        (
            "minecraft:oak_leaves".to_string(),
            Arc::from([TintLayer::Source(three.clone())]),
        ),
        (
            "minecraft:stone".to_string(),
            Arc::from([
                TintLayer::Vanilla(VanillaTint::Constant(0xAB_CDEF)),
                TintLayer::Vanilla(VanillaTint::ConstantSplit {
                    hand: 0x01_0101,
                    world: 0x02_0202,
                }),
                TintLayer::Source(three),
                TintLayer::Vanilla(VanillaTint::Grass),
            ]),
        ),
        ("mymod:missing".to_string(), Arc::from([])),
    ]);
    tables.apply_tint_layers(assets.registry, &layers);
    let center = Chunk::empty(0, 0);
    let mut region = [[None; 3]; 3];
    region[1][1] = Some(&center);
    let (zoom, blend) = (BiomeZoom::new(0), 2);
    let world = |name| {
        tables.block_tints(
            state(name),
            [5, 70, 9],
            &region,
            &biomes,
            zoom,
            blend,
            false,
        )
    };
    let particle = |name| {
        let pos = [5, 70, 9];
        tables.terrain_particle_tint(state(name), pos, &region, &biomes, zoom, blend, false)
    };

    let oak = state("minecraft:oak_leaves");
    assert_eq!(tables.default_tint(oak, 0, 0), Some(0x11_2233));
    assert_eq!(
        tables.tint_source(oak, 0),
        Some(TintSource::Fixed(0x11_2233))
    );
    assert_eq!(world("minecraft:oak_leaves")(0), Some(0x44_0005));
    assert_eq!(particle("minecraft:oak_leaves"), Some(0x77_8899));
    // Vanilla's own foliage layer is gone: one layer, and index 1 is past it.
    assert_eq!(world("minecraft:oak_leaves")(1), None);

    let stone = state("minecraft:stone");
    let stone_world = world("minecraft:stone");
    assert_eq!(tables.default_tint(stone, 0, 0), Some(0xAB_CDEF));
    assert_eq!(stone_world(0), Some(0xAB_CDEF));
    assert_eq!(tables.default_tint(stone, 1, 0), Some(0x01_0101));
    assert_eq!(stone_world(1), Some(0x02_0202));
    assert_eq!(stone_world(2), Some(0x44_0005));
    // A vanilla source on a mod's list: the same grass as short grass at the same place.
    assert_eq!(stone_world(3), world("minecraft:short_grass")(0));
    assert!(stone_world(3).is_some());
    assert_eq!(stone_world(4), None);
    assert_eq!(tables.default_tint(stone, 4, 0), None);
}

/// Colours a leaf block by what is under it and by its `distance`, counting its calls.
struct ByGround {
    calls: AtomicUsize,
}
impl BlockTintSource for ByGround {
    fn color(&self, _: &ModState) -> u32 {
        0xFF_FFFF
    }
    fn color_in_world(
        &self,
        state: &ModState,
        level: &dyn BlockAndTintGetter,
        [x, y, z]: [i32; 3],
    ) -> u32 {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let on_stone = &*level.block_state([x, y - 1, z]).block == "minecraft:stone";
        match (on_stone, state.value("distance")) {
            (true, Some("1")) => 0x40_80C0,
            (true, _) => 0x20_4060,
            (false, _) => 0x10_2030,
        }
    }
}

/// The chunk mesher asks a mod's `colorInWorld` once per block, whatever number of faces share
/// its tint index, hands it the block's state and a level it reads around the block, and draws
/// the faces with its answer.
#[test]
fn the_mesher_asks_a_mod_source_once_per_block_with_the_world_around_it() {
    let Some(root) = resources() else {
        return;
    };
    let assets = Assets::load(&root).unwrap();
    let facts = BlockFacts::load(&root, assets.registry).unwrap();
    let biomes = Biomes::load(&ResourceRoot::directory(root.clone())).unwrap();
    let render = assets
        .reload(&ResourceRoot::directory(root.clone()))
        .unwrap();
    let visibility = crate::models::load_visibility(&root, assets.registry).unwrap();
    let mut tables = MeshTables::new(&assets, &render, &facts, visibility.clone()).unwrap();
    let source = Arc::new(ByGround {
        calls: AtomicUsize::new(0),
    });
    let layers = BTreeMap::from([(
        "minecraft:oak_leaves".to_string(),
        Arc::from([TintLayer::Source(source.clone())]),
    )]);
    tables.apply_tint_layers(assets.registry, &layers);
    let registry = assets.registry;
    let oak = registry.default_state("minecraft:oak_leaves").unwrap();
    let mut near = registry.state(oak).unwrap().clone();
    near.properties.insert("distance".into(), "1".into());
    let near = registry.id(&near).unwrap();
    let stone = registry.default_state("minecraft:stone").unwrap();
    let mut chunk = Chunk::empty(0, 0);
    // On stone at distance 1, on stone at distance 7, and floating.
    chunk.set(2, MIN_Y + 4, 2, stone.0);
    chunk.set(2, MIN_Y + 5, 2, near.0);
    chunk.set(8, MIN_Y + 4, 8, stone.0);
    chunk.set(8, MIN_Y + 5, 8, oak.0);
    chunk.set(13, MIN_Y + 9, 13, oak.0);
    let mut region = [[None; 3]; 3];
    region[1][1] = Some(&chunk);
    let mut mesher = Mesher {
        nether_shading: false,
        shader_pack_lighting: false,
        separate_ao: false,
        shader_pack: false,
        assets: &assets,
        render: &render,
        tables: &tables,
        biomes: &biomes,
        visibility,
        zoom: BiomeZoom::new(0),
        blend_radius: 0,
        smooth_lighting: false,
        cutout_leaves: true,
        opaque_ice: false,
        translucent_layers: &[],
        leaf_culling: LeafCullingMode::Off,
        hidden_states: &[],
    };
    let mesh = mesher.mesh(&region, 0);
    assert_eq!(source.calls.load(Ordering::Relaxed), 3);
    // The up faces are unshaded (1.0), so they carry the tint itself, packed as RGBA.
    let colours: Vec<u32> = mesh
        .solid
        .iter()
        .chain(&mesh.translucent)
        .map(|v| v.color)
        .collect();
    for tint in [0x40_80C0, 0x20_4060, 0x10_2030] {
        let packed = pack_rgb([tint >> 16 & 255, tint >> 8 & 255, tint & 255]);
        assert!(colours.contains(&packed), "no face tinted {tint:06X}");
    }
}
