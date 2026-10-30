//! Игровые текстуры рендера: рельеф, объекты, постройки, предметы, сущности.
//!
//! Вынесены из `rendering/mod.rs` в отдельный файл: видов текстур много и
//! будет больше, а мешам блоков нужен только доступ к хэндлам по виду
//! (`object_texture`, `item_texture`, …).

use bevy::prelude::*;

use crate::simulation::{
    EntityKind, EntityState, ItemKind, ObjectKind, WorldObject, ITEM_KIND_COUNT,
};

/// Хэндлы текстур для всех типов тайлов, объектов и игрока.
#[derive(Resource)]
pub(crate) struct TileTextures {
    water: Handle<Image>,
    sand: Handle<Image>,
    dirt: Handle<Image>,
    grass: Handle<Image>,
    forest_grass: Handle<Image>,
    stone: Handle<Image>,
    clay: Handle<Image>,
    farmland: Handle<Image>,
    deep_water: Handle<Image>,
    ocean: Handle<Image>,
    mountain: Handle<Image>,
    tree: Handle<Image>,
    bush: Handle<Image>,
    rock: Handle<Image>,
    apple_tree: Handle<Image>,
    apple_tree_empty: Handle<Image>,
    /// Текстуры построек по дискриминанту вида-постройки.
    buildings: [Handle<Image>; BUILDING_KIND_COUNT],
    /// Иконки предметов по дискриминанту `ItemKind`.
    items: [Handle<Image>; ITEM_KIND_COUNT],
    player: Handle<Image>,
    wolf_standing: Handle<Image>,
    wolf_lying: Handle<Image>,
    wolf_walk: [Handle<Image>; 2],
}

/// Число видов-построек с текстурами (порядок совпадает с
/// `ObjectKind::BUILDINGS`).
pub(crate) const BUILDING_KIND_COUNT: usize = 1;

impl TileTextures {
    /// Загружает все текстуры из `assets/textures/...`.
    pub(crate) fn load(assets: &AssetServer) -> Self {
        Self {
            water: assets.load("textures/tiles/water.png"),
            sand: assets.load("textures/tiles/sand.png"),
            dirt: assets.load("textures/tiles/dirt.png"),
            grass: assets.load("textures/tiles/grass.png"),
            forest_grass: assets.load("textures/tiles/forest_grass.png"),
            stone: assets.load("textures/tiles/stone.png"),
            clay: assets.load("textures/tiles/clay.png"),
            farmland: assets.load("textures/tiles/farmland.png"),
            deep_water: assets.load("textures/tiles/deep_water.png"),
            ocean: assets.load("textures/tiles/ocean.png"),
            mountain: assets.load("textures/tiles/mountain.png"),
            tree: assets.load("textures/objects/tree.png"),
            bush: assets.load("textures/objects/bush.png"),
            rock: assets.load("textures/objects/rock.png"),
            apple_tree: assets.load("textures/objects/apple_tree.png"),
            apple_tree_empty: assets.load("textures/objects/apple_tree_empty.png"),
            buildings: [assets.load("textures/buildings/campfire.png")],
            items: [
                assets.load("textures/items/stick.png"),
                assets.load("textures/items/rock.png"),
                assets.load("textures/items/food/apple.png"),
                assets.load("textures/items/grass_bunch_raw.png"),
                assets.load("textures/items/plant_fiber.png"),
                assets.load("textures/items/cordage_fiber.png"),
                assets.load("textures/items/tools/axe_primal.png"),
                assets.load("textures/items/tools/pickaxe_primal.png"),
                assets.load("textures/items/tools/hoe_primal.png"),
                assets.load("textures/items/tools/shovel_primal.png"),
                assets.load("textures/items/weapon/spear_primal.png"),
                assets.load("textures/items/tools/knife_stone.png"),
                assets.load("textures/items/tools/fishing_rod_primal.png"),
                assets.load("textures/items/weapon/arrow.png"),
                assets.load("textures/items/weapon/bow_wood.png"),
                assets.load("textures/items/armor/helmet_chainmail.png"),
                assets.load("textures/items/armor/chestplate_chainmail.png"),
                assets.load("textures/items/armor/leggings_chainmail.png"),
                assets.load("textures/items/armor/boots_chainmail.png"),
                assets.load("textures/items/bone.png"),
                assets.load("textures/items/shard/shard_bone.png"),
                assets.load("textures/items/arrow_fletching.png"),
                assets.load("textures/items/feather.png"),
                assets.load("textures/items/string.png"),
                assets.load("textures/items/stick_bundle.png"),
                assets.load("textures/items/book.png"),
                assets.load("textures/items/paper.png"),
                assets.load("textures/items/paper_pulp.png"),
                assets.load("textures/items/clods/clod_clay.png"),
                assets.load("textures/items/brick_clay_unfired.png"),
                assets.load("textures/items/brick_clay_baked.png"),
                assets.load("textures/items/seeds/seeds_wheat.png"),
                assets.load("textures/items/wheat.png"),
                assets.load("textures/items/tools/hammer_stone.png"),
                assets.load("textures/items/nugget/nugget_copper.png"),
                assets.load("textures/items/nugget/nugget_gold.png"),
                assets.load("textures/items/nugget/nugget_silver.png"),
                assets.load("textures/items/ingot/ingot_copper.png"),
                assets.load("textures/items/ingot/ingot_bronze.png"),
                assets.load("textures/items/ingot/ingot_tin.png"),
                assets.load("textures/items/ingot/ingot_gold.png"),
                assets.load("textures/items/ingot/ingot_silver.png"),
                assets.load("textures/items/gem/gem_quartz.png"),
                assets.load("textures/items/gem/gem_flint.png"),
                assets.load("textures/items/gem/gem_emerald.png"),
                assets.load("textures/items/gem/gem_ruby.png"),
                assets.load("textures/items/dust/dust_bronze.png"),
                assets.load("textures/items/dust/dust_charcoal.png"),
                assets.load("textures/items/dust/dust_copper.png"),
                assets.load("textures/items/dust/dust_flour.png"),
                assets.load("textures/items/dust/dust_gold.png"),
                assets.load("textures/items/dust/dust_pit_ash.png"),
                assets.load("textures/items/dust/dust_silver.png"),
                assets.load("textures/items/dust/dust_sugar.png"),
                assets.load("textures/items/dust/dust_sulfur.png"),
                assets.load("textures/items/dust/dust_tin.png"),
                assets.load("textures/items/dust/dust_wood_chips.png"),
                assets.load("textures/items/food/apple_baked.png"),
                assets.load("textures/items/food/carrot.png"),
                assets.load("textures/items/food/carrot_baked.png"),
                assets.load("textures/items/food/egg.png"),
                assets.load("textures/items/food/egg_roasted.png"),
                assets.load("textures/items/food/lard.png"),
                assets.load("textures/items/food/meat_bird_raw.png"),
                assets.load("textures/items/food/meat_bird_cooked.png"),
                assets.load("textures/items/food/meat_herbivore_raw.png"),
                assets.load("textures/items/food/meat_herbivore_cooked.png"),
                assets.load("textures/items/food/meat_predator_raw.png"),
                assets.load("textures/items/food/meat_predator_cooked.png"),
                assets.load("textures/items/food/potato.png"),
                assets.load("textures/items/food/potato_baked.png"),
                assets.load("textures/items/food/strange_tuber.png"),
                assets.load("textures/items/food/strawberries.png"),
                assets.load("textures/items/raw_ore/raw_copper.png"),
                assets.load("textures/items/raw_ore/raw_gold.png"),
                assets.load("textures/items/raw_ore/raw_silver.png"),
                assets.load("textures/items/raw_ore/raw_tin.png"),
                assets.load("textures/items/tools/axe_copper.png"),
                assets.load("textures/items/tools/hammer_copper.png"),
                assets.load("textures/items/tools/hoe_copper.png"),
                assets.load("textures/items/tools/pickaxe_copper.png"),
                assets.load("textures/items/tools/saw_copper.png"),
                assets.load("textures/items/tools/shears_copper.png"),
                assets.load("textures/items/tools/shovel_copper.png"),
                assets.load("textures/items/tools/sickle_copper.png"),
            ],
            player: assets.load("textures/entities/player.png"),
            wolf_standing: assets.load("textures/entities/wolf/wolf_standing.png"),
            wolf_lying: assets.load("textures/entities/wolf/wolf_lying.png"),
            wolf_walk: [
                assets.load("textures/entities/wolf/wolf_walk1.png"),
                assets.load("textures/entities/wolf/wolf_walk2.png"),
            ],
        }
    }

    /// Хэндл текстуры по объекту. Пустая яблоня (без плодов) рисуется
    /// отдельной текстурой, но остаётся тем же видом объекта.
    pub(crate) fn object_texture(&self, obj: &WorldObject) -> &Handle<Image> {
        match obj.kind {
            ObjectKind::AppleTree if obj.charge == 0 => &self.apple_tree_empty,
            kind => self.kind_texture(kind),
        }
    }

    /// Хэндл текстуры по виду объекта (яблоня — плодоносящая).
    pub(crate) fn kind_texture(&self, kind: ObjectKind) -> &Handle<Image> {
        match kind {
            ObjectKind::Tree => &self.tree,
            ObjectKind::Bush => &self.bush,
            ObjectKind::Rock => &self.rock,
            ObjectKind::AppleTree => &self.apple_tree,
            ObjectKind::Campfire => &self.buildings[0],
        }
    }

    /// Хэндл текстуры предмета.
    pub(crate) fn item_texture(&self, kind: ItemKind) -> &Handle<Image> {
        &self.items[kind as usize]
    }

    /// Хэндл текстуры игрока.
    pub(crate) fn player_texture(&self) -> &Handle<Image> {
        &self.player
    }

    /// Текстура существа по виду, состоянию и кадру анимации ходьбы.
    /// Исходники нарисованы смотрящими вправо; зеркалит рендер.
    pub(crate) fn entity_texture(&self, entity: &crate::simulation::SimEntity) -> &Handle<Image> {
        match entity.kind {
            EntityKind::Wolf => match entity.state {
                EntityState::Standing => &self.wolf_standing,
                EntityState::Lying => &self.wolf_lying,
                EntityState::Walking => &self.wolf_walk[entity.walk_frame() as usize],
            },
        }
    }

    /// Хэндлы тайлов в порядке индексов атласа: Water … Mountain.
    pub(crate) fn atlas_sources(&self) -> [&Handle<Image>; 11] {
        [
            &self.water,
            &self.sand,
            &self.dirt,
            &self.grass,
            &self.forest_grass,
            &self.stone,
            &self.clay,
            &self.farmland,
            &self.deep_water,
            &self.ocean,
            &self.mountain,
        ]
    }
}

/// Загружает текстуры при старте.
pub(crate) fn load_tile_textures(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(TileTextures::load(&assets));
}
