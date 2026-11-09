//! Игровые текстуры и атласы: единая загрузка исходников и динамическая
//! упаковка в атласы при старте.
//!
//! Все текстуры (тайлы, объекты, постройки, культуры, предметы, существа и
//! UI-иконки) грузятся здесь одним реестром `GameTextures`. Рендер мира
//! работает через `TextureAtlases`, который собирается из этого реестра,
//! когда Bevy догрузит PNG. Атласы собираются `DynamicTextureAtlasBuilder`
//! и при нехватке места пересобираются в размере ×2 — добавлять текстуры
//! можно без ручной правки координат.
//!
//! UI берёт исходные хэндлы из `GameTextures`: иконки доступны сразу, а не
//! после асинхронной сборки атласа. Исходники всё равно нужны для контуров
//! выделения и alpha-bounds существ, так что отдельной копии памяти нет.

use std::collections::HashMap;

use bevy::asset::{AssetId, LoadState, RenderAssetUsages};
use bevy::image::{
    DynamicTextureAtlasBuilder, DynamicTextureAtlasBuilderError, ImageLoaderSettings,
    ImageSampler, TextureAtlasLayout,
};
use bevy::math::{URect, UVec2};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::simulation::items::BodySlot;
use crate::simulation::{
    BuildingKind, CropKind, EntityKind, EntityState, ItemKind, ObjectKind, SimEntity, TileType,
    WorldObject, CROP_KIND_COUNT, ITEM_KIND_COUNT, MAX_CROP_STAGES,
};

/// Число слотов `ObjectKind` (природные 0–3 и постройки 4–10).
const OBJECT_KIND_SLOTS: usize = 11;

/// Число текстур стадий культур (плотная раскладка `[культура][стадия]`).
const CROP_TEXTURE_COUNT: usize = CROP_KIND_COUNT * MAX_CROP_STAGES;

/// Отступ между текстурами в атласе. Он же даёт место для расширения краёв
/// тайлов (защита от швов при nearest-фильтрации).
const ATLAS_PADDING: u32 = 2;

/// Минимальный начальный размер атласов. Дальше размер подбирается по
/// фактическому набору текстур (ближайшая степень двойки) и растёт при
/// переполнении.
const TILE_ATLAS_MIN_SIDE: u32 = 256;
const ITEM_ATLAS_MIN_SIDE: u32 = 1024;
const SPRITE_ATLAS_MIN_SIDE: u32 = 1024;

/// Технический предел одной стороны атласа (защита от бесконечного роста).
const MAX_ATLAS_SIDE: u32 = 8192;

/// Формат всех игровых PNG — RGBA8 sRGB. Другие форматы заменяются
/// прозрачным плейсхолдером с предупреждением в лог.
const ATLAS_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

// ---------------------------------------------------------------------------
// Единый реестр исходных текстур
// ---------------------------------------------------------------------------

/// Позиции кадров одного вида существ во входном списке атласа.
struct EntityFramePositions {
    standing: usize,
    lying: usize,
    walk: Vec<usize>,
}

/// Готовые списки источников и их позиции: собираются один раз при загрузке,
/// чтобы система атласов не перестраивала `Vec` каждый кадр.
struct AtlasSources {
    tiles: Vec<Handle<Image>>,
    items: Vec<Handle<Image>>,
    sprites: Vec<Handle<Image>>,
    item_positions: [usize; ITEM_KIND_COUNT],
    crop_positions: [usize; CROP_TEXTURE_COUNT],
    object_positions: [usize; OBJECT_KIND_SLOTS],
    apple_tree_empty_position: usize,
    entity_positions: [EntityFramePositions; 3],
}

/// Исходные текстуры всех категорий. Загружается один раз в `PreStartup`;
/// UI использует хэндлы напрямую, рендер мира — собранные из них атласы.
#[derive(Resource)]
pub(crate) struct GameTextures {
    /// Тайлы в порядке `TileType::from_discriminant(1..)`.
    tiles: Vec<Handle<Image>>,
    /// Природные объекты в порядке `ObjectKind::ALL`.
    natural_objects: [Handle<Image>; 4],
    /// Постройки в порядке `BuildingKind::ALL`; у каждой — один «фейс».
    buildings: [Handle<Image>; 7],
    /// Предметы по дискриминанту `ItemKind`.
    items: [Handle<Image>; ITEM_KIND_COUNT],
    /// Существа по `EntityKind as usize` (Wolf, Human, Rabbit).
    entities: [EntitySources; 3],
    /// Пустые слоты брони в порядке `ARMOR_LAYOUT`.
    empty_armor_slots: [Handle<Image>; 4],
    trash_can: Handle<Image>,
    play: Handle<Image>,
    pause: Handle<Image>,
    /// Единый список источников для сборки атласов.
    atlas_sources: AtlasSources,
}

/// Исходники одного вида существ: покой, лежание и кадры ходьбы.
struct EntitySources {
    standing: Handle<Image>,
    lying: Handle<Image>,
    walk: Vec<Handle<Image>>,
}

/// Ключ текстуры в едином реестре путей. Добавление нового элемента
/// сводится к одному match-плечу в `texture_path`, а не к правке нескольких
/// параллельных списков.
#[derive(Clone, Copy, Debug)]
enum TextureKey {
    Tile(TileType),
    /// Природный объект или постройка (`ObjectKind::Building`).
    Object(ObjectKind),
    AppleTreeEmpty,
    Crop(CropKind, u8),
    Item(ItemKind),
    Entity(EntityKind, EntitySlot),
    Ui(UiTexture),
}

/// Кадр существа в реестре путей.
#[derive(Clone, Copy, Debug)]
enum EntitySlot {
    Standing,
    Lying,
    Walk(u8),
}

/// Текстуры интерфейса, не входящие в мировые атласы.
#[derive(Clone, Copy, Debug)]
enum UiTexture {
    ArmorHelmet,
    ArmorChestplate,
    ArmorLeggings,
    ArmorBoots,
    TrashCan,
    Play,
    Pause,
}

impl GameTextures {
    /// Загружает все текстуры из `assets/textures/...`.
    fn load(assets: &AssetServer) -> Self {
        let load = |key: TextureKey| load_nearest(assets, texture_path(key));

        // Тайлы — итерацией по стабильным дискриминантам: новый вид тайла
        // не требует правки счётчика, только плеча в `texture_path`.
        let mut tiles = Vec::new();
        let mut discriminant = 1_u8;
        while let Some(kind) = TileType::from_discriminant(discriminant) {
            tiles.push(load(TextureKey::Tile(kind)));
            discriminant += 1;
        }

        let natural_objects = std::array::from_fn(|index| {
            let kind = ObjectKind::ALL[index];
            debug_assert_eq!(kind.discriminant() as usize, index);
            load(TextureKey::Object(kind))
        });
        let apple_tree_empty = load(TextureKey::AppleTreeEmpty);
        let buildings = std::array::from_fn(|index| {
            let kind = BuildingKind::ALL[index];
            debug_assert_eq!(kind as usize, index);
            load(TextureKey::Object(ObjectKind::Building(kind)))
        });

        // Стадии культур: недостающие стадии дублируют зрелую (см.
        // `crop_stage_slot`), поэтому атлас получает плотную раскладку.
        let crops: [Handle<Image>; CROP_TEXTURE_COUNT] = std::array::from_fn(|slot| {
            let kind = CropKind::ALL[slot / MAX_CROP_STAGES];
            let stage = slot % MAX_CROP_STAGES;
            let file_stage = stage.min(kind.stage_count() - 1);
            load(TextureKey::Crop(kind, file_stage as u8))
        });

        let items: [Handle<Image>; ITEM_KIND_COUNT] = std::array::from_fn(|index| {
            debug_assert_eq!(ItemKind::ALL[index] as usize, index);
            load(TextureKey::Item(ItemKind::ALL[index]))
        });

        let entities: [EntitySources; 3] = std::array::from_fn(|index| {
            let kind = EntityKind::ALL[index];
            debug_assert_eq!(kind as usize, index);
            EntitySources {
                standing: load(TextureKey::Entity(kind, EntitySlot::Standing)),
                lying: load(TextureKey::Entity(kind, EntitySlot::Lying)),
                walk: (0..kind.walk_frame_count())
                    .map(|frame| load(TextureKey::Entity(kind, EntitySlot::Walk(frame))))
                    .collect(),
            }
        });

        let empty_armor_slots = [
            load(TextureKey::Ui(UiTexture::ArmorHelmet)),
            load(TextureKey::Ui(UiTexture::ArmorChestplate)),
            load(TextureKey::Ui(UiTexture::ArmorLeggings)),
            load(TextureKey::Ui(UiTexture::ArmorBoots)),
        ];
        let trash_can = load(TextureKey::Ui(UiTexture::TrashCan));
        let play = load(TextureKey::Ui(UiTexture::Play));
        let pause = load(TextureKey::Ui(UiTexture::Pause));

        // Готовые списки источников и их позиции. Собираются один раз —
        // система атласов потом не перестраивает Vec каждый кадр.
        let mut item_sources = Vec::with_capacity(ITEM_KIND_COUNT + CROP_TEXTURE_COUNT);
        let mut item_positions = [0_usize; ITEM_KIND_COUNT];
        for kind in ItemKind::ALL {
            item_positions[kind as usize] = item_sources.len();
            item_sources.push(items[kind as usize].clone());
        }
        let mut crop_positions = [0_usize; CROP_TEXTURE_COUNT];
        for (slot, position) in crop_positions.iter_mut().enumerate() {
            *position = item_sources.len();
            item_sources.push(crops[slot].clone());
        }

        let mut sprite_sources = Vec::new();
        let mut object_positions = [0_usize; OBJECT_KIND_SLOTS];
        for kind in ObjectKind::ALL {
            object_positions[kind.discriminant() as usize] = sprite_sources.len();
            sprite_sources.push(natural_objects[kind.discriminant() as usize].clone());
        }
        let apple_tree_empty_position = sprite_sources.len();
        sprite_sources.push(apple_tree_empty.clone());
        for kind in BuildingKind::ALL {
            object_positions[ObjectKind::Building(kind).discriminant() as usize] =
                sprite_sources.len();
            sprite_sources.push(buildings[kind as usize].clone());
        }
        let mut entity_positions: [EntityFramePositions; 3] = std::array::from_fn(|_| {
            EntityFramePositions {
                standing: 0,
                lying: 0,
                walk: Vec::new(),
            }
        });
        for kind in EntityKind::ALL {
            let sources = &entities[kind as usize];
            let standing = sprite_sources.len();
            sprite_sources.push(sources.standing.clone());
            let lying = sprite_sources.len();
            sprite_sources.push(sources.lying.clone());
            let walk = sources
                .walk
                .iter()
                .map(|source| {
                    let position = sprite_sources.len();
                    sprite_sources.push(source.clone());
                    position
                })
                .collect();
            entity_positions[kind as usize] = EntityFramePositions {
                standing,
                lying,
                walk,
            };
        }

        Self {
            atlas_sources: AtlasSources {
                tiles: tiles.clone(),
                items: item_sources,
                sprites: sprite_sources,
                item_positions,
                crop_positions,
                object_positions,
                apple_tree_empty_position,
                entity_positions,
            },
            tiles,
            natural_objects,
            buildings,
            items,
            entities,
            empty_armor_slots,
            trash_can,
            play,
            pause,
        }
    }

    /// Иконка тайла рельефа (UI). `Empty` не имеет иконки; на всякий случай
    /// возвращается первый тайл, чтобы интерфейс не паниковал.
    pub(crate) fn tile(&self, kind: TileType) -> &Handle<Image> {
        &self.tiles[(kind as usize).saturating_sub(1).min(self.tiles.len() - 1)]
    }

    /// Иконка объекта (UI). Постройки используют свой «фейс».
    pub(crate) fn object(&self, kind: ObjectKind) -> &Handle<Image> {
        match kind {
            ObjectKind::Building(building) => &self.buildings[building as usize],
            ObjectKind::AppleTree => &self.natural_objects[3],
            kind => &self.natural_objects[kind.discriminant() as usize],
        }
    }

    /// Иконка предмета.
    pub(crate) fn item(&self, kind: ItemKind) -> &Handle<Image> {
        &self.items[kind as usize]
    }

    /// Иконка существа (кадр покоя).
    pub(crate) fn entity(&self, kind: EntityKind) -> &Handle<Image> {
        &self.entities[kind as usize].standing
    }

    /// Текстура пустого слота брони по части тела. `Body` (броня
    /// неразумных существ) своей текстуры не имеет.
    pub(crate) fn empty_armor_slot(&self, slot: BodySlot) -> Option<&Handle<Image>> {
        match slot {
            BodySlot::Head => Some(&self.empty_armor_slots[0]),
            BodySlot::Chest => Some(&self.empty_armor_slots[1]),
            BodySlot::Legs => Some(&self.empty_armor_slots[2]),
            BodySlot::Boots => Some(&self.empty_armor_slots[3]),
            BodySlot::Body => None,
        }
    }

    /// Текстура слота-мусорки (пустое состояние).
    pub(crate) fn trash_can(&self) -> &Handle<Image> {
        &self.trash_can
    }

    /// Иконка «play» (показать, когда мир на паузе).
    pub(crate) fn play(&self) -> &Handle<Image> {
        &self.play
    }

    /// Иконка «pause» (показать, когда мир идёт).
    pub(crate) fn pause(&self) -> &Handle<Image> {
        &self.pause
    }
}

/// Загружает все игровые текстуры до старта систем, которые их читают.
pub(crate) fn load_game_textures(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(GameTextures::load(&assets));
}

/// Загружает PNG с nearest-семплированием: исходники — пиксель-арт.
fn load_nearest(assets: &AssetServer, path: String) -> Handle<Image> {
    assets
        .load_builder()
        .with_settings::<ImageLoaderSettings>(|settings| {
            settings.sampler = ImageSampler::nearest();
        })
        .load(path)
}

// ---------------------------------------------------------------------------
// Собранные атласы
// ---------------------------------------------------------------------------

/// Вид атласа: 32px-мелочь (предметы и культуры) и 128px-спрайты.
/// Тайлы живут отдельным `Atlas` без `AtlasTexture`: terrain-меши получают
/// готовую UV-таблицу напрямую.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum AtlasKind {
    Items,
    Sprites,
}

/// Ссылка на текстуру внутри атласа.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct AtlasTexture {
    pub(crate) kind: AtlasKind,
    /// Индекс ячейки в раскладке. `u16` достаточно для всех разумных
    /// атласов и уменьшает размер ключей кэшей.
    pub(crate) index: u16,
}

/// Готовый атлас: изображение, раскладка и денормализованные данные для
/// быстрого доступа к UV/размерам без похода в `Assets`.
pub(crate) struct Atlas {
    pub(crate) image: Handle<Image>,
    pub(crate) layout: Handle<TextureAtlasLayout>,
    rects: Vec<URect>,
    /// Исходные хэндлы по индексу атласа (для контуров и alpha-bounds).
    sources: Vec<Handle<Image>>,
    size: UVec2,
}

impl Atlas {
    /// UV-прямоугольник `[u0, v0, u1, v1]`, где v0 — верх.
    fn uv(&self, index: usize) -> [f32; 4] {
        uv_from_rect(self.rects[index], self.size)
    }

}

/// UV-прямоугольник тайла внутри атласа. Чистая функция — проверяется тестом.
pub(crate) fn uv_from_rect(rect: URect, atlas_size: UVec2) -> [f32; 4] {
    [
        rect.min.x as f32 / atlas_size.x as f32,
        rect.min.y as f32 / atlas_size.y as f32,
        rect.max.x as f32 / atlas_size.x as f32,
        rect.max.y as f32 / atlas_size.y as f32,
    ]
}

/// Индексы текстур одного вида существ в атласе спрайтов.
struct EntityFrames {
    standing: u16,
    lying: u16,
    walk: Vec<u16>,
}

/// Все готовые атласы и отображение «вид → текстура». Собирается один раз
/// после загрузки исходников; далее только читается.
#[derive(Resource)]
pub(crate) struct TextureAtlases {
    pub(crate) tiles: Atlas,
    pub(crate) items: Atlas,
    pub(crate) sprites: Atlas,
    /// UV тайлов по индексу атласа (`TileType as usize - 1`).
    tile_uv: Vec<[f32; 4]>,
    item: [u16; ITEM_KIND_COUNT],
    crop: [u16; CROP_TEXTURE_COUNT],
    object: [u16; OBJECT_KIND_SLOTS],
    apple_tree_empty: u16,
    entity: [EntityFrames; 3],
    /// Уникальные текстуры существ (без дублей кадров) для alpha-скана.
    entity_unique: Vec<AtlasTexture>,
}

impl TextureAtlases {
    /// Атлас по виду.
    pub(crate) fn atlas(&self, kind: AtlasKind) -> &Atlas {
        match kind {
            AtlasKind::Items => &self.items,
            AtlasKind::Sprites => &self.sprites,
        }
    }

    /// Изображение атласа для материала/спрайта.
    pub(crate) fn image(&self, texture: AtlasTexture) -> &Handle<Image> {
        &self.atlas(texture.kind).image
    }

    /// Раскладка атласа (для 2D `Sprite::texture_atlas`).
    pub(crate) fn layout(&self, texture: AtlasTexture) -> &Handle<TextureAtlasLayout> {
        &self.atlas(texture.kind).layout
    }

    /// UV-прямоугольник ячейки `[u0, v0, u1, v1]`, где v0 — верх. Нужен
    /// для запекания UV в меш спрайта.
    pub(crate) fn uv(&self, texture: AtlasTexture) -> [f32; 4] {
        self.atlas(texture.kind).uv(texture.index as usize)
    }

    /// Исходная (полная) текстура кадра — для контуров и alpha-bounds.
    pub(crate) fn source(&self, texture: AtlasTexture) -> &Handle<Image> {
        &self.atlas(texture.kind).sources[texture.index as usize]
    }

    /// Размер исходной текстуры в пикселях (равен размеру ячейки атласа).
    pub(crate) fn source_size(&self, texture: AtlasTexture) -> UVec2 {
        let rect = self.atlas(texture.kind).rects[texture.index as usize];
        rect.max - rect.min
    }

    /// UV тайлов по индексу атласа для сборки terrain-мешей.
    pub(crate) fn tile_uv(&self) -> &[[f32; 4]] {
        &self.tile_uv
    }

    /// Текстура объекта; пустая яблоня имеет отдельный кадр.
    pub(crate) fn object(&self, object: &WorldObject) -> AtlasTexture {
        match object.kind {
            ObjectKind::AppleTree if object.charge == 0 => AtlasTexture {
                kind: AtlasKind::Sprites,
                index: self.apple_tree_empty,
            },
            kind => self.object_kind(kind),
        }
    }

    /// Текстура вида объекта (яблоня — плодоносящая).
    pub(crate) fn object_kind(&self, kind: ObjectKind) -> AtlasTexture {
        AtlasTexture {
            kind: AtlasKind::Sprites,
            index: self.object[kind.discriminant() as usize],
        }
    }

    /// Крест костра.
    pub(crate) fn campfire_cross(&self) -> AtlasTexture {
        self.object_kind(ObjectKind::Building(BuildingKind::Campfire))
    }

    /// Плоская декаль склада.
    pub(crate) fn storage_flat(&self) -> AtlasTexture {
        self.object_kind(ObjectKind::Building(BuildingKind::Storage))
    }

    /// Стадия культуры по плотной раскладке `crop_stage_slot`.
    pub(crate) fn crop(&self, kind: CropKind, stage: u8) -> AtlasTexture {
        let index = self.crop[crop_stage_slot(kind, stage)];
        AtlasTexture {
            kind: AtlasKind::Items,
            index,
        }
    }

    /// Текстура предмета.
    pub(crate) fn item(&self, kind: ItemKind) -> AtlasTexture {
        AtlasTexture {
            kind: AtlasKind::Items,
            index: self.item[kind as usize],
        }
    }

    /// Текстура существа по виду, состоянию и кадру анимации.
    pub(crate) fn entity(&self, entity: &SimEntity) -> AtlasTexture {
        let frames = &self.entity[entity.kind as usize];
        let index = match entity.state {
            EntityState::Standing => frames.standing,
            EntityState::Lying => frames.lying,
            EntityState::Walking => {
                let frame = entity.walk_frame() as usize;
                frames.walk[frame.min(frames.walk.len() - 1)]
            }
        };
        AtlasTexture {
            kind: AtlasKind::Sprites,
            index,
        }
    }

    /// Уникальные текстуры существ: alpha-скан и контуры должны покрыть
    /// каждый кадр ровно один раз.
    pub(crate) fn entity_textures(&self) -> &[AtlasTexture] {
        &self.entity_unique
    }
}

// ---------------------------------------------------------------------------
// Сборка атласов
// ---------------------------------------------------------------------------

/// Собирает атласы, когда все исходные PNG загружены. Работает один раз:
/// после вставки `TextureAtlases` система сразу выходит.
pub(crate) fn build_texture_atlases(
    mut commands: Commands,
    textures: Res<GameTextures>,
    mut images: ResMut<Assets<Image>>,
    server: Res<AssetServer>,
    mut layouts: ResMut<Assets<TextureAtlasLayout>>,
    existing: Option<Res<TextureAtlases>>,
    mut fatal: Local<bool>,
) {
    if existing.is_some() || *fatal {
        return;
    }

    // Списки источников и их позиции собраны один раз при загрузке.
    let sources = &textures.atlas_sources;

    // 1. Атлас тайлов (32×32).
    let Some(tiles) = build_atlas(
        &images,
        &server,
        &sources.tiles,
        UVec2::splat(32),
        TILE_ATLAS_MIN_SIDE,
        true,
    ) else {
        return;
    };
    let Ok(tiles) = tiles else {
        error!("не удалось собрать атлас тайлов");
        *fatal = true;
        return;
    };

    // 2. Атлас мелочи: предметы + стадии культур (все 32×32).
    let Some(items) = build_atlas(
        &images,
        &server,
        &sources.items,
        UVec2::splat(32),
        ITEM_ATLAS_MIN_SIDE,
        false,
    ) else {
        return;
    };
    let Ok(items) = items else {
        error!("не удалось собрать атлас предметов");
        *fatal = true;
        return;
    };

    // 3. Общий атлас 128px-спрайтов: объекты, постройки, существа.
    let Some(sprites) = build_atlas(
        &images,
        &server,
        &sources.sprites,
        UVec2::splat(128),
        SPRITE_ATLAS_MIN_SIDE,
        false,
    ) else {
        return;
    };
    let Ok(sprites) = sprites else {
        error!("не удалось собрать атлас спрайтов");
        *fatal = true;
        return;
    };

    // 4. Фиксируем изображения и раскладки как ассеты.
    let tiles = store_atlas(tiles, &mut images, &mut layouts);
    let items = store_atlas(items, &mut images, &mut layouts);
    let sprites = store_atlas(sprites, &mut images, &mut layouts);

    // 5. Строим отображения «вид → текстура». Индексы в атласе стабильны:
    // они выдаются в порядке добавления и не меняются при росте.
    let tile_uv = tiles
        .atlas
        .rects
        .iter()
        .map(|rect| uv_from_rect(*rect, tiles.atlas.size))
        .collect();
    let item = std::array::from_fn(|index| items.indices[sources.item_positions[index]]);
    let crop = std::array::from_fn(|index| items.indices[sources.crop_positions[index]]);
    let object = std::array::from_fn(|index| sprites.indices[sources.object_positions[index]]);
    let apple_tree_empty = sprites.indices[sources.apple_tree_empty_position];
    let entity: [EntityFrames; 3] = std::array::from_fn(|index| {
        let positions = &sources.entity_positions[index];
        EntityFrames {
            standing: sprites.indices[positions.standing],
            lying: sprites.indices[positions.lying],
            walk: positions
                .walk
                .iter()
                .map(|&position| sprites.indices[position])
                .collect(),
        }
    });
    let mut entity_unique = Vec::new();
    for frames in &entity {
        for index in [frames.standing, frames.lying]
            .into_iter()
            .chain(frames.walk.iter().copied())
        {
            let texture = AtlasTexture {
                kind: AtlasKind::Sprites,
                index,
            };
            if !entity_unique.contains(&texture) {
                entity_unique.push(texture);
            }
        }
    }

    commands.insert_resource(TextureAtlases {
        tiles: tiles.atlas,
        items: items.atlas,
        sprites: sprites.atlas,
        tile_uv,
        item,
        crop,
        object,
        apple_tree_empty,
        entity,
        entity_unique,
    });
}

/// Результат сборки одного атласа вместе с отображением входных позиций.
struct BuiltAtlas {
    atlas: Atlas,
    /// Для каждой входной текстуры — её индекс в атласе (с учётом дублей).
    indices: Vec<u16>,
}

/// Временная структура: собранные изображение и раскладка ещё не в ассетах.
struct RawAtlas {
    image: Image,
    layout: TextureAtlasLayout,
    sources: Vec<Handle<Image>>,
    indices: Vec<u16>,
}

/// Кладёт собранный атлас в `Assets` и строит быстрый `Atlas`.
fn store_atlas(
    raw: RawAtlas,
    images: &mut Assets<Image>,
    layouts: &mut Assets<TextureAtlasLayout>,
) -> BuiltAtlas {
    let size = raw.layout.size;
    let rects = raw.layout.textures.clone();
    let image = images.add(raw.image);
    let layout = layouts.add(raw.layout);
    BuiltAtlas {
        atlas: Atlas {
            image,
            layout,
            rects,
            sources: raw.sources,
            size,
        },
        indices: raw.indices,
    }
}

/// Собирает атлас из исходников. `None` — не все PNG загрузились, `Err` —
/// атлас невозможно уложить в технический предел.
fn build_atlas(
    images: &Assets<Image>,
    server: &AssetServer,
    sources: &[Handle<Image>],
    placeholder_size: UVec2,
    min_side: u32,
    extend_tile_edges: bool,
) -> Option<Result<RawAtlas, ()>> {
    // Ждём загрузки: незагруженный ассет — не ошибка, попробуем в следующем
    // кадре. Окончательно упавший PNG заменяем прозрачным плейсхолдером,
    // чтобы опечатка в пути не блокировала весь мир.
    let mut slots = Vec::with_capacity(sources.len());
    for handle in sources {
        if let Some(image) = images.get(handle) {
            if image.texture_descriptor.format == ATLAS_FORMAT {
                slots.push(SourceSlot::Loaded(image));
            } else {
                warn!(
                    "текстура {:?} имеет формат {:?}, ожидается RGBA8; используется плейсхолдер",
                    handle.id(),
                    image.texture_descriptor.format
                );
                slots.push(SourceSlot::Placeholder);
            }
        } else if let LoadState::Failed(error) = server.load_state(handle.id()) {
            warn!(
                "не удалось загрузить текстуру {:?} ({:?}): {error}",
                handle.id(),
                server.get_path(handle.id())
            );
            slots.push(SourceSlot::Placeholder);
        } else {
            return None;
        }
    }

    let placeholder = placeholder_image(placeholder_size);
    let mut size = minimal_side(&slots, &placeholder, ATLAS_PADDING, min_side);
    loop {
        if size.x > MAX_ATLAS_SIDE || size.y > MAX_ATLAS_SIDE {
            return Some(Err(()));
        }
        let mut image = Image::new_fill(
            Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 0],
            ATLAS_FORMAT,
            RenderAssetUsages::default(),
        );
        image.sampler = ImageSampler::nearest();
        let mut layout = TextureAtlasLayout::new_empty(size);
        let mut builder = DynamicTextureAtlasBuilder::new(size, ATLAS_PADDING);
        let mut seen: HashMap<AssetId<Image>, u16> = HashMap::with_capacity(sources.len());
        let mut unique_sources = Vec::with_capacity(sources.len());
        let mut indices = Vec::with_capacity(sources.len());
        let mut grew = false;

        for (handle, slot) in sources.iter().zip(&slots) {
            if let Some(&index) = seen.get(&handle.id()) {
                indices.push(index);
                continue;
            }
            let source = slot.image(&placeholder);
            match builder.add_texture(&mut layout, source, &mut image) {
                Ok(index) => {
                    debug_assert_eq!(index, unique_sources.len());
                    let index = u16::try_from(index).expect("в атласе больше 65535 ячеек");
                    seen.insert(handle.id(), index);
                    unique_sources.push(handle.clone());
                    indices.push(index);
                }
                Err(DynamicTextureAtlasBuilderError::FailedToAllocateSpace) => {
                    grew = true;
                    break;
                }
                Err(error) => {
                    warn!("ошибка упаковки атласа: {error}");
                    return Some(Err(()));
                }
            }
        }

        if grew {
            size *= 2;
            continue;
        }

        // У тайлов расширяем края на 1 px в отступы: nearest-фильтрация
        // на границе ячейки иначе может взять прозрачный пиксель и дать шов.
        if extend_tile_edges {
            extend_edges(&mut image, &layout.textures, ATLAS_PADDING);
        }
        // CPU-копия атласа после загрузки в GPU не нужна.
        image.asset_usage = RenderAssetUsages::RENDER_WORLD;

        return Some(Ok(RawAtlas {
            image,
            layout,
            sources: unique_sources,
            indices,
        }));
    }
}

/// Начальный размер атласа: ближайшая степень двойки, вмещающая все
/// источники с отступами; не меньше `min_side`.
fn minimal_side(
    slots: &[impl AtlasSlot],
    placeholder: &Image,
    padding: u32,
    min_side: u32,
) -> UVec2 {
    let mut area = 0_u64;
    let mut max_side = 1_u32;
    for slot in slots {
        let image = slot.image(placeholder);
        let width = image.width() + padding;
        let height = image.height() + padding;
        area += width as u64 * height as u64;
        max_side = max_side.max(width).max(height);
    }
    let square = (area as f64).sqrt().ceil() as u32;
    let side = max_side.max(square).next_power_of_two().max(min_side);
    UVec2::splat(side.min(MAX_ATLAS_SIDE))
}

/// Доступ к изображению источника (загруженному или плейсхолдеру).
trait AtlasSlot {
    fn image<'a>(&'a self, placeholder: &'a Image) -> &'a Image;
}

impl<'a> AtlasSlot for SourceSlot<'a> {
    fn image<'b>(&'b self, placeholder: &'b Image) -> &'b Image {
        match self {
            SourceSlot::Loaded(image) => image,
            SourceSlot::Placeholder => placeholder,
        }
    }
}

/// Источник для оценки размера: загруженный или плейсхолдер.
enum SourceSlot<'a> {
    Loaded(&'a Image),
    Placeholder,
}

/// Прозрачная текстура-заглушка нужного размера.
fn placeholder_image(size: UVec2) -> Image {
    Image::new_fill(
        Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        ATLAS_FORMAT,
        RenderAssetUsages::default(),
    )
}

/// Расширяет края каждой ячейки на 1 пиксель в отступы атласа. Без этого
/// nearest-фильтрация на границе тайла может взять прозрачный пиксель
/// (шов между тайлами). Безопасно при `padding >= 2`.
fn extend_edges(image: &mut Image, rects: &[URect], padding: u32) {
    let radius = (padding / 2) as i32;
    if radius == 0 {
        return;
    }
    let width = image.width() as i32;
    let height = image.height() as i32;
    let Some(data) = image.data.as_deref_mut() else {
        return;
    };
    // Копируем исходные ячейки, чтобы расширение одной не портило соседнюю.
    let original = data.to_vec();
    let pixel = |x: i32, y: i32| -> [u8; 4] {
        let offset = ((y.clamp(0, height - 1) * width + x.clamp(0, width - 1)) * 4) as usize;
        [
            original[offset],
            original[offset + 1],
            original[offset + 2],
            original[offset + 3],
        ]
    };
    for rect in rects {
        let x0 = rect.min.x as i32;
        let y0 = rect.min.y as i32;
        let x1 = rect.max.x as i32;
        let y1 = rect.max.y as i32;
        for y in y0 - radius..y1 + radius {
            for x in x0 - radius..x1 + radius {
                if x >= x0 && x < x1 && y >= y0 && y < y1 {
                    continue;
                }
                if x < 0 || y < 0 || x >= width || y >= height {
                    continue;
                }
                let color = pixel(x.clamp(x0, x1 - 1), y.clamp(y0, y1 - 1));
                let offset = ((y * width + x) * 4) as usize;
                data[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Файлы текстур
// ---------------------------------------------------------------------------

/// Единая таблица «вид → путь текстуры». Новый элемент (тайл, объект,
/// постройка, предмет, стадия культуры, кадр существа, UI-иконка) требует
/// ровно одного плеча здесь.
fn texture_path(key: TextureKey) -> String {
    // Кадры анимации ходьбы: порядок соответствует `walk_frame_count` и
    // `SimEntity::walk_frame()`. Человек временно использует один кадр.
    const WOLF_WALK: [&str; 2] = [
        "textures/entities/wolf/wolf_walk1.png",
        "textures/entities/wolf/wolf_walk2.png",
    ];
    const RABBIT_WALK: [&str; 3] = [
        "textures/entities/rabbit/rabbit_jump_2.png",
        "textures/entities/rabbit/rabbit_standing.png",
        "textures/entities/rabbit/rabbit_jump.png",
    ];

    if let TextureKey::Crop(kind, stage) = key {
        return format!("textures/crops/{}_stage{stage}.png", crop_base(kind));
    }

    let path = match key {
        TextureKey::Tile(kind) => match kind {
            TileType::Empty | TileType::Water => "textures/tiles/water.png",
            TileType::Sand => "textures/tiles/sand.png",
            TileType::Dirt => "textures/tiles/dirt.png",
            TileType::Grass => "textures/tiles/grass.png",
            TileType::ForestGrass => "textures/tiles/forest_grass.png",
            TileType::Stone => "textures/tiles/stone.png",
            TileType::Clay => "textures/tiles/clay.png",
            TileType::Farmland => "textures/tiles/farmland.png",
            TileType::DeepWater => "textures/tiles/deep_water.png",
            TileType::Ocean => "textures/tiles/ocean.png",
            TileType::Mountain => "textures/tiles/mountain.png",
            TileType::Moss => "textures/tiles/moss.png",
            TileType::Mud => "textures/tiles/mud.png",
            TileType::Podzol => "textures/tiles/podzol.png",
        },
        TextureKey::Object(kind) => match kind {
            ObjectKind::Tree => "textures/objects/tree.png",
            ObjectKind::Bush => "textures/objects/bush.png",
            ObjectKind::Rock => "textures/objects/rock.png",
            ObjectKind::AppleTree => "textures/objects/apple_tree.png",
            ObjectKind::Building(kind) => match kind {
                BuildingKind::Campfire => "textures/buildings/campfire_cross.png",
                BuildingKind::CraftingTable => "textures/buildings/crafting_table_side.png",
                BuildingKind::Furnace => "textures/buildings/furnace_front.png",
                BuildingKind::Barrel => "textures/buildings/barrel_side.png",
                BuildingKind::BlastFurnace => "textures/buildings/blast_furnace_front.png",
                BuildingKind::Storage => "textures/buildings/storage.png",
                BuildingKind::SmithingTable => "textures/buildings/smithing_table_front.png",
            },
        },
        TextureKey::AppleTreeEmpty => "textures/objects/apple_tree_empty.png",
        TextureKey::Crop(..) => unreachable!("культуры обработаны выше"),
        TextureKey::Item(kind) => ITEM_FILES[kind as usize],
        TextureKey::Entity(kind, slot) => match (kind, slot) {
            (EntityKind::Wolf, EntitySlot::Standing) => {
                "textures/entities/wolf/wolf_standing.png"
            }
            (EntityKind::Wolf, EntitySlot::Lying) => "textures/entities/wolf/wolf_lying.png",
            (EntityKind::Wolf, EntitySlot::Walk(frame)) => {
                WOLF_WALK[(frame as usize).min(WOLF_WALK.len() - 1)]
            }
            // Человек временно рисуется одним кадром (`human.png`) во всех
            // состояниях; отдельного спрайта игрока больше нет.
            (EntityKind::Human, _) => "textures/entities/human/human.png",
            (EntityKind::Rabbit, EntitySlot::Standing) => {
                "textures/entities/rabbit/rabbit_sit.png"
            }
            (EntityKind::Rabbit, EntitySlot::Lying) => {
                "textures/entities/rabbit/rabbit_lying.png"
            }
            (EntityKind::Rabbit, EntitySlot::Walk(frame)) => {
                RABBIT_WALK[(frame as usize).min(RABBIT_WALK.len() - 1)]
            }
        },
        TextureKey::Ui(ui) => match ui {
            UiTexture::ArmorHelmet => "textures/ui/inventory/empty_armor_slot_helmet.png",
            UiTexture::ArmorChestplate => {
                "textures/ui/inventory/empty_armor_slot_chestplate.png"
            }
            UiTexture::ArmorLeggings => {
                "textures/ui/inventory/empty_armor_slot_leggings.png"
            }
            UiTexture::ArmorBoots => "textures/ui/inventory/empty_armor_slot_boots.png",
            UiTexture::TrashCan => "textures/ui/inventory/trash_can.png",
            UiTexture::Play => "textures/ui/play.png",
            UiTexture::Pause => "textures/ui/pause.png",
        },
    };
    path.to_string()
}

/// Базовое имя файла стадий культуры.
fn crop_base(kind: CropKind) -> &'static str {
    match kind {
        CropKind::Wheat => "wheat",
        CropKind::Beetroot => "beetroots",
        CropKind::Carrot => "carrots",
        CropKind::Potato => "potatoes",
        CropKind::Cherry => "cherry",
    }
}

/// Смещение стадии культуры в плотной раскладке текстур. Чистая функция —
/// без графики проверяется тестом.
pub(crate) fn crop_stage_slot(kind: CropKind, stage: u8) -> usize {
    let stage = (stage as usize).min(kind.stage_count() - 1);
    kind as usize * MAX_CROP_STAGES + stage
}

/// Пути предметов в порядке `ItemKind::ALL` (91 вид).
const ITEM_FILES: [&str; ITEM_KIND_COUNT] = [
    "textures/items/stick.png",
    "textures/items/rock.png",
    "textures/items/food/apple.png",
    "textures/items/grass_bunch_raw.png",
    "textures/items/plant_fiber.png",
    "textures/items/cordage_fiber.png",
    "textures/items/tools/axe_primal.png",
    "textures/items/tools/pickaxe_primal.png",
    "textures/items/tools/hoe_primal.png",
    "textures/items/tools/shovel_primal.png",
    "textures/items/weapon/spear_primal.png",
    "textures/items/tools/knife_stone.png",
    "textures/items/tools/fishing_rod_primal.png",
    "textures/items/weapon/arrow.png",
    "textures/items/weapon/bow_wood.png",
    "textures/items/armor/helmet_chainmail.png",
    "textures/items/armor/chestplate_chainmail.png",
    "textures/items/armor/leggings_chainmail.png",
    "textures/items/armor/boots_chainmail.png",
    "textures/items/bone.png",
    "textures/items/shard/shard_bone.png",
    "textures/items/arrow_fletching.png",
    "textures/items/feather.png",
    "textures/items/string.png",
    "textures/items/stick_bundle.png",
    "textures/items/book.png",
    "textures/items/paper.png",
    "textures/items/paper_pulp.png",
    "textures/items/clods/clod_clay.png",
    "textures/items/ceramics/brick_clay_unfired.png",
    "textures/items/ceramics/brick_clay_baked.png",
    "textures/items/seeds/seeds_wheat.png",
    "textures/items/wheat.png",
    "textures/items/tools/hammer_stone.png",
    "textures/items/nugget/nugget_copper.png",
    "textures/items/nugget/nugget_gold.png",
    "textures/items/nugget/nugget_silver.png",
    "textures/items/ingot/ingot_copper.png",
    "textures/items/ingot/ingot_bronze.png",
    "textures/items/ingot/ingot_tin.png",
    "textures/items/ingot/ingot_gold.png",
    "textures/items/ingot/ingot_silver.png",
    "textures/items/gem/gem_quartz.png",
    "textures/items/gem/gem_flint.png",
    "textures/items/gem/gem_emerald.png",
    "textures/items/gem/gem_ruby.png",
    "textures/items/dust/dust_bronze.png",
    "textures/items/dust/dust_charcoal.png",
    "textures/items/dust/dust_copper.png",
    "textures/items/dust/dust_flour.png",
    "textures/items/dust/dust_gold.png",
    "textures/items/dust/dust_ash.png",
    "textures/items/dust/dust_silver.png",
    "textures/items/dust/dust_sugar.png",
    "textures/items/dust/dust_sulfur.png",
    "textures/items/dust/dust_tin.png",
    "textures/items/dust/dust_wood.png",
    "textures/items/food/apple_baked.png",
    "textures/items/food/carrot.png",
    "textures/items/food/carrot_baked.png",
    "textures/items/food/egg.png",
    "textures/items/food/egg_roasted.png",
    "textures/items/food/lard.png",
    "textures/items/food/meat_bird_raw.png",
    "textures/items/food/meat_bird_cooked.png",
    "textures/items/food/meat_herbivore_raw.png",
    "textures/items/food/meat_herbivore_cooked.png",
    "textures/items/food/meat_predator_raw.png",
    "textures/items/food/meat_predator_cooked.png",
    "textures/items/food/potato.png",
    "textures/items/food/potato_baked.png",
    "textures/items/food/strange_tuber.png",
    "textures/items/raw_ore/raw_copper.png",
    "textures/items/raw_ore/raw_gold.png",
    "textures/items/raw_ore/raw_silver.png",
    "textures/items/raw_ore/raw_tin.png",
    "textures/items/tools/axe_copper.png",
    "textures/items/tools/hammer_copper.png",
    "textures/items/tools/hoe_copper.png",
    "textures/items/tools/pickaxe_copper.png",
    "textures/items/tools/saw_copper.png",
    "textures/items/tools/shears_copper.png",
    "textures/items/tools/shovel_copper.png",
    "textures/items/tools/sickle_copper.png",
    "textures/items/seeds/seeds_beetroot.png",
    "textures/items/food/beetroot.png",
    "textures/items/food/beetroot_roasted.png",
    "textures/items/food/cherry.png",
    "textures/items/gem/gem_coal.png",
    "textures/items/gem/gem_charcoal.png",
    "textures/items/torch.png",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// UV считается в долях атласа, не выходит за [0, 1] и не вырожден.
    #[test]
    fn uv_from_rect_stays_inside_atlas() {
        let size = UVec2::new(256, 256);
        let rect = URect::new(34, 68, 66, 100);
        let [u0, v0, u1, v1] = uv_from_rect(rect, size);
        assert!(0.0 <= u0 && u0 < u1 && u1 <= 1.0);
        assert!(0.0 <= v0 && v0 < v1 && v1 <= 1.0);
        assert!((u0 - 34.0 / 256.0).abs() < 1e-6);
        assert!((v1 - 100.0 / 256.0).abs() < 1e-6);
    }

    /// Смещение стадии зажимается по числу существующих стадий культуры.
    #[test]
    fn crop_stage_slot_clamps_to_last_stage() {
        assert_eq!(crop_stage_slot(CropKind::Wheat, 7), 7);
        assert_eq!(crop_stage_slot(CropKind::Wheat, 99), 7);
        assert_eq!(
            crop_stage_slot(CropKind::Carrot, 99),
            2 * MAX_CROP_STAGES + 3
        );
    }
}

