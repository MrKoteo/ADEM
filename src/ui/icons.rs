//! Хэндлы текстур UI: иконки тайлов, объектов и предметов
//! (нижняя панель, инвентарь), а также иконки play/pause.
//! Игровые иконки грузятся с nearest-семплированием: исходники —
//! пиксель-арт, при растяжении они не должны размываться. Иконки
//! управления — обычные картинки, им оставлен линейный фильтр.

use bevy::image::{ImageLoaderSettings, ImageSampler};
use bevy::prelude::*;

use crate::simulation::items::BodySlot;
use crate::simulation::{EntityKind, ItemKind, ObjectKind, TileType, ITEM_KIND_COUNT};

/// Число видов тайлов с иконками (Water..Mountain; Empty в UI не нужен).
const TILE_ICON_COUNT: usize = 11;

/// Число специализаций брони разумного существа с текстурой пустого слота.
const EMPTY_ARMOR_SLOT_COUNT: usize = 4;

/// Иконки UI. Индексация: тайлы — `TileType as u8 - 1`, объекты и
/// предметы — `as usize` дискриминанта.
#[derive(Resource)]
pub struct UiIcons {
    tiles: [Handle<Image>; TILE_ICON_COUNT],
    /// По дискриминанту `ObjectKind` (включая постройки).
    objects: [Handle<Image>; 5],
    items: [Handle<Image>; ITEM_KIND_COUNT],
    entities: [Handle<Image>; 1],
    /// Текстуры пустых слотов брони в порядке `ARMOR_LAYOUT`
    /// (head, chest, legs, boots).
    empty_armor_slots: [Handle<Image>; EMPTY_ARMOR_SLOT_COUNT],
    /// Текстура слота-мусорки в окне инвентаря (видна, когда слот пуст).
    trash_can: Handle<Image>,
    /// «Play»: мир стоит на паузе, клик запустит симуляцию.
    play: Handle<Image>,
    /// «Pause»: мир идёт, клик поставит на паузу.
    pause: Handle<Image>,
}

impl UiIcons {
    /// Загружает все иконки из `assets/textures/...`.
    /// Пути — 'static: у `LoadBuilder::load` строка должна жить вечно,
    /// литералы это дают.
    pub fn load(assets: &AssetServer) -> Self {
        let nearest = |path: &'static str| {
            assets
                .load_builder()
                .with_settings::<ImageLoaderSettings>(|settings| {
                    settings.sampler = ImageSampler::nearest();
                })
                .load(path)
        };
        Self {
            tiles: [
                nearest("textures/tiles/water.png"),
                nearest("textures/tiles/sand.png"),
                nearest("textures/tiles/dirt.png"),
                nearest("textures/tiles/grass.png"),
                nearest("textures/tiles/forest_grass.png"),
                nearest("textures/tiles/stone.png"),
                nearest("textures/tiles/clay.png"),
                nearest("textures/tiles/farmland.png"),
                nearest("textures/tiles/deep_water.png"),
                nearest("textures/tiles/ocean.png"),
                nearest("textures/tiles/mountain.png"),
            ],
            objects: [
                nearest("textures/objects/tree.png"),
                nearest("textures/objects/bush.png"),
                nearest("textures/objects/rock.png"),
                nearest("textures/objects/apple_tree.png"),
                nearest("textures/buildings/campfire.png"),
            ],
            items: [
                nearest("textures/items/stick.png"),
                nearest("textures/items/rock.png"),
                nearest("textures/items/food/apple.png"),
                nearest("textures/items/grass_bunch_raw.png"),
                nearest("textures/items/plant_fiber.png"),
                nearest("textures/items/cordage_fiber.png"),
                nearest("textures/items/tools/axe_primal.png"),
                nearest("textures/items/tools/pickaxe_primal.png"),
                nearest("textures/items/tools/hoe_primal.png"),
                nearest("textures/items/tools/shovel_primal.png"),
                nearest("textures/items/weapon/spear_primal.png"),
                nearest("textures/items/tools/knife_stone.png"),
                nearest("textures/items/tools/fishing_rod_primal.png"),
                nearest("textures/items/weapon/arrow.png"),
                nearest("textures/items/weapon/bow_wood.png"),
                nearest("textures/items/armor/helmet_chainmail.png"),
                nearest("textures/items/armor/chestplate_chainmail.png"),
                nearest("textures/items/armor/leggings_chainmail.png"),
                nearest("textures/items/armor/boots_chainmail.png"),
            ],
            entities: [nearest("textures/entities/wolf/wolf_standing.png")],
            empty_armor_slots: [
                nearest("textures/ui/inventory/empty_armor_slot_helmet.png"),
                nearest("textures/ui/inventory/empty_armor_slot_chestplate.png"),
                nearest("textures/ui/inventory/empty_armor_slot_leggings.png"),
                nearest("textures/ui/inventory/empty_armor_slot_boots.png"),
            ],
            trash_can: nearest("textures/ui/inventory/trash_can.png"),
            play: assets.load("textures/ui/play.png"),
            pause: assets.load("textures/ui/pause.png"),
        }
    }

    /// Иконка тайла рельефа.
    pub fn tile(&self, kind: TileType) -> &Handle<Image> {
        &self.tiles[(kind as u8 - 1) as usize]
    }

    /// Иконка объекта.
    pub fn object(&self, kind: ObjectKind) -> &Handle<Image> {
        &self.objects[kind as usize]
    }

    /// Иконка предмета.
    pub fn item(&self, kind: ItemKind) -> &Handle<Image> {
        &self.items[kind as usize]
    }

    /// Иконка существа.
    pub fn entity(&self, kind: EntityKind) -> &Handle<Image> {
        &self.entities[kind as usize]
    }

    /// Текстура пустого слота брони по части тела. `Body` (броня
    /// неразумных существ) своей текстуры не имеет.
    pub fn empty_armor_slot(&self, slot: BodySlot) -> Option<&Handle<Image>> {
        match slot {
            BodySlot::Head => Some(&self.empty_armor_slots[0]),
            BodySlot::Chest => Some(&self.empty_armor_slots[1]),
            BodySlot::Legs => Some(&self.empty_armor_slots[2]),
            BodySlot::Boots => Some(&self.empty_armor_slots[3]),
            BodySlot::Body => None,
        }
    }

    /// Текстура слота-мусорки (пустое состояние).
    pub fn trash_can(&self) -> &Handle<Image> {
        &self.trash_can
    }

    /// Иконка «play» (показать, когда мир на паузе).
    pub fn play(&self) -> &Handle<Image> {
        &self.play
    }

    /// Иконка «pause» (показать, когда мир идёт).
    pub fn pause(&self) -> &Handle<Image> {
        &self.pause
    }
}
