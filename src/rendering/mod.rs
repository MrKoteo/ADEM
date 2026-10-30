//! Рендеринг мира: меши terrain-блоков с LOD и спрайты объектов.
//!
//! Мир нарезан на блоки 64×64 тайлов. Видимые блоки рисуются одним мешем
//! на блок: все тайлы блока — квады в общей текстуре-атласе. При отдалении
//! камеры соседние тайлы агрегируются в ячейки (LOD), и в ячейке берётся
//! доминирующий тип тайла. Смена LOD атомарна: сцена нового уровня строится
//! на фоне старой, и старая остаётся на экране, пока новая не покроет весь
//! вид — иначе по краям экрана мигали бы чёрные куски недостроенных блоков.
//! Объекты (деревья, кусты, валуны, яблони) остаются спрайтами и спавнятся
//! только для видимых блоков, чтобы не держать в памяти миллионы сущностей.

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::sprite_render::AlphaMode2d;

use crate::app::input::{PlayerInput, cursor_world_tile};
use crate::app::state::{GameMode, GameState, PlayerRes, SimTick, TickAccumulator, interpolate_pos, player_render_pos};
use crate::simulation::commands::buildable_at;
use crate::simulation::objects::build_cost;
use crate::simulation::{ChunkPos, EntityKind, Facing, ObjectKind, TerrainTiles, TilePos, TileType, World, CHUNK_SIZE, TILE_SIZE, WorldPos};
use crate::ui::UiFont;
use crate::ui::construction::ConstructionUi;

mod combat;
mod lighting;
mod textures;

use textures::TileTextures;

/// Сторона terrain-блока в тайлах. 64 делит все поддерживаемые размеры мира.
const BLOCK_TILES: i32 = 64;

/// Желаемый размер агрегированной ячейки на экране, в пикселях.
/// Менялось вручную при тюнинге — тест LOD-порогов берёт значение отсюда.
pub(crate) const TARGET_CELL_PX: f32 = 2.0;

/// Максимальный уровень детализации (ячейка 2^MAX_LOD тайлов).
pub(crate) const MAX_LOD: u32 = 6;

/// Размер одного тайла в исходных PNG.
const ATLAS_TILE_PX: u32 = 16;

/// Ширина повтора краёв вокруг тайла в атласе (защита от bleeding).
const ATLAS_GUTTER_PX: u32 = 2;

/// Сколько тайлов в атласе по горизонтали и вертикали.
const ATLAS_COLUMNS: u32 = 4;
const ATLAS_ROWS: u32 = 3;

/// Размер ячейки атласа с учётом повтора краёв.
const ATLAS_CELL_PX: u32 = ATLAS_TILE_PX + ATLAS_GUTTER_PX * 2;

/// Полный размер атласа.
const ATLAS_WIDTH: u32 = ATLAS_CELL_PX * ATLAS_COLUMNS;
const ATLAS_HEIGHT: u32 = ATLAS_CELL_PX * ATLAS_ROWS;

/// Количество непустых видов тайлов (индексы атласа). Варианты идут
/// подряд за `Empty`, порядок совпадает с `TileType` без него.
pub(crate) const TILE_KIND_COUNT: usize = 11;

/// Сколько блоков строить за кадр при LOD 0 (дальше бюджет растёт).
const BUILD_BLOCKS_BASE: usize = 4;

/// Верхняя граница бюджета построения блоков за кадр.
const BUILD_BLOCKS_MAX: usize = 32;

/// Запас вокруг видимой области, в тайлах (деревья выступают за свой тайл).
const VIEW_MARGIN_TILES: f32 = 2.0;

pub struct RenderingPlugin;

impl Plugin for RenderingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, textures::load_tile_textures)
            .init_resource::<TerrainRender>()
            .init_resource::<EntitySprites>()
            .init_resource::<HurtFlashes>()
            .init_resource::<lighting::CampfireLights>()
            .init_resource::<lighting::DarknessOverlay>()
            .add_systems(
                Update,
                (
                    ensure_ground_material,
                    render_world,
                    // Счётчики предметов скрываются/показываются по зуму;
                    // новый LOD мог изменить набор этих сущностей.
                    update_item_count_labels,
                )
                    .chain(),
            )
            // Реестр костров сверяется с миром до render_world: тот изымает
            // dirty-тайлы, по которым отслеживаются новые и потухшие костры.
            .add_systems(Update, lighting::update_campfire_positions.before(render_world))
            .add_systems(Update, (update_player_sprite, update_entity_sprites).after(SimTick))
            // Оверлей тьмы поверх мира ( lighting): после перестройки блоков,
            // чтобы камера и мир были актуальны.
            .add_systems(Update, lighting::update_darkness_overlay.after(update_item_count_labels))
            .add_systems(Update, update_build_hologram)
            // Боевые визуализации: след взмаха, снаряды, воткнутые копья.
            .add_plugins(combat::CombatFxPlugin);
    }
}

/// Построенный terrain-блок: сущность и её меш (если в блоке есть тайлы).
struct BuiltBlock {
    entity: Entity,
    mesh: Option<Handle<Mesh>>,
}

/// Координаты terrain-блока в сетке блоков.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct BlockPos {
    pub x: i32,
    pub y: i32,
}

/// Состояние рендера мира: материал, активные блоки и переход LOD.
#[derive(Resource, Default)]
struct TerrainRender {
    material: Option<Handle<ColorMaterial>>,
    /// Готовая сцена текущего LOD: видимые блоки.
    active: HashMap<BlockPos, BuiltBlock>,
    /// Строящаяся сцена нового LOD; активная остаётся на экране до её готовности.
    incoming: Option<IncomingLod>,
    blocks_per_side: i32,
    lod: u32,
}

/// Сцена нового LOD, достраивающаяся на фоне активной.
struct IncomingLod {
    lod: u32,
    blocks: HashMap<BlockPos, BuiltBlock>,
}

/// Метка сущности terrain-блока.
#[derive(Component)]
struct TerrainBlock;

/// Метка спрайта игрока.
#[derive(Component)]
struct PlayerSprite;

/// Метка динамического спрайта существа (животного).
#[derive(Component)]
struct EntitySprite;

/// Метка счётчика количества у лежащего предмета. Видимость пересчитывается
/// каждый кадр по зуму: на крупном LOD текста нет, а внутри LOD 0 он
/// скрывается, когда становится мельче `ITEM_COUNT_MIN_SCREEN_PX`.
#[derive(Component)]
struct ItemCountLabel;

/// Соответствие «id существа симуляции → Bevy-сущность спрайта».
/// Спрайты создаются только для видимых существ и удаляются, когда
/// существо уходит с экрана.
#[derive(Resource, Default)]
struct EntitySprites {
    map: HashMap<u64, Entity>,
}

/// Сколько секунд после удара существо подсвечивается красным.
const HURT_FLASH_SECS: f32 = 0.33;
/// Подсветка урона: красный поверх текстуры с 60% альфы.
const HURT_TINT: Color = Color::srgba(1.0, 0.0, 0.0, 0.6);

/// Вспышки урона существ: предыдущие HP (для детекта удара), таймеры
/// подсветки и overlay-спрайты «краснее» поверх основной текстуры.
#[derive(Resource, Default)]
struct HurtFlashes {
    prev_hp: HashMap<u64, f32>,
    timers: HashMap<u64, f32>,
    overlays: HashMap<u64, Entity>,
}

/// Размер спрайта волка в пикселях мира. Текстура 128×128, но рисунок
/// занимает её центр (тело — около 1/3 кадра), поэтому два тайла дают
/// зверя длиной примерно в тайл, сопоставимо с игроком.
const WOLF_SPRITE_PX: f32 = TILE_SIZE * 2.0;

/// Размер спрайта игрока в пикселях мира (текстура 64×64 масштабируется
/// до тайла; объекты масштабируются отдельно — см. `object_sprite_size`).
const PLAYER_SPRITE_PX: f32 = TILE_SIZE;

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

/// Собирает атлас из загруженных PNG и создаёт общий материал.
/// Пока не все текстуры загружены, материал не создаётся.
fn ensure_ground_material(
    mut render: ResMut<TerrainRender>,
    textures: Res<TileTextures>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    if render.material.is_some() {
        return;
    }
    let Some(atlas) = build_atlas_image(&textures, &images) else {
        return;
    };

    let atlas = images.add(atlas);
    render.material = Some(materials.add(ColorMaterial {
        texture: Some(atlas),
        alpha_mode: AlphaMode2d::Opaque,
        ..default()
    }));
}

/// Собирает атлас 4×3 из одиннадцати тайлов с повтором краёв.
/// Возвращает `None`, пока хотя бы одна исходная текстура не загружена.
fn build_atlas_image(textures: &TileTextures, images: &Assets<Image>) -> Option<Image> {
    let mut data = vec![0_u8; (ATLAS_WIDTH * ATLAS_HEIGHT * 4) as usize];

    for (index, handle) in textures.atlas_sources().into_iter().enumerate() {
        let image = images.get(handle)?;
        let pixels = tile_pixels(image);
        let fallback = kind_for_atlas_index(index as u8)
            .map(|kind| color_to_rgba8(kind.placeholder_color()))
            .unwrap_or([255, 0, 255, 255]);
        write_atlas_cell(&mut data, index as u32, pixels, fallback);
    }
    let mut atlas = Image::new(
        Extent3d {
            width: ATLAS_WIDTH,
            height: ATLAS_HEIGHT,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    atlas.sampler = ImageSampler::nearest();
    Some(atlas)
}

/// Проверяет формат и размер изображения и возвращает его RGBA8-пиксели.
fn tile_pixels(image: &Image) -> Option<&[u8]> {
    if image.texture_descriptor.format != TextureFormat::Rgba8UnormSrgb
        || image.width() != ATLAS_TILE_PX || image.height() != ATLAS_TILE_PX
    {
        return None;
    }
    image.data.as_deref()
}

/// Записывает одну ячейку атласа: тайл 8×8 и повтор его краёв по 2 пикселя.
/// Если исходных пикселей нет, ячейка заливается запасным цветом.
fn write_atlas_cell(data: &mut [u8], index: u32, pixels: Option<&[u8]>, fallback: [u8; 4]) {
    let cell_x = (index % ATLAS_COLUMNS) * ATLAS_CELL_PX;
    let cell_y = (index / ATLAS_COLUMNS) * ATLAS_CELL_PX;
    let max_src = ATLAS_TILE_PX as i32 - 1;

    for dy in 0..ATLAS_CELL_PX {
        for dx in 0..ATLAS_CELL_PX {
            let sx = (dx as i32 - ATLAS_GUTTER_PX as i32).clamp(0, max_src) as u32;
            let sy = (dy as i32 - ATLAS_GUTTER_PX as i32).clamp(0, max_src) as u32;
            let dst = (((cell_y + dy) * ATLAS_WIDTH + cell_x + dx) * 4) as usize;
            let src = ((sy * ATLAS_TILE_PX + sx) * 4) as usize;
            match pixels.and_then(|pixels| pixels.get(src..src + 4)) {
                Some(pixel) => data[dst..dst + 4].copy_from_slice(pixel),
                None => data[dst..dst + 4].copy_from_slice(&fallback),
            }
        }
    }
}

/// Переводит цвет в байты RGBA8.
fn color_to_rgba8(color: [f32; 4]) -> [u8; 4] {
    [
        (color[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// UV-прямоугольник тайла в атласе: `[u0, v0, u1, v1]`, где v0 — верх.
pub(crate) fn atlas_uv(index: u8) -> [f32; 4] {
    let col = index as u32 % ATLAS_COLUMNS;
    let row = index as u32 / ATLAS_COLUMNS;
    let x0 = col * ATLAS_CELL_PX + ATLAS_GUTTER_PX;
    let y0 = row * ATLAS_CELL_PX + ATLAS_GUTTER_PX;
    [
        x0 as f32 / ATLAS_WIDTH as f32,
        y0 as f32 / ATLAS_HEIGHT as f32,
        (x0 + ATLAS_TILE_PX) as f32 / ATLAS_WIDTH as f32,
        (y0 + ATLAS_TILE_PX) as f32 / ATLAS_HEIGHT as f32,
    ]
}

/// Перерисовывает видимые блоки мира и их деревья.
fn render_world(
    mut commands: Commands,
    mut state: ResMut<GameState>,
    mut render: ResMut<TerrainRender>,
    mut meshes: ResMut<Assets<Mesh>>,
    images: Res<Assets<Image>>,
    textures: Res<TileTextures>,
    font: Res<UiFont>,
    camera: Query<(&Transform, &Projection), With<Camera2d>>,
) {
    // Мир перегенерирован: убираем обе сцены и начинаем с нуля.
    if state.pending_rerender {
        state.pending_rerender = false;
        state.dirty_tiles.clear();
        clear_all_blocks(&mut commands, &mut meshes, &mut render);
        render.blocks_per_side = state
            .world
            .as_ref()
            .map(|world| blocks_per_side(world.size.tiles() as i32))
            .unwrap_or(0);
        render.lod = 0;
    }

    // Изменённые тайлы изымаются до заимствования мира: список
    // обнуляется на месте, «расходуется» рендером.
    let dirty = std::mem::take(&mut state.dirty_tiles);

    let Some(world) = state.world.as_ref() else {
        return;
    };
    let Some(material) = render.material.clone() else {
        return;
    };
    // Точечная перестройка блоков с изменёнными тайлами (инструменты
    // редактора, добыча игрока): блок пересобирается целиком, вместе
    // с дочерними объектами.
    if !dirty.is_empty() {
        rebuild_dirty_blocks(
            &mut commands,
            &mut meshes,
            &images,
            world,
            &material,
            &textures,
            &font,
            &dirty,
            &mut render,
        );
    }
    let Ok((camera_transform, projection)) = camera.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    if ortho.area.width() <= 0.0 || ortho.area.height() <= 0.0 {
        return;
    }

    let camera_center = camera_transform.translation.truncate();
    let margin = Vec2::splat(VIEW_MARGIN_TILES * TILE_SIZE);
    let view_min = Vec2::new(ortho.area.min.x, ortho.area.min.y) + camera_center - margin;
    let view_max = Vec2::new(ortho.area.max.x, ortho.area.max.y) + camera_center + margin;

    let Some((bx0, by0, bx1, by1)) =
        visible_block_range(view_min, view_max, render.blocks_per_side)
    else {
        clear_all_blocks(&mut commands, &mut meshes, &mut render);
        return;
    };

    // Строим недостающие блоки от центра экрана наружу, с бюджетом на кадр.
    let mut visible = Vec::with_capacity(((bx1 - bx0 + 1) * (by1 - by0 + 1)) as usize);
    for by in by0..=by1 {
        for bx in bx0..=bx1 {
            visible.push(BlockPos { x: bx, y: by });
        }
    }
    let view_center = (view_min + view_max) * 0.5;
    visible.sort_by(|a, b| {
        block_distance_sq(*a, view_center).total_cmp(&block_distance_sq(*b, view_center))
    });

    let lod = lod_for_scale(ortho.scale);

    // Управление переходом: активная сцена остаётся на экране, пока строится новая.
    match lod_transition_for(lod, render.lod, render.incoming.as_ref().map(|inc| inc.lod)) {
        LodTransition::None | LodTransition::Continue => {}
        LodTransition::Cancel => drop_incoming(&mut commands, &mut meshes, &mut render),
        LodTransition::Start => {
            drop_incoming(&mut commands, &mut meshes, &mut render);
            render.incoming = Some(IncomingLod {
                lod,
                blocks: HashMap::new(),
            });
        }
    }

    // Убираем блоки активной сцены, которые перестали быть видимыми.
    let stale: Vec<BlockPos> = render
        .active
        .keys()
        .copied()
        .filter(|pos| pos.x < bx0 || pos.x > bx1 || pos.y < by0 || pos.y > by1)
        .collect();
    for pos in stale {
        if let Some(block) = render.active.remove(&pos) {
            despawn_block(&mut commands, &mut meshes, block);
        }
    }

    let budget = (BUILD_BLOCKS_BASE << lod.min(3)).min(BUILD_BLOCKS_MAX);
    let mut built_now = 0_usize;

    // Во время перехода активную сцену не наращиваем: она всё равно будет
    // заменена целиком, а бюджет нужен на новую. Достраивание работает,
    // только когда перехода нет.
    if render.incoming.is_none() {
        for block in visible.iter().copied() {
            if built_now >= budget {
                break;
            }
            if render.active.contains_key(&block) {
                continue;
            }
            let built = build_block(
                &mut commands, &mut meshes, &images, world, &material, &textures, &font, block, lod,
            );
            render.active.insert(block, built);
            built_now += 1;
        }
    }

    // Достраиваем сцену нового LOD; когда она покрывает весь вид — подменяем
    // активную одним кадром, чтобы на экране не было чёрных кусков.
    // Карта блоков временно изымается, чтобы не конфликтовать с &render.
    let mut swap_ready = false;
    if render.incoming.is_some() {
        let incoming = render.incoming.as_mut().unwrap();
        let incoming_lod = incoming.lod;
        let mut blocks = std::mem::take(&mut incoming.blocks);

        let mut missing = visible
            .iter()
            .filter(|block| !blocks.contains_key(block))
            .count();
        for block in visible.iter().copied() {
            if built_now >= budget {
                break;
            }
            if blocks.contains_key(&block) {
                continue;
            }
            let built = build_block(
                &mut commands, &mut meshes, &images, world, &material, &textures, &font, block,
                incoming_lod,
            );
            blocks.insert(block, built);
            built_now += 1;
            missing -= 1;
        }
        swap_ready = missing == 0;
        render.incoming.as_mut().unwrap().blocks = blocks;
    }
    if swap_ready {
        promote_incoming(
            &mut commands, &mut meshes, &mut render, bx0, by0, bx1, by1,
        );
    }
}

/// Удаляет строящуюся сцену нового LOD (отмена перехода или перезапуск).
fn drop_incoming(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    render: &mut TerrainRender,
) {
    if let Some(incoming) = render.incoming.take() {
        for (_, block) in incoming.blocks {
            despawn_block(commands, meshes, block);
        }
    }
}

/// Атомарно подменяет активную сцену строящейся: старая удаляется, новая
/// становится текущей. Блоки за пределами видимой области не переносятся.
fn promote_incoming(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    render: &mut TerrainRender,
    bx0: i32,
    by0: i32,
    bx1: i32,
    by1: i32,
) {
    let Some(incoming) = render.incoming.take() else {
        return;
    };
    for (_, block) in render.active.drain() {
        despawn_block(commands, meshes, block);
    }
    for (pos, block) in incoming.blocks {
        if pos.x < bx0 || pos.x > bx1 || pos.y < by0 || pos.y > by1 {
            despawn_block(commands, meshes, block);
        } else {
            render.active.insert(pos, block);
        }
    }
    render.lod = incoming.lod;
}

/// Удаляет все сцены (при перегенерации мира или уходе мира с экрана).
fn clear_all_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    render: &mut TerrainRender,
) {
    for (_, block) in render.active.drain() {
        despawn_block(commands, meshes, block);
    }
    drop_incoming(commands, meshes, render);
}

/// Перестраивает terrain-блоки, содержащие изменённые тайлы, — и в активной
/// сцене, и в строящейся нового LOD, если блок там уже построен. Блок
/// пересобирается целиком: объекты — дети сущности блока, так что и они
/// подхватят изменения. Вне бюджета построения: пользовательских правок
/// за кадр немного, а видимость результата должна быть мгновенной.
fn rebuild_dirty_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    images: &Assets<Image>,
    world: &World,
    material: &Handle<ColorMaterial>,
    textures: &TileTextures,
    font: &Res<UiFont>,
    dirty: &[TilePos],
    render: &mut TerrainRender,
) {
    let mut blocks: HashSet<BlockPos> = HashSet::with_capacity(dirty.len());
    for tile in dirty {
        blocks.insert(BlockPos {
            x: tile.x.div_euclid(BLOCK_TILES),
            y: tile.y.div_euclid(BLOCK_TILES),
        });
    }

    let stale: Vec<BlockPos> = render
        .active
        .keys()
        .copied()
        .filter(|pos| blocks.contains(pos))
        .collect();
    for pos in stale {
        if let Some(block) = render.active.remove(&pos) {
            despawn_block(commands, meshes, block);
            let built = build_block(commands, meshes, images, world, material, textures, font, pos, render.lod);
            render.active.insert(pos, built);
        }
    }

    if let Some(incoming) = render.incoming.as_mut() {
        let lod = incoming.lod;
        let stale: Vec<BlockPos> = incoming
            .blocks
            .keys()
            .copied()
            .filter(|pos| blocks.contains(pos))
            .collect();
        for pos in stale {
            if let Some(block) = incoming.blocks.remove(&pos) {
                despawn_block(commands, meshes, block);
                let built =
                    build_block(commands, meshes, images, world, material, textures, font, pos, lod);
                incoming.blocks.insert(pos, built);
            }
        }
    }
}

/// Обновляет спрайт игрока: существует и следует за сущностью в режиме
/// игрока, удаляется в наблюдателе (игрок «припаркован»). Позиция
/// интерполируется между тиками, иначе движение выглядит рывками.
/// Спрайт зеркалит текстуру по горизонтали при движении влево; при
/// отсутствии горизонтального ввода сохраняет последнее направление.
fn update_player_sprite(
    mut commands: Commands,
    state: Res<GameState>,
    player: Res<PlayerRes>,
    acc: Res<TickAccumulator>,
    input: Res<PlayerInput>,
    textures: Res<TileTextures>,
    mut last_flip: Local<bool>,
    mut existing: Query<(Entity, &mut Transform, &mut Sprite), With<PlayerSprite>>,
) {
    if input.dir.0 < 0.0 {
        *last_flip = true;
    } else if input.dir.0 > 0.0 {
        *last_flip = false;
    }

    let Some(p) = player.0.as_ref().filter(|_| state.mode == GameMode::Player) else {
        for (entity, _, _) in &mut existing {
            commands.entity(entity).despawn();
        }
        return;
    };
    let transform = player_transform(&player_render_pos(p, &acc), state.world_size.tiles());
    let mut updated = false;
    for (_, mut tf, mut sprite) in &mut existing {
        *tf = transform;
        sprite.flip_x = *last_flip;
        updated = true;
    }
    if !updated {
        commands.spawn((
            Sprite {
                image: textures.player_texture().clone(),
                custom_size: Some(Vec2::splat(PLAYER_SPRITE_PX)),
                flip_x: *last_flip,
                ..default()
            },
            transform,
            PlayerSprite,
        ));
    }
}

/// Обновляет спрайты животных: создаёт их для видимых существ, двигает
/// интерполяцией между тиками, меняет текстуру по состоянию и кадру
/// анимации, зеркалит при взгляде влево. Невидимые существа не держат
/// Bevy-сущностей — рендер читает только видимую область.
fn update_entity_sprites(
    mut commands: Commands,
    state: Res<GameState>,
    acc: Res<TickAccumulator>,
    time: Res<Time>,
    textures: Res<TileTextures>,
    mut sprites: ResMut<EntitySprites>,
    mut flashes: ResMut<HurtFlashes>,
    camera: Query<(&Transform, &Projection), With<Camera2d>>,
) {
    let Some(world) = state.world.as_ref() else {
        for (_, entity) in sprites.map.drain() {
            commands.entity(entity).despawn();
        }
        for (_, overlay) in flashes.overlays.drain() {
            commands.entity(overlay).despawn();
        }
        flashes.prev_hp.clear();
        flashes.timers.clear();
        return;
    };
    let Ok((camera_transform, projection)) = camera.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    if ortho.area.width() <= 0.0 || ortho.area.height() <= 0.0 {
        return;
    }

    // Таймеры вспышек урона гаснут по реальному времени.
    let delta = time.delta_secs();
    flashes.timers.retain(|_, timer| {
        *timer -= delta;
        *timer > 0.0
    });

    let camera_center = camera_transform.translation.truncate();
    let margin = Vec2::splat(VIEW_MARGIN_TILES * TILE_SIZE);
    let view_min = Vec2::new(ortho.area.min.x, ortho.area.min.y) + camera_center - margin;
    let view_max = Vec2::new(ortho.area.max.x, ortho.area.max.y) + camera_center + margin;

    let world_tiles = world.size.tiles();
    let mut visible = HashSet::with_capacity(world.entities.len());
    for entity in world.entities.iter() {
        let pos = interpolate_pos(entity.prev_pos, entity.pos, &acc);
        let x = pos.x * TILE_SIZE;
        let y = pos.y * TILE_SIZE;
        if x < view_min.x || x > view_max.x || y < view_min.y || y > view_max.y {
            continue;
        }
        visible.insert(entity.id);

        // Детект удара: HP стало меньше, чем на прошлом кадре — вспышка.
        let previous = flashes.prev_hp.insert(entity.id, entity.hp);
        if previous.is_some_and(|prev| entity.hp < prev) {
            flashes.timers.insert(entity.id, HURT_FLASH_SECS);
        }

        let sprite = Sprite {
            image: textures.entity_texture(entity).clone(),
            custom_size: Some(entity_sprite_size(entity.kind)),
            flip_x: entity.facing == Facing::Left,
            ..default()
        };
        let transform = entity_transform(&pos, world_tiles);

        match sprites.map.get(&entity.id).copied() {
            Some(bevy_entity) => {
                commands.entity(bevy_entity).insert((sprite, transform));
            }
            None => {
                let bevy_entity = commands
                    .spawn((sprite, transform, EntitySprite))
                    .id();
                sprites.map.insert(entity.id, bevy_entity);
            }
        }

        // Красная подсветка: копия текстуры с красной 30% альфой поверх —
        // краснеет именно рисунок, а не прямоугольник спрайта.
        let hurt = flashes.timers.contains_key(&entity.id);
        match flashes.overlays.get(&entity.id).copied() {
            Some(overlay) if hurt => {
                let overlay_sprite = Sprite {
                    image: textures.entity_texture(entity).clone(),
                    custom_size: Some(entity_sprite_size(entity.kind)),
                    color: HURT_TINT,
                    flip_x: entity.facing == Facing::Left,
                    ..default()
                };
                commands.entity(overlay).insert((overlay_sprite, transform));
            }
            Some(overlay) => {
                flashes.overlays.remove(&entity.id);
                commands.entity(overlay).despawn();
            }
            None if hurt => {
                let overlay_sprite = Sprite {
                    image: textures.entity_texture(entity).clone(),
                    custom_size: Some(entity_sprite_size(entity.kind)),
                    color: HURT_TINT,
                    flip_x: entity.facing == Facing::Left,
                    ..default()
                };
                let overlay = commands.spawn((overlay_sprite, transform)).id();
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
        // Существо покинуло экран или умерло: вспышка и overlay не нужны.
        flashes.prev_hp.remove(&id);
        flashes.timers.remove(&id);
        if let Some(overlay) = flashes.overlays.remove(&id) {
            commands.entity(overlay).despawn();
        }
    }
}

/// Позиция спрайта существа: та же Z-формула, что у игрока и объектов
/// (сортировка «за/перед» по Y, GAME_DESIGN 3.2).
fn entity_transform(pos: &WorldPos, world_tiles: u32) -> Transform {
    let x = pos.x * TILE_SIZE;
    let y = pos.y * TILE_SIZE;
    let z = 1.0 + (world_tiles as f32 * TILE_SIZE - y) * 0.001;
    Transform::from_xyz(x, y, z)
}

/// Размер спрайта животного в пикселях мира.
fn entity_sprite_size(kind: EntityKind) -> Vec2 {
    match kind {
        EntityKind::Wolf => Vec2::splat(WOLF_SPRITE_PX),
    }
}

/// Позиция спрайта игрока (в тайлах, уже интерполированная). Z повторяет
/// формулу объектов — сортировка «за/перед» по Y (2.5D, GAME_DESIGN 3.2).
fn player_transform(p: &WorldPos, world_tiles: u32) -> Transform {
    let x = p.x * TILE_SIZE;
    let y = p.y * TILE_SIZE;
    let z = 1.0 + (world_tiles as f32 * TILE_SIZE - y) * 0.001;
    Transform::from_xyz(x, y, z)
}

/// Деспавнит сущность блока и освобождает его меш.
fn despawn_block(commands: &mut Commands, meshes: &mut Assets<Mesh>, block: BuiltBlock) {
    commands.entity(block.entity).despawn();
    if let Some(mesh) = block.mesh {
        meshes.remove(&mesh);
    }
}

/// Строит один блок: меш по LOD, сущность с материалом и объекты блока.
fn build_block(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    images: &Assets<Image>,
    world: &World,
    material: &Handle<ColorMaterial>,
    textures: &TileTextures,
    font: &Res<UiFont>,
    block: BlockPos,
    lod: u32,
) -> BuiltBlock {
    let mesh = build_block_mesh(world, block, lod).map(|mesh| meshes.add(mesh));
    let origin = block_origin(block);

    let mut entity = commands.spawn((
        Transform::from_translation(origin),
        Visibility::default(),
        TerrainBlock,
    ));
    if let Some(handle) = mesh.clone() {
        entity.insert((Mesh2d(handle), MeshMaterial2d(material.clone())));
    }
    // Объекты рисуются только на ближних LOD: дальше они мельче ячейки сетки.
    if lod <= 1 {
        spawn_objects(&mut entity, images, world, block, origin, world.size.tiles(), textures);
        spawn_items(&mut entity, world, block, origin, world.size.tiles(), textures, &font.0);
    }

    BuiltBlock {
        entity: entity.id(),
        mesh,
    }
}

/// Сколько блоков по стороне нужно для мира из `tiles` тайлов.
fn blocks_per_side(tiles: i32) -> i32 {
    (tiles + BLOCK_TILES - 1) / BLOCK_TILES
}

/// Пересечение видимой области с сеткой блоков.
/// Возвращает `(bx0, by0, bx1, by1)` или `None`, если мир вне экрана.
fn visible_block_range(
    min: Vec2,
    max: Vec2,
    blocks_per_side: i32,
) -> Option<(i32, i32, i32, i32)> {
    if blocks_per_side <= 0 {
        return None;
    }

    let block_world = BLOCK_TILES as f32 * TILE_SIZE;
    let bx0 = (min.x / block_world).floor() as i32;
    let by0 = (min.y / block_world).floor() as i32;
    let bx1 = (max.x / block_world).ceil() as i32 - 1;
    let by1 = (max.y / block_world).ceil() as i32 - 1;

    let bx0 = bx0.max(0);
    let by0 = by0.max(0);
    let bx1 = bx1.min(blocks_per_side - 1);
    let by1 = by1.min(blocks_per_side - 1);

    if bx0 > bx1 || by0 > by1 {
        None
    } else {
        Some((bx0, by0, bx1, by1))
    }
}

/// Левый нижний угол блока в мировых координатах.
fn block_origin(block: BlockPos) -> Vec3 {
    Vec3::new(
        (block.x * BLOCK_TILES) as f32 * TILE_SIZE,
        (block.y * BLOCK_TILES) as f32 * TILE_SIZE,
        0.0,
    )
}

/// Квадрат расстояния от центра блока до точки (для порядка построения).
fn block_distance_sq(block: BlockPos, point: Vec2) -> f32 {
    let center = block_origin(block).truncate() + Vec2::splat(BLOCK_TILES as f32 * TILE_SIZE * 0.5);
    center.distance_squared(point)
}

/// Подбирает LOD так, чтобы агрегированная ячейка была не меньше `TARGET_CELL_PX`.
pub(crate) fn lod_for_scale(scale: f32) -> u32 {
    let ratio = (TARGET_CELL_PX * scale / TILE_SIZE).max(1.0);
    let lod = ratio.log2().ceil();
    if lod <= 0.0 {
        0
    } else {
        (lod as u32).min(MAX_LOD)
    }
}

/// Нужно ли рисовать счётчики лежащих предметов при данном масштабе камеры:
/// только на LOD 0 и только если на экране текст не мельче
/// `ITEM_COUNT_MIN_SCREEN_PX`. На крупных LOD текст не показывается вовсе.
pub(crate) fn item_counts_visible(scale: f32) -> bool {
    lod_for_scale(scale) == 0 && ITEM_COUNT_WORLD_PX / scale >= ITEM_COUNT_MIN_SCREEN_PX
}

fn update_item_count_labels(
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

/// Решение по переходу LOD на этот кадр.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LodTransition {
    /// Перехода нет: активный LOD совпадает с желаемым.
    None,
    /// Камера вернулась к активному LOD — строящуюся сцену отменяем.
    Cancel,
    /// Продолжаем строить сцену желаемого LOD.
    Continue,
    /// Желаемый LOD сменился в процессе строительства — начинаем заново.
    Start,
}

/// Сопоставляет желаемый LOD камеры, активный LOD и LOD строящейся сцены.
pub(crate) fn lod_transition_for(
    desired: u32,
    active: u32,
    incoming: Option<u32>,
) -> LodTransition {
    match (desired == active, incoming) {
        (true, None) => LodTransition::None,
        (true, Some(_)) => LodTransition::Cancel,
        (false, Some(lod)) if lod == desired => LodTransition::Continue,
        (false, _) => LodTransition::Start,
    }
}

/// Строит меш блока: по кваду на ячейку LOD с доминирующим типом тайла.
pub(crate) fn build_block_mesh(world: &World, block: BlockPos, lod: u32) -> Option<Mesh> {
    let cells = (BLOCK_TILES >> lod) as usize;
    let cell_tiles = 1_i32 << lod;
    let world_tiles = world.size.tiles() as i32;
    let base_x = block.x * BLOCK_TILES;
    let base_y = block.y * BLOCK_TILES;

    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(cells * cells * 4);
    let mut uvs: Vec<[f32; 2]> = Vec::with_capacity(cells * cells * 4);
    let mut indices: Vec<u32> = Vec::with_capacity(cells * cells * 6);

    // Добавляет квад ячейки (cx, cy) с текстурой `index`.
    let mut push_quad = |cx: usize, cy: usize, index: u8| {
        let x0 = cx as f32 * cell_tiles as f32 * TILE_SIZE;
        let y0 = cy as f32 * cell_tiles as f32 * TILE_SIZE;
        let x1 = x0 + cell_tiles as f32 * TILE_SIZE;
        let y1 = y0 + cell_tiles as f32 * TILE_SIZE;
        let [u0, v0, u1, v1] = atlas_uv(index);
        let base = positions.len() as u32;

        positions.extend([
            [x0, y0, 0.0],
            [x1, y0, 0.0],
            [x1, y1, 0.0],
            [x0, y1, 0.0],
        ]);
        uvs.extend([[u0, v1], [u1, v1], [u1, v0], [u0, v0]]);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    };

    for cy in 0..cells {
        for cx in 0..cells {
            let x0 = base_x + cx as i32 * cell_tiles;
            let y0 = base_y + cy as i32 * cell_tiles;

            // Быстрый путь: ячейка целиком внутри мира и одного чанка с
            // однородным рельефом — доминанта известна без обхода тайлов.
            if let Some(index) = uniform_cell_index(world, x0, y0, cell_tiles, world_tiles) {
                push_quad(cx, cy, index);
                continue;
            }
            let mut counts = [0_u32; TILE_KIND_COUNT];
            for y in y0..y0 + cell_tiles {
                for x in x0..x0 + cell_tiles {
                    if x >= world_tiles || y >= world_tiles {
                        continue;
                    }
                    if let Some(index) = tile_atlas_index(world.terrain_kind(TilePos { x, y })) {
                        counts[index as usize] += 1;
                    }
                }
            }

            if let Some(index) = dominant_atlas_index(&counts) {
                push_quad(cx, cy, index);
            }
        }
    }

    if positions.is_empty() {
        return None;
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(indices));
    Some(mesh)
}

/// Если ячейка `[x0, x0+cell) × [y0, y0+cell)` целиком лежит внутри мира
/// и внутри одного чанка с однородным рельефом, возвращает индекс атласа.
/// Иначе None — ячейку считает общий путь с подсчётом тайлов.
fn uniform_cell_index(world: &World, x0: i32, y0: i32, cell: i32, world_tiles: i32) -> Option<u8> {
    if x0 + cell > world_tiles || y0 + cell > world_tiles {
        return None;
    }
    let chunk_side = CHUNK_SIZE as i32;
    if cell > chunk_side
        || x0.rem_euclid(chunk_side) + cell > chunk_side
        || y0.rem_euclid(chunk_side) + cell > chunk_side
    {
        return None;
    }
    let cpos = ChunkPos {
        x: x0.div_euclid(chunk_side) as u32,
        y: y0.div_euclid(chunk_side) as u32,
    };
    match &world.chunk(cpos).terrain {
        TerrainTiles::Uniform(kind) => tile_atlas_index(*kind),
        TerrainTiles::Full(_) => None,
    }
}

/// Выбирает самый частый индекс тайла; при равенстве — меньший индекс.
pub(crate) fn dominant_atlas_index(counts: &[u32; TILE_KIND_COUNT]) -> Option<u8> {
    let mut best: Option<(u8, u32)> = None;
    for (index, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, best_count)) => count > best_count,
        };
        if better {
            best = Some((index as u8, count));
        }
    }
    best.map(|(index, _)| index)
}

/// Индекс тайла в атласе; `Empty` не рисуется.
pub(crate) fn tile_atlas_index(kind: TileType) -> Option<u8> {
    match kind {
        TileType::Empty => None,
        other => Some(other as u8 - 1),
    }
}

/// Обратное преобразование индекса атласа в тип тайла (для запасного цвета).
fn kind_for_atlas_index(index: u8) -> Option<TileType> {
    Some(match index {
        0 => TileType::Water,
        1 => TileType::Sand,
        2 => TileType::Dirt,
        3 => TileType::Grass,
        4 => TileType::ForestGrass,
        5 => TileType::Stone,
        6 => TileType::Clay,
        7 => TileType::Farmland,
        8 => TileType::DeepWater,
        9 => TileType::Ocean,
        10 => TileType::Mountain,
        _ => return None,
    })
}

/// Спавнит объекты блока как дочерние спрайты, сохраняя Z-сортировку по Y.
/// Идёт по разреженному слою объектов четырёх чанков блока — стоимость
/// пропорциональна числу объектов, а не тайлам блока.
fn spawn_objects(
    parent: &mut EntityCommands,
    images: &Assets<Image>,
    world: &World,
    block: BlockPos,
    origin: Vec3,
    world_tiles: u32,
    textures: &TileTextures,
) {
    let max_y = world_tiles as f32 * TILE_SIZE;
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
                    let tile = TilePos {
                        x: cpx * CHUNK_SIZE as i32 + (idx % CHUNK_SIZE as u8) as i32,
                        y: cpy * CHUNK_SIZE as i32 + (idx / CHUNK_SIZE as u8) as i32,
                    };
                    let handle = textures.object_texture(obj);
                    let size = images
                        .get(handle)
                        .map(|image| object_sprite_size(obj.kind, image))
                        .unwrap_or(Vec2::splat(TILE_SIZE));
                    // Спрайт центром по X над тайлом; низ прижат к нижней
                    // границе родного тайла (не ниже и не в воздухе).
                    let wx = tile.x as f32 * TILE_SIZE + TILE_SIZE * 0.5;
                    let wy = tile.y as f32 * TILE_SIZE + size.y * 0.5;
                    parent.spawn((
                        Sprite {
                            image: handle.clone(),
                            custom_size: Some(size),
                            ..default()
                        },
                        Transform::from_xyz(wx - origin.x, wy - origin.y, 1.0 + (max_y - wy) * 0.001),
                    ));
                }
            }
        }
    });
}

/// Размер спрайта объекта из размеров текстуры. 64 px текстуры
/// отображаются в 1.2 тайла (объекты чуть перекрывают свой тайл), поэтому
/// текстура 64×64 даёт 1.2×1.2 тайла, а высокая 64×128 (деревья) —
/// 1.2×2.4 тайла. Постройки (текстуры 16×16) занимают ровно свой тайл.
/// Текстура ещё не загружена — тайл-плейсхолдер.
fn object_sprite_size(kind: ObjectKind, image: &Image) -> Vec2 {
    if kind.is_building() {
        return Vec2::splat(TILE_SIZE);
    }
    const TILES_PER_64PX: f32 = 1.2;
    let scale = TILES_PER_64PX * TILE_SIZE / 64.0;
    Vec2::new(image.width() as f32 * scale, image.height() as f32 * scale)
}

/// Спавнит предметы на тайлах блока (дроп) как дочерние спрайты.
fn spawn_items(
    parent: &mut EntityCommands,
    world: &World,
    block: BlockPos,
    origin: Vec3,
    world_tiles: u32,
    textures: &TileTextures,
    font: &Handle<Font>,
) {
    let max_y = world_tiles as f32 * TILE_SIZE;
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
                    let (wx, wy) = tile_center(tile);
                    parent.spawn((
                        Sprite {
                            image: textures.item_texture(stack.kind).clone(),
                            custom_size: Some(Vec2::splat(TILE_SIZE * 0.6)),
                            ..default()
                        },
                        Transform::from_xyz(wx - origin.x, wy - origin.y, 1.0 + (max_y - wy) * 0.001),
                    ));
                    if stack.count > 1 {
                        // Счётчик растрируется крупным шрифтом и уменьшается
                        // трансформом: при font_size = 4 глифы состоят из
                        // нескольких пикселей и на экране выглядят мылом
                        // при любом зуме. 40 px с масштабом 0.1 дают тот же
                        // размер на экране, но с чёткой растеризацией.
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
                            Transform::from_xyz(
                                wx - origin.x + TILE_SIZE * 0.25,
                                wy - origin.y - TILE_SIZE * 0.25,
                                2.0 + (max_y - wy) * 0.001,
                            )
                            .with_scale(Vec3::splat(ITEM_COUNT_SCALE)),
                        ));
                    }
                }
            }
        }
    });
}

/// Центр тайла в мировых координатах.
fn tile_center(tile: TilePos) -> (f32, f32) {
    let x = tile.x as f32 * TILE_SIZE + TILE_SIZE * 0.5;
    let y = tile.y as f32 * TILE_SIZE + TILE_SIZE * 0.5;
    (x, y)
}

/// Маркер голограммы выбранной постройки.
#[derive(Component)]
struct BuildHologram;

/// Голограмма постройки из меню строительства: полупрозрачный спрайт
/// выбранного вида на тайле под курсором (привязка к сетке тайлов).
/// Зелёная — поставить можно (тайл свободен, не вода, в радиусе
/// взаимодействия и хватает предметов), красная — нельзя.
fn update_build_hologram(
    mut commands: Commands,
    state: Res<GameState>,
    construction: Res<ConstructionUi>,
    player: Res<PlayerRes>,
    textures: Res<TileTextures>,
    images: Res<Assets<Image>>,
    windows: Query<&Window>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    existing: Query<Entity, With<BuildHologram>>,
) {
    let selected = if construction.open && state.mode == GameMode::Player {
        construction.selected
    } else {
        None
    };
    let (Some(kind), Some(world), Some(p)) = (selected, state.world.as_ref(), player.0.as_ref())
    else {
        for entity in &existing {
            commands.entity(entity).despawn();
        }
        return;
    };

    let Ok(window) = windows.single() else {
        return;
    };
    // Курсор вне окна или за границами мира: голограмму рисовать не на чем.
    let Some(tile) = cursor_world_tile(window, &cameras, Some(world)) else {
        for entity in &existing {
            commands.entity(entity).despawn();
        }
        return;
    };

    // Правило установки дублирует проверку команды: голограмма —
    // предсказание результата, а не источник истины (финальную проверку
    // всё равно делает симуляция при применении команды).
    let affordable = build_cost(kind).is_none_or(|(item, count)| p.inventory.count_of(item) >= count);
    let valid = affordable && buildable_at(world, tile) && p.reachable(tile, world);

    let handle = textures.kind_texture(kind).clone();
    let size = images
        .get(&handle)
        .map(|image| object_sprite_size(kind, image))
        .unwrap_or(Vec2::splat(TILE_SIZE));
    // Спрайт стоит так же, как объекты: центр по X над тайлом, низ прижат
    // к нижней границе тайла; Z — по формуле 2.5D-сортировки.
    let wx = tile.x as f32 * TILE_SIZE + TILE_SIZE * 0.5;
    let wy = tile.y as f32 * TILE_SIZE + size.y * 0.5;
    let max_y = world.size.tiles() as f32 * TILE_SIZE;
    let z = 1.0 + (max_y - wy) * 0.001;
    let color = if valid {
        Color::srgba(0.45, 1.0, 0.45, 0.55)
    } else {
        Color::srgba(1.0, 0.35, 0.35, 0.55)
    };
    let sprite = Sprite {
        image: handle,
        custom_size: Some(size),
        color,
        ..default()
    };

    match existing.iter().next() {
        Some(entity) => {
            commands
                .entity(entity)
                .insert((sprite, Transform::from_xyz(wx, wy, z)));
        }
        None => {
            commands.spawn((sprite, Transform::from_xyz(wx, wy, z), BuildHologram));
        }
    }
}
