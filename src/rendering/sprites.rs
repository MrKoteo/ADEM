//! Спрайты мира: объекты/постройки/культуры/предметы на тайлах — дети
//! terrain-блоков (их спавнит `terrain.rs` при постройке блока), дроп-сущности
//! и существа — самостоятельные camera-facing квады, контуры выделения,
//! вспышки урона и счётчики предметов. Общие ассеты (квад, материалы,
//! контуры) кэшируются в `WorldSpriteAssets`.

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::app::state::{EditorSelection, GameMode, GameState, TickAccumulator, interpolate_pos};
use crate::simulation::{BuildingKind, ChunkPos, EntityKind, Facing, ObjectKind, TilePos, World, WorldPos, CHUNK_SIZE, TILE_SIZE};

use super::isometric::{WORLD_TILE_SIZE, world_position};
use super::terrain::{BLOCK_TILES, BlockPos, lod_for_scale};
use super::textures::{AtlasKind, AtlasTexture, TextureAtlases};

/// Метка динамического спрайта существа (животного, человека).
#[derive(Component)]
pub(crate) struct EntitySprite;

/// Метка счётчика количества у лежащего предмета. Видимость пересчитывается
/// каждый кадр по зуму: на крупном LOD текста нет, а внутри LOD 0 он
/// скрывается, когда становится мельче `ITEM_COUNT_MIN_SCREEN_PX`.
#[derive(Component)]
pub(crate) struct ItemCountLabel;

/// Соответствие «id существа симуляции → Bevy-сущность спрайта».
/// Спрайты создаются только для видимых существ и удаляются, когда
/// существо уходит с экрана. `textures` хранит текстуру текущего
/// материала: она меняется только со сменой состояния/кадра анимации,
/// поэтому `MeshMaterial3d` пере-инсертится лишь тогда.
#[derive(Resource, Default)]
pub(crate) struct EntitySprites {
    map: HashMap<u64, Entity>,
    textures: HashMap<u64, AtlasTexture>,
}

/// Соответствие «id item-сущности дропа → (спрайт, последняя позиция)».
/// Лежащий дроп статичен, брошенный летит — позиция спрайта обновляется
/// при изменении координат сущности.
#[derive(Resource, Default)]
pub(crate) struct ItemEntitySprites {
    map: HashMap<u64, (Entity, [f32; 2])>,
}

/// Общий quad и материалы для world-space спрайтов с depth test.
/// `object_sizes` кэширует размеры спрайтов по текстуре: чтение размеров
/// из `Assets<Image>` на каждый спавн заменяется lookup'ом. Кэшируется
/// только загруженное изображение — до загрузки размер считается заново.
#[derive(Resource, Default)]
pub(crate) struct WorldSpriteAssets {
    /// Единичный квад для контуров (у контура своя текстура-канвас).
    quad: Option<Handle<Mesh>>,
    /// Меши спрайтов с запечёнными UV ячейки атласа. Разные меши с одним
    /// материалом Bevy может рисовать одним batch set (multi-draw).
    meshes: HashMap<AtlasTexture, Handle<Mesh>>,
    /// Общие материалы на атлас: UV уже в мешах, поэтому материал не
    /// зависит от конкретной текстуры. `bool` — красная подсветка урона.
    materials: HashMap<(AtlasKind, bool), Handle<StandardMaterial>>,
    /// Кэш контуров выделения: материал + пад-факторы квада по исходной
    /// текстуре (одна запись вместо двух согласованных кэшей).
    pub(crate) outlines: HashMap<AtlasTexture, OutlineAsset>,
    /// Кэш материалов голограммы: (атлас, зелёная/красная сторона).
    pub(crate) hologram_materials: HashMap<(AtlasKind, bool), Handle<StandardMaterial>>,
}

/// Контур выделения для одной текстуры: материал белого дилатированного
/// силуэта и пад-факторы квада — во сколько раз канвас контура шире/выше
/// исходной текстуры. Внутренняя область канваса при таком масштабе ложится
/// на спрайт один в один, кольцо выходит ровно на радиус дилатации.
#[derive(Clone)]
pub(crate) struct OutlineAsset {
    pub(crate) material: Handle<StandardMaterial>,
    pub(crate) pad: Vec2,
}

/// Сколько секунд после удара существо подсвечивается красным.
const HURT_FLASH_SECS: f32 = 0.33;
/// Подсветка урона: красный поверх текстуры с 60% альфы.
const HURT_TINT: Color = Color::srgba(1.0, 0.0, 0.0, 0.6);

/// Вспышки урона существ: предыдущие HP (для детекта удара), таймеры
/// подсветки и overlay-спрайты «краснее» поверх основной текстуры.
#[derive(Resource, Default)]
pub(crate) struct HurtFlashes {
    prev_hp: HashMap<u64, f32>,
    timers: HashMap<u64, f32>,
    overlays: HashMap<u64, Entity>,
}

/// Контур выделения существа в редакторе (белая рамка): соответствие
/// «id существа → Bevy-сущность квадрата-контура». Контуры живут, пока
/// сущность подсвечена (`EditorSelection`) и видна.
#[derive(Resource, Default)]
pub(crate) struct SelectionOutlines {
    map: HashMap<u64, Entity>,
    /// Текстура, из которой собран материал контура. Смена состояния/кадра
    /// анимации меняет текстуру спрайта — без трекинга силуэт оставался бы
    /// от прежнего состояния и рассинхронизировался с целью.
    textures: HashMap<u64, AtlasTexture>,
}

/// Толщина белого контура выделения в мировых пикселях (с каждой стороны).
/// Одна для всех существ: кольцо одинаково выглядит на одном зуме независимо
/// от плотности пикселей текстуры.
pub(crate) const OUTLINE_THICKNESS_PX: f32 = 0.33;

/// Размер спрайта волка в пикселях мира. Текстура 128×128, но рисунок
/// занимает её центр (тело — около 1/3 кадра), поэтому два тайла дают
/// зверя длиной примерно в тайл, сопоставимо с игроком.
pub(crate) const WOLF_SPRITE_PX: f32 = TILE_SIZE * 2.0;

/// Размер спрайта кролика в пикселях мира. Текстура 128×128, но рисунок
/// заметно меньше волчьего (~25×24 px): при кваде в два тайла кролик
/// выглядит примерно вдвое уже волка.
pub(crate) const RABBIT_SPRITE_PX: f32 = TILE_SIZE * 2.0;

/// Размер спрайта игрока в пикселях мира (текстура 256×256 масштабируется
/// до тайла; объекты масштабируются отдельно — см. `object_sprite_size`).
const PLAYER_SPRITE_PX: f32 = TILE_SIZE;

/// Наземные предметы рисуются заметно меньше существ и ниже относительно
/// проекции тайла, ближе к его экранному центру.
const GROUND_ITEM_SCALE: f32 = 0.75;
const GROUND_ITEM_SCREEN_OFFSET: f32 = -TILE_SIZE * 0.25;

/// На сколько мировых пикселей сдвигать плоские спрайты объектов и
/// построек вниз **по экрану**. Сдвиг выполняется в плоскости земли
/// (см. `ground_screen_vertical_offset`), поэтому высота спрайта не
/// меняется и он не врастает в рельеф.
pub(crate) const OBJECT_SPRITE_SCREEN_DROP_PX: f32 = 8.0;

pub(crate) fn ground_screen_vertical_offset(camera_rotation: Quat, pixels: f32) -> Vec3 {
    let camera_up = camera_rotation * Vec3::Y;
    let ground_up = camera_up.truncate();
    let projected_length = ground_up.length_squared();
    if projected_length <= f32::EPSILON {
        return Vec3::ZERO;
    }
    Vec3::new(ground_up.x, ground_up.y, 0.0) * (pixels / projected_length)
}

/// Непрозрачная область рисунка в текстуре, в долях ширины/высоты
/// (правая и нижняя границы — исключающие). Считается по альфа-каналу
/// один раз на текстуру; из неё выводятся «ноги на землю», зона пикинга
/// курсором и радиус дилатации контура выделения — ручные константы
/// протухали при каждой перерисовке спрайтов.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpriteAlphaBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl SpriteAlphaBounds {
    /// Сдвиг спрайта вниз, в долях высоты, чтобы нижние пиксели рисунка
    /// стояли на земле: доля прозрачной полосы под рисунком.
    pub fn bottom_offset(self) -> f32 {
        1.0 - self.bottom
    }
}

/// Кэш видимых областей спрайтов существ по альфа-каналу их текстур.
/// Заполняется по мере загрузки ассетов (`update_entity_sprite_visuals`);
/// рендер и пикинг обязаны видеть один и тот же рисунок, поэтому источник
/// один — фактические пиксели текстуры.
#[derive(Resource, Default)]
pub(crate) struct EntitySpriteVisuals {
    map: HashMap<AtlasTexture, Option<SpriteAlphaBounds>>,
    /// Все текстуры существ отсканированы — дальше система бесплатна.
    warm: bool,
}

impl EntitySpriteVisuals {
    /// Границы рисунка текстуры. `None` — текстура ещё не отсканирована
    /// или непрозрачных пикселей в ней нет.
    pub(crate) fn bounds(&self, texture: AtlasTexture) -> Option<SpriteAlphaBounds> {
        self.map.get(&texture).copied().flatten()
    }

    /// Сдвиг «ног на землю» для текстуры; 0.0, пока границы неизвестны.
    pub(crate) fn bottom_offset(&self, texture: AtlasTexture) -> f32 {
        self.bounds(texture).map_or(0.0, SpriteAlphaBounds::bottom_offset)
    }
}

/// Непрозрачные границы рисунка по альфа-каналу (порог 128 — тот же, что
/// у контура выделения). `None` — формат не RGBA8 или рисунка нет.
pub(crate) fn scan_alpha_bounds(image: &Image) -> Option<SpriteAlphaBounds> {
    if image.texture_descriptor.format != TextureFormat::Rgba8UnormSrgb {
        return None;
    }
    let pixels = image.data.as_deref()?;
    let width = image.width();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0, 0);
    for (index, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if pixel[3] < 128 {
            continue;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    (max_x >= min_x).then(|| SpriteAlphaBounds {
        left: min_x as f32 / width as f32,
        top: min_y as f32 / image.height() as f32,
        right: (max_x + 1) as f32 / width as f32,
        bottom: (max_y + 1) as f32 / image.height() as f32,
    })
}

/// Скан видимых областей текстур существ: кэш наполняется по мере загрузки
/// ассетов, после полного прогрева система сводится к раннему выходу.
/// Хэндлы текстур известны на старте, ассеты грузятся асинхронно — недостающие
/// досканиваются на следующих кадрах.
pub(crate) fn update_entity_sprite_visuals(
    atlases: Option<Res<TextureAtlases>>,
    images: Res<Assets<Image>>,
    mut visuals: ResMut<EntitySpriteVisuals>,
) {
    if visuals.warm {
        return;
    }
    let Some(atlases) = atlases else {
        return;
    };
    let mut scanned = 0;
    let mut total = 0;
    for &texture in atlases.entity_textures() {
        total += 1;
        if visuals.map.contains_key(&texture) {
            scanned += 1;
            continue;
        }
        if let Some(image) = images.get(atlases.source(texture)) {
            visuals.map.insert(texture, scan_alpha_bounds(image));
            scanned += 1;
        }
    }
    visuals.warm = scanned == total;
}

/// Кегль счётчика лежащих предметов и его масштаб. Растр крупный
/// (40 px × 0.1): при мелком кегле глифы состоят из нескольких пикселей
/// и выглядят мылом при любом зуме.
const ITEM_COUNT_FONT_SIZE: f32 = 40.0;
const ITEM_COUNT_SCALE: f32 = 0.1;

/// Номинальная высота счётчика в пикселях мира.
const ITEM_COUNT_WORLD_PX: f32 = ITEM_COUNT_FONT_SIZE * ITEM_COUNT_SCALE;

/// Минимальный размер счётчика на экране, px. Мельче — это уже мыльный
/// пиксель, текст не показывается (и не спавнится на крупных LOD).
const ITEM_COUNT_MIN_SCREEN_PX: f32 = 4.0;

pub(crate) fn world_sprite_quad(
    meshes: &mut Assets<Mesh>,
    sprite_assets: &mut WorldSpriteAssets,
) -> Handle<Mesh> {
    sprite_assets
        .quad
        .get_or_insert_with(|| meshes.add(Rectangle::new(1.0, 1.0)))
        .clone()
}

/// Меш спрайта с UV, запечёнными в ячейку атласа. Один материал на атлас
/// (см. `world_sprite_material`) тогда подходит всем его текстурам, а
/// разные меши Bevy может собрать в один multi-draw batch set.
pub(crate) fn sprite_mesh(
    atlases: &TextureAtlases,
    texture: AtlasTexture,
    meshes: &mut Assets<Mesh>,
    sprite_assets: &mut WorldSpriteAssets,
) -> Handle<Mesh> {
    sprite_assets
        .meshes
        .entry(texture)
        .or_insert_with(|| {
            let mut mesh = Mesh::from(Rectangle::new(1.0, 1.0));
            let [u0, v0, u1, v1] = atlases.uv(texture);
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_UV_0,
                vec![[u1, v0], [u0, v0], [u0, v1], [u1, v1]],
            );
            mesh.asset_usage = RenderAssetUsages::RENDER_WORLD;
            meshes.add(mesh)
        })
        .clone()
}

/// Материал контура выделения для текстуры: белый силуэт, дилатированный
/// на радиус, соответствующий `OUTLINE_THICKNESS_PX` при данном размере
/// спрайта. `None` — текстура ещё не загружена (контур появится на кадре
/// после загрузки; система вызывается на каждый кадр выделения).
fn world_outline_asset(
    images: &mut Assets<Image>,
    atlases: &TextureAtlases,
    texture: AtlasTexture,
    sprite_size: Vec2,
    sprite_assets: &mut WorldSpriteAssets,
    materials: &mut Assets<StandardMaterial>,
) -> Option<OutlineAsset> {
    if let Some(asset) = sprite_assets.outlines.get(&texture) {
        return Some(asset.clone());
    }
    // Размеры и канвас извлекаются до вставки новой текстуры: мутация
    // `Assets<Image>` несовместима с живым заёмом источника.
    let (width, height, radius_x, radius_y, image) = {
        let source = images.get(atlases.source(texture))?;
        let radius_x = outline_radius_texels(source.width(), sprite_size.x);
        let radius_y = outline_radius_texels(source.height(), sprite_size.y);
        let image = build_outline_image(source, radius_x, radius_y)?;
        (source.width(), source.height(), radius_x, radius_y, image)
    };
    let asset = OutlineAsset {
        material: materials.add(StandardMaterial {
            base_color: Color::WHITE,
            base_color_texture: Some(images.add(image)),
            alpha_mode: AlphaMode::Mask(0.5),
            cull_mode: None,
            unlit: true,
            ..default()
        }),
        pad: Vec2::new(
            (width + 2 * radius_x) as f32 / width as f32,
            (height + 2 * radius_y) as f32 / height as f32,
        ),
    };
    sprite_assets.outlines.insert(texture, asset.clone());
    Some(asset)
}

/// Радиус дилатации в текселях, дающий `OUTLINE_THICKNESS_PX` мировых
/// пикселей. Плотность пикселей у текстур разная (человек — 256 текселей
/// на 16 мировых, волк — 128 на 32), радиус её компенсирует.
pub(crate) fn outline_radius_texels(texture_px: u32, sprite_world_px: f32) -> u32 {
    (OUTLINE_THICKNESS_PX * texture_px as f32 / sprite_world_px)
        .round()
        .max(1.0) as u32
}

/// Белый силуэт рисунка, дилатированный на заданный радиус: канвас на
/// 2·radius шире и выше исходника, непрозрачные пиксели — белые. Контур
/// рисуется этим канвасом с трансформом спрайта (масштаб — на канвасные
/// доли, см. `OutlineAsset::pad`), поэтому кольцо везде одной толщины
/// и не зависит от того, где в текстуре лежит рисунок. `None` — формат
/// не RGBA8 или рисунка нет (строить контур не из чего).
pub(crate) fn build_outline_image(source: &Image, radius_x: u32, radius_y: u32) -> Option<Image> {
    if source.texture_descriptor.format != TextureFormat::Rgba8UnormSrgb {
        return None;
    }
    let pixels = source.data.as_deref()?;
    let width = source.width();
    let height = source.height();
    let mut mask = Vec::with_capacity((width * height) as usize);
    for pixel in pixels.as_chunks::<4>().0 {
        mask.push(pixel[3] >= 128);
    }
    let dilated = dilate_mask(&mask, width, height, radius_x, radius_y);

    let canvas_width = width + 2 * radius_x;
    let canvas_height = height + 2 * radius_y;
    let mut data = vec![0_u8; (canvas_width * canvas_height * 4) as usize];
    for (index, &set) in dilated.iter().enumerate() {
        if !set {
            continue;
        }
        let x = index as u32 % width;
        let y = index as u32 / width;
        let offset = ((y + radius_y) * canvas_width + x + radius_x) as usize * 4;
        data[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    let mut outline = Image::new(
        Extent3d {
            width: canvas_width,
            height: canvas_height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    // Рисунок пиксель-арта: жёсткие края кольца без размытия.
    outline.sampler = ImageSampler::nearest();
    Some(outline)
}

/// Дилатация бинарной маски прямоугольником (2·radius+1)², сепарабельно:
/// горизонтальный и вертикальный проходы со скользящим окном пересчёта
/// счётчика — O(W·H) независимо от радиуса. Углы кольца прямые; для
/// контура в несколько мировых пикселей это неотличимо от круга.
pub(crate) fn dilate_mask(mask: &[bool], width: u32, height: u32, radius_x: u32, radius_y: u32) -> Vec<bool> {
    let width = width as usize;
    let height = height as usize;
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let rx = radius_x as usize;
    let ry = radius_y as usize;

    // Горизонтальный проход: окно [x - rx, x + rx] по строке, счётчик
    // непрозрачных пикселей в нём обновляется инкрементально.
    let mut horizontal = vec![false; mask.len()];
    for y in 0..height {
        let row = y * width;
        let mut count = mask[row..row + rx.min(width - 1) + 1]
            .iter()
            .filter(|pixel| **pixel)
            .count();
        for x in 0..width {
            horizontal[row + x] = count > 0;
            if x >= rx && mask[row + x - rx] {
                count -= 1;
            }
            let added = x + rx + 1;
            if added < width && mask[row + added] {
                count += 1;
            }
        }
    }

    // Вертикальный проход: то же окно по столбцу результата.
    let mut dilated = vec![false; mask.len()];
    for x in 0..width {
        let mut count = (0..=ry.min(height - 1))
            .filter(|offset| horizontal[offset * width + x])
            .count();
        for y in 0..height {
            dilated[y * width + x] = count > 0;
            if y >= ry && horizontal[(y - ry) * width + x] {
                count -= 1;
            }
            let added = y + ry + 1;
            if added < height && horizontal[added * width + x] {
                count += 1;
            }
        }
    }
    dilated
}

fn world_sprite_material(
    atlases: &TextureAtlases,
    texture: AtlasTexture,
    hurt_tint: bool,
    sprite_assets: &mut WorldSpriteAssets,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    // UV запечены в меш, поэтому материал общий на весь атлас: меньше
    // бинд-групп и больше возможностей для batching.
    let kind = texture.kind;
    sprite_assets
        .materials
        .entry((kind, hurt_tint))
        .or_insert_with(|| {
            materials.add(StandardMaterial {
                base_color: if hurt_tint { HURT_TINT } else { Color::WHITE },
                base_color_texture: Some(atlases.atlas(kind).image.clone()),
                alpha_mode: if hurt_tint {
                    AlphaMode::Blend
                } else {
                    AlphaMode::Mask(0.5)
                },
                cull_mode: None,
                unlit: true,
                ..default()
            })
        })
        .clone()
}

/// Спрайт игрока больше не нужен отдельной сущностью: по целевому формату
/// игрок — это захваченное существо мира и рисуется общим кодом спрайтов
/// существ (`update_entity_sprites`), текстура — `human.png`.
/// Синхронизирует спрайты item-сущностей дропа с `world.item_entities`:
/// новый дроп — спрайт, подбор/исчезновение — снятие спрайта. Позиция
/// статична (дроп не двигается),Billboard повёрнут к камере при спавне.
pub(crate) fn update_item_entity_sprites(
    mut commands: Commands,
    state: Res<GameState>,
    atlases: Option<Res<TextureAtlases>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sprite_assets: ResMut<WorldSpriteAssets>,
    mut sprites: ResMut<ItemEntitySprites>,
    camera: Query<&Transform, (With<Camera2d>, Without<EntitySprite>)>,
    sprite_transforms: Query<&mut Transform, With<EntitySprite>>,
) {
    let Some(world) = state.world.as_ref() else {
        for (_, (entity, _)) in sprites.map.drain() {
            commands.entity(entity).despawn();
        }
        return;
    };
    let Some(atlases) = atlases else {
        return;
    };
    let Ok(camera_transform) = camera.single() else {
        return;
    };
    let mut sprite_transforms: Query<&mut Transform, With<EntitySprite>> = sprite_transforms;
    // Снятие спрайтов подобранных/исчезнувших предметов.
    let stale: Vec<u64> = sprites
        .map
        .keys()
        .copied()
        .filter(|id| world.item_entity(*id).is_none())
        .collect();
    for id in stale {
        if let Some((entity, _)) = sprites.map.remove(&id) {
            commands.entity(entity).despawn();
        }
    }
    let camera_up = camera_transform.rotation * Vec3::Y;
    let height = TILE_SIZE * 0.3;
    for entity in &world.item_entities.items {
        let pos_xy = [entity.pos.x, entity.pos.y];
        if let Some(&(sprite, last_pos)) = sprites.map.get(&entity.id) {
            // Лежащий предмет не двигается; летящий — обновляем позицию
            // спрайта (интерполяция не нужна: шаг тика и так плавный).
            if (last_pos[0] - entity.pos.x).abs() > 1e-4 || (last_pos[1] - entity.pos.y).abs() > 1e-4 {
                if let Ok(mut transform) = sprite_transforms.get_mut(sprite) {
                    transform.translation = world_position(world, Vec2::new(entity.pos.x, entity.pos.y)) + camera_up * height;
                }
                sprites.map.insert(entity.id, (sprite, pos_xy));
            }
            continue;
        }
        let pos = world_position(world, Vec2::new(entity.pos.x, entity.pos.y))
            + camera_up * height;
        let texture = atlases.item(entity.stack.kind);
        let mesh = sprite_mesh(&atlases, texture, &mut meshes, &mut sprite_assets);
        let material =
            world_sprite_material(&atlases, texture, false, &mut sprite_assets, &mut materials);
        let size = Vec2::splat(TILE_SIZE * 0.5 * GROUND_ITEM_SCALE);
        let mut transform = Transform::from_translation(pos)
            .with_rotation(camera_transform.rotation);
        transform.scale = Vec3::new(size.x, size.y, 1.0);
        let sprite = commands
            .spawn((Mesh3d(mesh), MeshMaterial3d(material), transform, EntitySprite))
            .id();
        sprites.map.insert(entity.id, (sprite, pos_xy));
    }
}

/// Обновляет спрайты животных: создаёт их для видимых существ, двигает
/// интерполяцией между тиками, меняет текстуру по состоянию и кадру
/// анимации, зеркалит при взгляде влево. Невидимые существа не держат
/// Bevy-сущностей — рендер читает только видимую область.
pub(crate) fn update_entity_sprites(
    mut commands: Commands,
    state: Res<GameState>,
    acc: Res<TickAccumulator>,
    time: Res<Time>,
    atlases: Option<Res<TextureAtlases>>,
    visuals: Res<EntitySpriteVisuals>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sprite_assets: ResMut<WorldSpriteAssets>,
    mut sprites: ResMut<EntitySprites>,
    mut flashes: ResMut<HurtFlashes>,
    camera: Query<(&Camera, &Transform, &Projection), With<Camera2d>>,
) {
    let Some(world) = state.world.as_ref() else {
        for (_, entity) in sprites.map.drain() {
            commands.entity(entity).despawn();
        }
        sprites.textures.clear();
        for (_, overlay) in flashes.overlays.drain() {
            commands.entity(overlay).despawn();
        }
        flashes.prev_hp.clear();
        flashes.timers.clear();
        return;
    };
    let Some(atlases) = atlases else {
        return;
    };
    let Ok((camera, camera_transform, projection)) = camera.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    let Some(viewport_size) = camera.logical_viewport_size() else {
        return;
    };
    let sprite_margin_px = WOLF_SPRITE_PX / ortho.scale;
    let camera_global = GlobalTransform::from(*camera_transform);

    // Таймеры вспышек урона гаснут по реальному времени.
    let delta = time.delta_secs();
    flashes.timers.retain(|_, timer| {
        *timer -= delta;
        *timer > 0.0
    });

    let mut visible = HashSet::with_capacity(world.entities.len());
    for entity in world.entities.iter() {
        let pos = interpolate_pos(entity.prev_pos, entity.pos, &acc);
        let world_pos = world_position(world, Vec2::new(pos.x, pos.y));
        let Ok(viewport_pos) = camera.world_to_viewport(&camera_global, world_pos) else {
            continue;
        };
        if viewport_pos.x < -sprite_margin_px
            || viewport_pos.x > viewport_size.x + sprite_margin_px
            || viewport_pos.y < -sprite_margin_px
            || viewport_pos.y > viewport_size.y + sprite_margin_px
        {
            continue;
        }
        visible.insert(entity.id);

        // Детект удара: HP стало меньше, чем на прошлом кадре — вспышка.
        let previous = flashes.prev_hp.insert(entity.id, entity.hp);
        if previous.is_some_and(|prev| entity.hp < prev) {
            flashes.timers.insert(entity.id, HURT_FLASH_SECS);
        }

        let texture = atlases.entity(entity);
        let mesh = sprite_mesh(&atlases, texture, &mut meshes, &mut sprite_assets);
        let transform = entity_transform(
            &pos,
            world,
            camera_transform.rotation,
            entity_sprite_size(entity.kind),
            visuals.bottom_offset(texture),
            entity.facing == Facing::Left,
        );

        match sprites.map.get(&entity.id).copied() {
            Some(bevy_entity) => {
                // Материал меняется только со сменой текстуры (состояние/кадр
                // анимации): без смены — двигаем спрайт, не трогая компонент.
                if sprites.textures.get(&entity.id) != Some(&texture) {
                    let material = world_sprite_material(
                        &atlases,
                        texture,
                        false,
                        &mut sprite_assets,
                        &mut materials,
                    );
                    commands
                        .entity(bevy_entity)
                        .insert((Mesh3d(mesh.clone()), MeshMaterial3d(material), transform));
                    sprites.textures.insert(entity.id, texture);
                } else {
                    commands.entity(bevy_entity).insert(transform);
                }
            }
            None => {
                let material = world_sprite_material(
                    &atlases,
                    texture,
                    false,
                    &mut sprite_assets,
                    &mut materials,
                );
                let bevy_entity = commands
                    .spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material),
                        transform,
                        EntitySprite,
                    ))
                    .id();
                sprites.map.insert(entity.id, bevy_entity);
                sprites.textures.insert(entity.id, texture);
            }
        }

        // Красная подсветка: копия текстуры с красной 30% альфой поверх —
        // краснеет именно рисунок, а не прямоугольник спрайта.
        let hurt = flashes.timers.contains_key(&entity.id);
        match flashes.overlays.get(&entity.id).copied() {
            Some(overlay) if hurt => {
                let material = world_sprite_material(
                    &atlases,
                    texture,
                    true,
                    &mut sprite_assets,
                    &mut materials,
                );
                commands
                    .entity(overlay)
                    .insert((Mesh3d(mesh.clone()), MeshMaterial3d(material), transform));
            }
            Some(overlay) => {
                flashes.overlays.remove(&entity.id);
                commands.entity(overlay).despawn();
            }
            None if hurt => {
                let material = world_sprite_material(
                    &atlases,
                    texture,
                    true,
                    &mut sprite_assets,
                    &mut materials,
                );
                let overlay = commands
                    .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(material), transform))
                    .id();
                flashes.overlays.insert(entity.id, overlay);
            }
            None => {}
        }
    }

    let stale: Vec<u64> = sprites
        .map
        .keys()
        .copied()
        .filter(|id| !visible.contains(id))
        .collect();
    for id in stale {
        if let Some(entity) = sprites.map.remove(&id) {
            commands.entity(entity).despawn();
        }
        sprites.textures.remove(&id);
        // Существо покинуло экран или умерло: вспышка и overlay не нужны.
        flashes.prev_hp.remove(&id);
        flashes.timers.remove(&id);
        if let Some(overlay) = flashes.overlays.remove(&id) {
            commands.entity(overlay).despawn();
        }
    }
}

/// Белый контур выделения существа в редакторе: подсветка под курсором
/// (`EditorSelection::hovered`) и открытое окно-инспектор (`selected`).
/// Контур — дилатированный белый силуэт текстуры (`world_outline_asset`),
/// нарисованный на канвасе с паддингом тем же трансформом, что у спрайта:
/// кольцо везде одной мировой толщины и не смещается при рисунке,
/// нецентрованном в текстуре. Рисуется только в наблюдателе; существо
/// ушло — контур снят.
pub(crate) fn update_selection_outlines(
    mut commands: Commands,
    state: Res<GameState>,
    selection: Res<EditorSelection>,
    acc: Res<TickAccumulator>,
    atlases: Option<Res<TextureAtlases>>,
    visuals: Res<EntitySpriteVisuals>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut sprite_assets: ResMut<WorldSpriteAssets>,
    mut outlines: ResMut<SelectionOutlines>,
    camera: Query<(&Camera, &Transform), With<Camera2d>>,
) {
    let Some(world) = state.world.as_ref() else {
        for (_, entity) in outlines.map.drain() {
            commands.entity(entity).despawn();
        }
        outlines.textures.clear();
        return;
    };
    // Список существ с контуром: hovered и selected (без дубликатов),
    // только живые и только в режиме редактора.
    let mut want: Vec<u64> = Vec::new();
    if state.mode == GameMode::Observer {
        for id in [selection.hovered, selection.selected].into_iter().flatten() {
            if world.entities.get(id).is_some() && !want.contains(&id) {
                want.push(id);
            }
        }
    }
    let stale: Vec<u64> = outlines
        .map
        .keys()
        .copied()
        .filter(|id| !want.contains(id))
        .collect();
    for id in stale {
        if let Some(entity) = outlines.map.remove(&id) {
            commands.entity(entity).despawn();
        }
        outlines.textures.remove(&id);
    }
    if want.is_empty() {
        return;
    }
    let Some(atlases) = atlases else {
        return;
    };
    let Ok((_, camera_transform)) = camera.single() else {
        return;
    };
    let quad = world_sprite_quad(&mut meshes, &mut sprite_assets);
    // Контур рисуется чуть глубже сцены: он за спрайтом существа, но
    // перед рельефом.
    let back = camera_transform.rotation * -Vec3::Z * 0.05;
    for id in want {
        let Some(entity) = world.entities.get(id) else {
            continue;
        };
        let pos = interpolate_pos(entity.prev_pos, entity.pos, &acc);
        let texture = atlases.entity(entity);
        // Трансформ ровно тот же, что у спрайта; масштаб домножается на
        // пад-факторы канваса контура (перевод текселей канваса в тексели
        // исходника), поэтому внутренняя область ложится на спрайт 1:1,
        // а кольцо везде ровно OUTLINE_THICKNESS_PX.
        let size = entity_sprite_size(entity.kind);
        let mut transform = entity_transform(
            &pos,
            world,
            camera_transform.rotation,
            size,
            visuals.bottom_offset(texture),
            entity.facing == Facing::Left,
        );
        transform.translation += back;
        match outlines.map.get(&id).copied() {
            Some(bevy_entity) => {
                // Материал меняется вместе с текстурой (состояние/кадр
                // анимации): волк лёг — силуэт обязан смениться на лежачего,
                // иначе контур останется от прежнего состояния.
                if outlines.textures.get(&id) != Some(&texture) {
                    let Some(asset) = world_outline_asset(
                        &mut images,
                        &atlases,
                        texture,
                        size,
                        &mut sprite_assets,
                        &mut materials,
                    ) else {
                        continue;
                    };
                    transform.scale.x *= asset.pad.x;
                    transform.scale.y *= asset.pad.y;
                    commands
                        .entity(bevy_entity)
                        .insert((MeshMaterial3d(asset.material), transform));
                    outlines.textures.insert(id, texture);
                } else {
                    if let Some(asset) = sprite_assets.outlines.get(&texture) {
                        transform.scale.x *= asset.pad.x;
                        transform.scale.y *= asset.pad.y;
                    }
                    commands.entity(bevy_entity).insert(transform);
                }
            }
            None => {
                // Текстура не загружена — силуэт построить не из чего:
                // контур появится на следующем кадре выделения.
                let Some(asset) = world_outline_asset(
                    &mut images,
                    &atlases,
                    texture,
                    size,
                    &mut sprite_assets,
                    &mut materials,
                ) else {
                    continue;
                };
                transform.scale.x *= asset.pad.x;
                transform.scale.y *= asset.pad.y;
                let bevy_entity = commands
                    .spawn((Mesh3d(quad.clone()), MeshMaterial3d(asset.material), transform))
                    .id();
                outlines.map.insert(id, bevy_entity);
                outlines.textures.insert(id, texture);
            }
        }
    }
}

/// Спрайт существа развёрнут к камере и стоит на поверхности тайла.
/// `bottom_offset` опускает квадрат, если у текстуры снизу прозрачная
/// полоса (видимые пиксели должны касаться земли).
fn entity_transform(
    pos: &WorldPos,
    world: &World,
    camera_rotation: Quat,
    size: Vec2,
    bottom_offset: f32,
    flip_x: bool,
) -> Transform {
    let location = world_position(world, Vec2::new(pos.x, pos.y));
    let camera_up = camera_rotation * Vec3::Y;
    let mut transform =
        Transform::from_translation(location + camera_up * (size.y * (0.5 - bottom_offset)))
            .with_rotation(camera_rotation);
    transform.scale = Vec3::new(if flip_x { -size.x } else { size.x }, size.y, 1.0);
    transform
}

/// Размер спрайта существа в пикселях мира.
pub(crate) fn entity_sprite_size(kind: EntityKind) -> Vec2 {
    match kind {
        EntityKind::Wolf => Vec2::splat(WOLF_SPRITE_PX),
        // Человек временно рисуется текстурой игрока (решение автора):
        // квадрат в один тайл, как раньше был спрайт игрока.
        EntityKind::Human => Vec2::splat(PLAYER_SPRITE_PX),
        EntityKind::Rabbit => Vec2::splat(RABBIT_SPRITE_PX),
    }
}

/// Нужно ли рисовать счётчики лежащих предметов при данном масштабе камеры:
/// только на LOD 0 и только если на экране текст не мельче
/// `ITEM_COUNT_MIN_SCREEN_PX`. На крупных LOD текст не показывается вовсе.
pub(crate) fn item_counts_visible(scale: f32) -> bool {
    lod_for_scale(scale) == 0 && ITEM_COUNT_WORLD_PX / scale >= ITEM_COUNT_MIN_SCREEN_PX
}

pub(crate) fn update_item_count_labels(
    cameras: Query<&Projection, With<Camera2d>>,
    mut labels: Query<&mut Visibility, With<ItemCountLabel>>,
) {
    let Ok(projection) = cameras.single() else {
        return;
    };
    let Projection::Orthographic(orthographic) = projection else {
        return;
    };
    let visibility = if item_counts_visible(orthographic.scale) {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut label in &mut labels {
        *label = visibility;
    }
}

/// Спавнит объекты блока как дочерние спрайты. Объекты и постройки —
/// camera-facing спрайты с UV, запечёнными в меш атласа; костёр — крест
/// из двух вертикальных квадов вдоль середин сторон тайла; склад — плоский
/// декаль на всю поверхность тайла. Идёт по разреженному слою объектов
/// четырёх чанков блока — стоимость пропорциональна числу объектов.
pub(crate) fn spawn_objects(
    parent: &mut EntityCommands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut WorldSpriteAssets,
    world: &World,
    block: BlockPos,
    origin: Vec3,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
) {
    let bx0 = block.x * BLOCK_TILES;
    let by0 = block.y * BLOCK_TILES;
    // Блок 64×64 покрывает ровно 4×4 чанков (сетка блоков кратна чанку);
    // у края мира часть чанков за границей — они пропускаются.
    let chunks_per_block = BLOCK_TILES / CHUNK_SIZE as i32;
    let cps = world.chunks_per_side as i32;

    parent.with_children(|parent| {
        for cy in 0..chunks_per_block {
            for cx in 0..chunks_per_block {
                let cpx = bx0.div_euclid(CHUNK_SIZE as i32) + cx;
                let cpy = by0.div_euclid(CHUNK_SIZE as i32) + cy;
                if cpx < 0 || cpy < 0 || cpx >= cps || cpy >= cps {
                    continue;
                }
                let chunk = world.chunk(ChunkPos { x: cpx as u32, y: cpy as u32 });
                for (&idx, obj) in &chunk.objects {
                    let tile_x = cpx * CHUNK_SIZE as i32 + (idx % CHUNK_SIZE as u8) as i32;
                    let tile_y = cpy * CHUNK_SIZE as i32 + (idx / CHUNK_SIZE as u8) as i32;
                    let tile_world = world_position(
                        world,
                        Vec2::new(tile_x as f32 + 0.5, tile_y as f32 + 0.5),
                    );
                    let anchor = tile_world - origin;
                    match obj.kind {
                        // Костёр — станция, но рисуется крестом из двух
                        // вертикальных квадов вдоль осей X и Y тайла.
                        ObjectKind::Building(BuildingKind::Campfire) => {
                            spawn_campfire_cross(
                                parent, meshes, sprite_assets, materials, atlases, anchor,
                            );
                        }
                        // Склад: плоская текстура-декаль поверх тайла.
                        ObjectKind::Building(BuildingKind::Storage) => {
                            spawn_flat_decal(
                                parent, meshes, sprite_assets, materials,
                                atlases, atlases.storage_flat(), anchor, 1.0,
                            );
                        }
                        // Все остальные постройки — плоский спрайт по общему
                        // правилу объектов (низ прижат к центру тайла).
                        _ => {
                            let texture = atlases.object(obj);
                            let mesh = sprite_mesh(atlases, texture, meshes, sprite_assets);
                            let size = object_sprite_size(atlases.source_size(texture));
                            let height = size.y * 0.5;
                            let camera_up = camera_rotation * Vec3::Y;
                            let material = world_sprite_material(
                                atlases,
                                texture,
                                false,
                                sprite_assets,
                                materials,
                            );
                            // Сдвиг вниз по экрану — в плоскости земли, без
                            // изменения высоты: спрайт «стоит» на тайле, но
                            // не уходит под рельеф.
                            let screen_drop = ground_screen_vertical_offset(
                                camera_rotation,
                                -OBJECT_SPRITE_SCREEN_DROP_PX,
                            );
                            let mut transform = Transform::from_translation(
                                anchor + camera_up * height + screen_drop,
                            )
                            .with_rotation(camera_rotation);
                            transform.scale = Vec3::new(size.x, size.y, 1.0);
                            parent.spawn((Mesh3d(mesh), MeshMaterial3d(material), transform));
                        }
                    }
                }
            }
        }
    });
}

/// Небольшой z-сдвиг декаля над рельефом против z-fighting.
pub(crate) const DECAL_Z_OFFSET: f32 = 0.02;

/// Крест из двух вертикальных квадов вдоль осей X и Y через центр точки
/// (из середин противоположных сторон): двойная сторона и маска-альфа
/// материалов спрайтов дают правильный вид с фиксированной камеры.
/// Общая основа костра и культур.
fn spawn_cross(
    parent: &mut ChildSpawnerCommands,
    meshes: &mut Assets<Mesh>,
    sprite_assets: &mut WorldSpriteAssets,
    materials: &mut Assets<StandardMaterial>,
    atlases: &TextureAtlases,
    texture: AtlasTexture,
    anchor: Vec3,
    edge: f32,
    height: f32,
) {
    let mesh = sprite_mesh(atlases, texture, meshes, sprite_assets);
    let material = world_sprite_material(atlases, texture, false, sprite_assets, materials);
    let center = anchor + Vec3::Z * height * 0.5;
    // Квад 1: стена вдоль оси X (нормаль −Y, ширина по X, высота по Z).
    parent.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(material.clone()),
        Transform::from_translation(center)
            .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
            .with_scale(Vec3::new(edge, height, 1.0)),
    ));
    // Квад 2: та же стена, довернутая вокруг Z на 90° — вдоль оси Y.
    parent.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(center)
            .with_rotation(
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2) * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            )
            .with_scale(Vec3::new(edge, height, 1.0)),
    ));
}

/// Крест костра в тайле.
fn spawn_campfire_cross(
    parent: &mut ChildSpawnerCommands,
    meshes: &mut Assets<Mesh>,
    sprite_assets: &mut WorldSpriteAssets,
    materials: &mut Assets<StandardMaterial>,
    atlases: &TextureAtlases,
    anchor: Vec3,
) {
    spawn_cross(
        parent, meshes, sprite_assets, materials,
        atlases, atlases.campfire_cross(), anchor,
        WORLD_TILE_SIZE, TILE_SIZE * 0.6,
    );
}

/// Плоский декаль на поверхность тайла (культуры, склад): горизонтальный
/// квад, заполняющий ромб тайла, с z-сдвигом против z-fighting. `scale`
/// — доля тайла (1.0 — весь ромб).
fn spawn_flat_decal(
    parent: &mut ChildSpawnerCommands,
    meshes: &mut Assets<Mesh>,
    sprite_assets: &mut WorldSpriteAssets,
    materials: &mut Assets<StandardMaterial>,
    atlases: &TextureAtlases,
    texture: AtlasTexture,
    anchor: Vec3,
    scale: f32,
) {
    let mesh = sprite_mesh(atlases, texture, meshes, sprite_assets);
    let material = world_sprite_material(atlases, texture, false, sprite_assets, materials);
    let edge = WORLD_TILE_SIZE * scale;
    parent.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(anchor + Vec3::Z * DECAL_Z_OFFSET)
            .with_scale(Vec3::new(edge, edge, 1.0)),
    ));
}

/// Размер спрайта по размерам исходной текстуры: 64 px → 1.6 тайла,
/// 128 px → 3.2 тайла. Постройки используют то же правило, что и природные
/// объекты, — крупнее, чтобы детализация 128 px читалась на экране.
pub(crate) fn object_sprite_size(texture_size: UVec2) -> Vec2 {
    const TILES_PER_64PX: f32 = 1.6;
    let scale = TILES_PER_64PX * TILE_SIZE / 64.0;
    Vec2::new(texture_size.x as f32 * scale, texture_size.y as f32 * scale)
}

/// Высота креста культуры, мировых единиц (как у костра — часть тайла).
const CROP_HEIGHT: f32 = TILE_SIZE * 0.55;

/// Спавнит культуры блока крестами из двух вертикальных квадов (как костёр).
/// Идёт по разреженному слою культур четырёх чанков блока.
pub(crate) fn spawn_crops(
    parent: &mut EntityCommands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut WorldSpriteAssets,
    world: &World,
    block: BlockPos,
    origin: Vec3,
    atlases: &TextureAtlases,
) {
    let bx0 = block.x * BLOCK_TILES;
    let by0 = block.y * BLOCK_TILES;
    let chunks_per_block = BLOCK_TILES / CHUNK_SIZE as i32;
    let cps = world.chunks_per_side as i32;

    parent.with_children(|parent| {
        for cy in 0..chunks_per_block {
            for cx in 0..chunks_per_block {
                let cpx = bx0.div_euclid(CHUNK_SIZE as i32) + cx;
                let cpy = by0.div_euclid(CHUNK_SIZE as i32) + cy;
                if cpx < 0 || cpy < 0 || cpx >= cps || cpy >= cps {
                    continue;
                }
                let chunk = world.chunk(ChunkPos { x: cpx as u32, y: cpy as u32 });
                for (&idx, crop) in &chunk.crops {
                    let tile = TilePos {
                        x: cpx * CHUNK_SIZE as i32 + (idx % CHUNK_SIZE as u8) as i32,
                        y: cpy * CHUNK_SIZE as i32 + (idx / CHUNK_SIZE as u8) as i32,
                    };
                    let tile_world = world_position(
                        world, Vec2::new(tile.x as f32 + 0.5, tile.y as f32 + 0.5),
                    );
                    spawn_cross(
                        parent, meshes, sprite_assets, materials,
                        atlases, atlases.crop(crop.kind, crop.stage),
                        tile_world - origin,
                        WORLD_TILE_SIZE, CROP_HEIGHT,
                    );
                }
            }
        }
    });
}

/// Спавнит предметы на тайлах блока (дроп) как дочерние спрайты.
pub(crate) fn spawn_items(
    parent: &mut EntityCommands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut WorldSpriteAssets,
    world: &World,
    block: BlockPos,
    origin: Vec3,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Handle<Font>,
) {
    let bx0 = block.x * BLOCK_TILES;
    let by0 = block.y * BLOCK_TILES;
    let chunks_per_block = BLOCK_TILES / CHUNK_SIZE as i32;
    let cps = world.chunks_per_side as i32;

    parent.with_children(|parent| {
        for cy in 0..chunks_per_block {
            for cx in 0..chunks_per_block {
                let cpx = bx0.div_euclid(CHUNK_SIZE as i32) + cx;
                let cpy = by0.div_euclid(CHUNK_SIZE as i32) + cy;
                if cpx < 0 || cpy < 0 || cpx >= cps || cpy >= cps {
                    continue;
                }
                let chunk = world.chunk(ChunkPos { x: cpx as u32, y: cpy as u32 });
                for (&idx, stack) in &chunk.items {
                    let tile = TilePos {
                        x: cpx * CHUNK_SIZE as i32 + (idx % CHUNK_SIZE as u8) as i32,
                        y: cpy * CHUNK_SIZE as i32 + (idx / CHUNK_SIZE as u8) as i32,
                    };
                    let tile_world = world_position(
                        world,
                        Vec2::new(tile.x as f32 + 0.5, tile.y as f32 + 0.5),
                    );
                    let local = tile_world - origin;
                    let item_anchor = local + Vec3::Z;
                    let item_height = TILE_SIZE * 0.3;
                    let item_offset = camera_rotation * Vec3::Y * item_height
                        + ground_screen_vertical_offset(
                            camera_rotation,
                            GROUND_ITEM_SCREEN_OFFSET,
                        );
                    let texture = atlases.item(stack.kind);
                    let mesh = sprite_mesh(atlases, texture, meshes, sprite_assets);
                    let material =
                        world_sprite_material(atlases, texture, false, sprite_assets, materials);
                    let item_size = Vec2::splat(TILE_SIZE * 0.6 * GROUND_ITEM_SCALE);
                    let mut transform = Transform::from_translation(
                        item_anchor + item_offset,
                    )
                    .with_rotation(camera_rotation);
                    transform.scale = Vec3::new(item_size.x, item_size.y, 1.0);
                    parent.spawn((Mesh3d(mesh), MeshMaterial3d(material), transform));
                    if stack.count > 1 {
                        // Счётчик растрируется крупным шрифтом и уменьшается
                        // трансформом: при font_size = 4 глифы состоят из
                        // нескольких пикселей и на экране выглядят мылом
                        // при любом зуме. 40 px с масштабом 0.1 дают тот же
                        // размер на экране, но с чёткой растеризацией.
                        let label_anchor = item_anchor
                            + item_offset
                            + Vec3::new(TILE_SIZE * 0.25, -TILE_SIZE * 0.25, 2.0);
                        parent.spawn((
                            Text2d::new(stack.count.to_string()),
                            Visibility::Visible,
                            ItemCountLabel,
                            TextFont {
                                font: font.clone().into(),
                                font_size: ITEM_COUNT_FONT_SIZE.into(),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                            Transform::from_translation(label_anchor)
                            .with_rotation(camera_rotation)
                            .with_scale(Vec3::splat(ITEM_COUNT_SCALE)),
                        ));
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod ground_item_tests {
    use super::*;

    #[test]
    fn ground_item_offset_moves_down_on_screen_without_changing_height() {
        let camera_rotation =
            Quat::from_rotation_z(0.4) * Quat::from_rotation_x(-std::f32::consts::FRAC_PI_6);
        let camera_up = camera_rotation * Vec3::Y;
        let camera_right = camera_rotation * Vec3::X;
        let offset =
            ground_screen_vertical_offset(camera_rotation, GROUND_ITEM_SCREEN_OFFSET);

        assert!(offset.z.abs() < f32::EPSILON);
        assert!((offset.dot(camera_up) - GROUND_ITEM_SCREEN_OFFSET).abs() < 1e-4);
        assert!(offset.dot(camera_right).abs() < 1e-4);
    }
}
