//! Terrain-блоки: сборка мешей 64×64 тайлов, LOD, dirty-перестройки и атлас.
//!
//! Видимые блоки рисуются одним мешем на блок: все тайлы блока — квады в
//! общей текстуре-атласе. При отдалении камеры соседние тайлы агрегируются
//! в ячейки (LOD), и в ячейке берётся доминирующий тип тайла. Смена LOD
//! атомарна: сцена нового уровня строится на фоне старой, и старая остаётся
//! на экране, пока новая не покроет весь вид — иначе по краям экрана мигали
//! бы чёрные куски недостроенных блоков. Спрайты объектов/предметов блока
//! спавнит `sprites.rs`; здесь блоки только собираются и перестраиваются.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

use crate::app::camera::WorldCamera;
use crate::app::state::GameState;
use crate::simulation::dirty::DirtyTiles;
use crate::simulation::{ChunkPos, TerrainTiles, TilePos, TileType, World, CHUNK_SIZE, TILE_SIZE};
use crate::ui::UiFont;

use super::isometric::{WORLD_TILE_SIZE, tile_elevation};
use super::sprites::spawn_block_decor;
use super::textures::TextureAtlases;

/// Сторона блока на самых крупных LOD.
pub(crate) const MAX_BLOCK_TILES: i32 = 64;

/// Сторона terrain-меша для данного LOD. Ближний LOD строится по чанкам
/// (16×16): правка одного тайла пересобирает маленький меш, а не блок
/// 64×64. Дальше размер растёт до 32 (LOD 1) и 64 (LOD 2+).
pub(crate) fn block_tiles_for_lod(lod: u32) -> i32 {
    match lod {
        0 => 16,
        1 => 32,
        _ => MAX_BLOCK_TILES,
    }
}

/// Пауза между пересборками dirty-мешей на крупных LOD, секунд (индекс —
/// LOD минус 3). На LOD ≤ 2 правки видны сразу; дальше частые пересборки
/// больших мешей на огромном мире давали рывки — обновления разрежаются.
const DIRTY_FLUSH_INTERVAL_SECS: [f32; 4] = [0.1, 0.2, 0.4, 0.8];

/// Предел очереди dirty-рельефа на дальних LOD. Массовые правки (эрозия,
/// рост травы на огромном мире на x100) могут порождать тысячи тайлов за
/// кадр; далёкий меш всё равно не показывает разницу в один тайл, поэтому
/// backlog сверх предела сбрасывается, а не копится в ОЗУ. При смене LOD
/// сцена соберётся заново из актуального мира.
const MAX_PENDING_DIRTY_TERRAIN: usize = 4096;

/// Сколько dirty-тайлов обрабатывать за одно обновление на крупном LOD.
/// Ограничение по блокам косвенное: несколько тайлов одного блока дают
/// одну пересборку. Остаток остаётся в очереди до следующего окна.
const DIRTY_FLUSH_TILES: [usize; 4] = [64, 32, 16, 8];

/// Желаемый размер агрегированной ячейки на экране, в пикселях.
/// Менялось вручную при тюнинге — тест LOD-порогов берёт значение отсюда.
pub(crate) const TARGET_CELL_PX: f32 = 2.0;

/// Максимальный уровень детализации (ячейка 2^MAX_LOD тайлов).
pub(crate) const MAX_LOD: u32 = 6;



/// Сколько блоков строить за кадр при LOD 0 (дальше бюджет растёт).
const BUILD_BLOCKS_BASE: usize = 4;

/// Верхняя граница бюджета построения блоков за кадр.
const BUILD_BLOCKS_MAX: usize = 32;

/// Максимальный LOD, на котором рисуются объекты и предметы блока.
///
/// Декор рисуется только вплотную к камере. На LOD 1 ячейка уже 2×2 тайла,
/// а видимая область большой карты (2048²) покрывает ~1800×1000 тайлов:
/// пер-тайловые спрайты давали десятки тысяч Bevy-сущностей и роняли FPS
/// даже при том, что сами предметы — точки в несколько пикселей. Дальше
/// LOD 0 декор не создаётся вовсе; вид на дальних планах задаёт рельеф.
const DECOR_MAX_LOD: u32 = 0;

/// Рисуется ли декор (объекты, культуры, предметы) на этом LOD.
pub(crate) fn decor_visible_at(lod: u32) -> bool {
    lod == DECOR_MAX_LOD
}

/// Запас вокруг видимой области, в тайлах (деревья выступают за свой тайл).
const VIEW_MARGIN_TILES: f32 = 10.0;

const TERRAIN_LENS_SHADER_PATH: &str = "shaders/terrain_lens.wesl";

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub(crate) struct TerrainLensExtension {}

impl MaterialExtension for TerrainLensExtension {
    fn fragment_shader() -> ShaderRef {
        TERRAIN_LENS_SHADER_PATH.into()
    }

    fn enable_prepass() -> bool {
        false
    }
}

pub(crate) type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainLensExtension>;

/// Материал terrain-блока: с линзами (расширенный шейдер + vertex color)
/// или обычный `StandardMaterial` без атрибута цвета. Цвет вершины — 16
/// байт на вершину и аллокация на каждый меш; когда линзы выключены, он
/// не нужен вовсе.
#[derive(Clone)]
pub(crate) enum TerrainMaterialHandle {
    Lens(Handle<TerrainMaterial>),
    Plain(Handle<StandardMaterial>),
}

impl TerrainMaterialHandle {
    /// Вставляет `MeshMaterial3d` нужного типа в сущность блока.
    fn insert(&self, entity: &mut EntityCommands) {
        match self {
            TerrainMaterialHandle::Lens(handle) => {
                entity.insert(MeshMaterial3d(handle.clone()));
            }
            TerrainMaterialHandle::Plain(handle) => {
                entity.insert(MeshMaterial3d(handle.clone()));
            }
        }
    }
}

/// Построенный terrain-блок: сущность и её меш (если в блоке есть тайлы).
pub(crate) struct BuiltBlock {
    pub(crate) entity: Entity,
    pub(crate) mesh: Option<Handle<Mesh>>,
    pub(crate) lod: u32,
    pub(crate) fallback: bool,
}

/// Координаты terrain-блока в сетке блоков.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct BlockPos {
    pub x: i32,
    pub y: i32,
}

/// Состояние рендера мира: материал, активные блоки и переход LOD.
#[derive(Resource, Default)]
pub(crate) struct TerrainRender {
    pub(crate) material: Option<Handle<TerrainMaterial>>,
    /// Обычный материал без vertex color (линзы выключены).
    pub(crate) plain_material: Option<Handle<StandardMaterial>>,
    /// Текущий режим мешей: `true` — с vertex color и lens-материалом.
    pub(crate) lens_mode: Option<bool>,
    /// Готовая сцена текущего LOD: видимые блоки.
    pub(crate) active: HashMap<BlockPos, BuiltBlock>,
    /// Строящаяся сцена нового LOD; активная остаётся на экране до её готовности.
    pub(crate) incoming: Option<IncomingLod>,
    pub(crate) lod: u32,
    /// Последний желаемый (по масштабу) LOD и сколько он держится: переход
    /// запускается только после `LOD_SETTLE_SECS`, чтобы зум колесом не
    /// перезапускал сборку сцены каждый кадр.
    pub(crate) desired_lod: u32,
    pub(crate) desired_lod_secs: f32,
    /// Множитель цвета terrain-тайлов по чанкам, задаваемый линзами.
    pub(crate) lens_tints: Arc<HashMap<ChunkPos, [f32; 4]>>,
    /// Очередь dirty-рельефа между обновлениями на крупных LOD: на LOD > 2
    /// пересборки больших мешей разрежаются, остаток ждёт следующего окна.
    /// Тайл остаётся здесь до фактической пересборки — потерять правку
    /// при отмене LOD-перехода или смене зума нельзя.
    pending_terrain: DirtyTiles,
    /// Накопленное время с последнего обновления dirty на крупном LOD.
    dirty_flush_secs: f32,
}

/// Сцена нового LOD, достраивающаяся на фоне активной.
pub(crate) struct IncomingLod {
    pub(crate) lod: u32,
    pub(crate) blocks: HashMap<BlockPos, BuiltBlock>,
}

/// Счётчики рендера для FPS-панели (GAME_DESIGN 9, ARCHITECTURE 9):
/// события dirty по каналам, перестройки блоков, время сборки мешей,
/// число видимых блоков и спрайтов. События и время накапливаются с
/// прошлого опроса панели; панель сбрасывает их, читая. Только для
/// отладки, на симуляцию не влияет.
#[derive(Resource, Default)]
pub(crate) struct RenderMetrics {
    /// Terrain-события dirty с прошлого опроса панели.
    pub(crate) terrain_events: u32,
    /// Decor-события dirty (объекты/предметы) с прошлого опроса панели.
    pub(crate) decor_events: u32,
    /// Полных перестроек terrain-блоков с прошлого опроса панели.
    pub(crate) rebuilds: u32,
    /// Суммарное время `build_block_mesh` с прошлого опроса панели, мс.
    pub(crate) mesh_build_ms: f32,
    /// Видимых terrain-блоков на текущий кадр.
    pub(crate) visible_blocks: u32,
    /// Спрайтов-детей terrain-блоков на текущий кадр (объекты, предметы,
    /// счётчики).
    pub(crate) block_sprites: u32,
    /// Включён ли расширенный режим F3. Панель выставляет флаг; вне F3
    /// дорогие счётчики (обход детей блоков) не считаются.
    pub(crate) extended: bool,
    /// Остаток до следующего пересчёта `block_sprites`, секунд.
    pub(crate) sprites_refresh_secs: f32,
}

/// Ключ видимой области: пока камера/окно не изменились, список видимых
/// блоков не пересобирается (сортировка и HashSet на 1000+ блоков — не
/// бесплатные). `step` входит в ключ: сетка видимых блоков зависит от LOD.
#[derive(Clone, Copy, PartialEq)]
struct VisibleKey {
    translation: Vec3,
    rotation: Quat,
    scale: f32,
    viewport: Vec2,
    world_tiles: i32,
    step: i32,
}

/// Кэш видимой области между кадрами.
#[derive(Default)]
pub(crate) struct VisibleCache {
    key: Option<VisibleKey>,
    blocks: Vec<BlockPos>,
    set: HashSet<BlockPos>,
}

/// Пауза стабилизации зума перед запуском перехода LOD: пока колесо
/// крутится, не начинаем новую сцену каждые несколько кадров.
const LOD_SETTLE_SECS: f32 = 0.12;

/// Метка сущности terrain-блока.
#[derive(Component)]
pub(crate) struct TerrainBlock;

pub(crate) fn ensure_ground_material(
    mut render: ResMut<TerrainRender>,
    atlases: Option<Res<TextureAtlases>>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    mut plain_materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(atlases) = atlases else {
        return;
    };
    if render.material.is_none() {
        render.material = Some(materials.add(ExtendedMaterial {
            base: StandardMaterial {
                base_color_texture: Some(atlases.tiles.image.clone()),
                alpha_mode: AlphaMode::Opaque,
                unlit: true,
                ..default()
            },
            extension: TerrainLensExtension {},
        }));
    }
    if render.plain_material.is_none() {
        render.plain_material = Some(plain_materials.add(StandardMaterial {
            base_color_texture: Some(atlases.tiles.image.clone()),
            alpha_mode: AlphaMode::Opaque,
            unlit: true,
            ..default()
        }));
    }
}

/// Перерисовывает видимые блоки мира и их деревья.
pub(crate) fn render_world(
    mut commands: Commands,
    time: Res<Time>,
    mut state: ResMut<GameState>,
    mut render: ResMut<TerrainRender>,
    mut metrics: ResMut<RenderMetrics>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    terrain_materials: Res<Assets<TerrainMaterial>>,
    mut sprite_assets: ResMut<super::sprites::WorldSpriteAssets>,
    atlases: Option<Res<TextureAtlases>>,
    font: Res<UiFont>,
    camera: Query<(&Camera, &Transform, &Projection), With<WorldCamera>>,
    terrain_blocks: Query<&Children, With<TerrainBlock>>,
    mut mesh_scratch: Local<BlockMeshScratch>,
    mut mesh_pool: Local<MeshBufferPool>,
    mut visible_caches: Local<VisibleCaches>,
) {
    // Мир перегенерирован: убираем обе сцены и начинаем с нуля. Очередь
    // dirty-рельефа старого мира тоже не нужна.
    if state.pending_rerender {
        state.pending_rerender = false;
        state.dirty_tiles.clear();
        clear_all_blocks(&mut commands, &mut meshes, &mut render, &mut mesh_pool);
        render.lod = 0;
        render.desired_lod = 0;
        render.desired_lod_secs = 0.0;
        render.pending_terrain.clear();
        render.dirty_flush_secs = 0.0;
    }

    // Изменённые тайлы изымаются до заимствования мира: список
    // обнуляется на месте, «расходуется» рендером.
    let dirty = std::mem::take(&mut state.dirty_tiles);
    metrics.terrain_events += dirty.terrain.len() as u32;
    metrics.decor_events += dirty.decor.len() as u32;

    let Some(world) = state.world.as_ref() else {
        return;
    };
    let lenses_enabled = state.lenses.sentients || state.lenses.buildings;
    // Материал и наличие vertex color зависят от линз: без линз меш не
    // несёт атрибут цвета (16 байт/вершину и аллокацию на блок).
    let material = if lenses_enabled {
        let Some(handle) = render.material.clone() else {
            return;
        };
        if terrain_materials.get(&handle).is_none() {
            return;
        }
        TerrainMaterialHandle::Lens(handle)
    } else {
        let Some(handle) = render.plain_material.clone() else {
            return;
        };
        if materials.get(&handle).is_none() {
            return;
        }
        TerrainMaterialHandle::Plain(handle)
    };
    if render.lens_mode != Some(lenses_enabled) {
        render.lens_mode = Some(lenses_enabled);
        clear_all_blocks(&mut commands, &mut meshes, &mut render, &mut mesh_pool);
    }
    let with_color = lenses_enabled;
    let Some(atlases) = atlases else {
        return;
    };
    let Ok((camera, camera_transform, projection)) = camera.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    let lens_tints = Arc::clone(&render.lens_tints);

    // Dirty-рельеф копится в очереди и обрабатывается по бюджету. На LOD 0–2
    // правки видны сразу (меши маленькие или их немного). На дальних LOD
    // пересборка 64×64-меша дорогая и на большом мире случалась бы каждый
    // кадр — обновления разрежаются, остаток ждёт следующего окна.
    // Тайлы блоков, которых нет в сцене (за кадром/ещё не построены),
    // отбрасываются: такой блок при появлении соберётся из актуального мира.
    if !dirty.terrain.is_empty() {
        let active_step = block_tiles_for_lod(render.lod);
        let incoming_step = render
            .incoming
            .as_ref()
            .map(|incoming| block_tiles_for_lod(incoming.lod));
        for tile in dirty.terrain {
            let active_block = BlockPos {
                x: tile.x.div_euclid(active_step),
                y: tile.y.div_euclid(active_step),
            };
            let in_active = render.active.contains_key(&active_block);
            let in_incoming = incoming_step.is_some_and(|step| {
                let block = BlockPos {
                    x: tile.x.div_euclid(step),
                    y: tile.y.div_euclid(step),
                };
                render
                    .incoming
                    .as_ref()
                    .is_some_and(|incoming| incoming.blocks.contains_key(&block))
            });
            if !in_active && !in_incoming {
                continue;
            }
            if render.pending_terrain.terrain.len() >= MAX_PENDING_DIRTY_TERRAIN {
                render.pending_terrain.clear();
            }
            render.pending_terrain.terrain(tile);
        }
    }
    let active_lod = render.lod;
    let mut dirty_batch: Vec<TilePos> = Vec::new();
    if !render.pending_terrain.terrain.is_empty() {
        let due = if active_lod <= 2 {
            true
        } else {
            render.dirty_flush_secs += time.delta_secs();
            render.dirty_flush_secs >= dirty_flush_interval(active_lod)
        };
        if due {
            // На ближних LOD очередь обрабатывается целиком; старое
            // накопленное время не должно ускорять первое окно после
            // отдаления.
            render.dirty_flush_secs = 0.0;
            dirty_batch = render
                .pending_terrain
                .take_terrain_batch(dirty_flush_tiles(active_lod));
        }
    }
    // Точечная перестройка terrain-блоков (инструменты редактора, добыча
    // травы, растительность): блок пересобирается целиком, вместе с
    // дочерними объектами.
    if !dirty_batch.is_empty() {
        rebuild_dirty_blocks(
            &mut commands, &mut meshes, &mut materials, &mut sprite_assets, world,
            &material, with_color, camera_transform.rotation, &atlases, &font,
            &dirty_batch, &mut render, &mut metrics, &lens_tints,
            &mut mesh_scratch, &mut mesh_pool,
        );
    }
    // Объекты и предметы (декор) не меняют геометрию меша: достаточно
    // пересобрать дочерние спрайты затронутых блоков.
    if !dirty.decor.is_empty() {
        refresh_decor(
            &mut commands, &mut meshes, &mut materials, &mut sprite_assets, world,
            camera_transform.rotation, &atlases, &font, &dirty.decor, &render, &mut mesh_scratch,
        );
    }
    if ortho.area.width() <= 0.0 || ortho.area.height() <= 0.0 {
        return;
    }
    let Some(viewport_size) = camera.logical_viewport_size() else {
        return;
    };
    // Проверяем проекцию 3D-границ каждого блока, а не пересечения лучей
    // только по углам viewport. Это остаётся консервативным для любого
    // промежуточного рельефа и не теряет блоки при панорамировании.
    let world_tiles = world.size.tiles() as i32;
    let half_view = viewport_size * (ortho.scale * 0.5);
    let margin = VIEW_MARGIN_TILES * WORLD_TILE_SIZE;

    let raw_lod = lod_for_scale(ortho.scale).min(if lenses_enabled { 4 } else { MAX_LOD });
    // Переход LOD стартует только после короткой паузы зума: иначе колесо
    // перезапускает `incoming` каждые несколько кадров и выбрасывает работу.
    if raw_lod == render.desired_lod {
        render.desired_lod_secs += time.delta_secs();
    } else {
        render.desired_lod = raw_lod;
        render.desired_lod_secs = 0.0;
    }
    let lod_settled = render.desired_lod_secs >= LOD_SETTLE_SECS;
    let lod = if lod_settled {
        raw_lod
    } else {
        render.incoming.as_ref().map_or(render.lod, |inc| inc.lod)
    };

    // Управление переходом: активная сцена остаётся на экране, пока строится новая.
    match lod_transition_for(lod, render.lod, render.incoming.as_ref().map(|inc| inc.lod)) {
        LodTransition::None | LodTransition::Continue => {}
        LodTransition::Cancel => {
            drop_incoming(&mut commands, &mut meshes, &mut render, &mut mesh_pool)
        }
        LodTransition::Start => {
            drop_incoming(&mut commands, &mut meshes, &mut render, &mut mesh_pool);
            render.incoming = Some(IncomingLod {
                lod, blocks: HashMap::new(),
            });
        }
    }

    // --- Активная сцена. Размер блока зависит от её LOD: 16 тайлов на LOD 0
    // (по чанкам), 32 на LOD 1, 64 на LOD 2+. ---
    let active_step = block_tiles_for_lod(render.lod);
    let active_blocks_per_side = blocks_per_side(world_tiles, active_step);
    let active_key = VisibleKey {
        translation: camera_transform.translation,
        rotation: camera_transform.rotation,
        scale: ortho.scale,
        viewport: viewport_size,
        world_tiles,
        step: active_step,
    };
    ensure_visible(
        &mut visible_caches.active,
        active_key,
        active_step,
        active_blocks_per_side,
        world_tiles,
        camera_transform,
        half_view,
        margin,
        ortho.near,
        ortho.far,
    );
    if visible_caches.active.blocks.is_empty() {
        clear_all_blocks(&mut commands, &mut meshes, &mut render, &mut mesh_pool);
        return;
    }

    // Новая часть viewport сразу получает дешёвый плоский placeholder,
    // вместо чёрного фона на время построения полноразмерного блока.
    let fallback_lod = render.lod;
    for block in visible_caches.active.blocks.iter().copied() {
        render
            .active
            .entry(block)
            .or_insert_with(|| build_fallback_block(
                &mut commands,
                &mut meshes,
                world,
                &material,
                block,
                fallback_lod,
                with_color,
                atlases.tile_uv(),
            ));
    }

    // Убираем блоки активной сцены, которые перестали быть видимыми.
    // Проверка работает и во время LOD-перехода: невидимые блоки не нужны
    // ни старой сцене (они за кадром), ни новой — а накопление таких блоков
    // с их пер-тайловым декором и было «утечкой» при быстром отдалении.
    let stale: Vec<BlockPos> = render
        .active
        .keys()
        .copied()
        .filter(|pos| !visible_caches.active.set.contains(pos))
        .collect();
    for pos in stale {
        if let Some(block) = render.active.remove(&pos) {
            despawn_block(&mut commands, &mut meshes, &mut mesh_pool, block);
        }
    }

    // Во время перехода активную сцену не наращиваем: она всё равно будет
    // заменена целиком, а бюджет нужен на новую. Достраивание работает,
    // только когда перехода нет. Пока зум не стабилизировался и желаемый
    // LOD отличается от активного, тоже не строим: иначе в паузе дебаунса
    // успели бы собраться блоки старого LOD с декором.
    let stale_lod = !lod_settled && raw_lod != render.lod;
    if render.incoming.is_none() && !stale_lod {
        let budget = build_budget(render.lod, active_step);
        let mut built_now = 0_usize;
        for block in visible_caches.active.blocks.iter().copied() {
            if built_now >= budget {
                break;
            }
            let needs_rebuild = render
                .active
                .get(&block)
                .is_some_and(|built| built.fallback || built.lod != render.lod);
            if !needs_rebuild {
                continue;
            }
            if let Some(old) = render.active.remove(&block) {
                despawn_block(&mut commands, &mut meshes, &mut mesh_pool, old);
            }
            let built = build_block(
                &mut commands, &mut meshes, &mut materials, &mut sprite_assets, world,
                &material, with_color, camera_transform.rotation, &atlases, &font,
                &mut metrics, block, render.lod, &lens_tints, &mut mesh_scratch,
                &mut mesh_pool,
            );
            render.active.insert(block, built);
            built_now += 1;
        }
    }

    // --- Сцена нового LOD: своя сетка блоков (сторона от LOD), своя
    // видимая область. Когда она покрывает весь вид — подменяем активную
    // одним кадром, чтобы на экране не было чёрных кусков. ---
    let mut swap_ready = false;
    if let Some(incoming_lod) = render.incoming.as_ref().map(|inc| inc.lod) {
        let incoming_step = block_tiles_for_lod(incoming_lod);
        let incoming_blocks_per_side = blocks_per_side(world_tiles, incoming_step);
        let incoming_key = VisibleKey {
            translation: camera_transform.translation,
            rotation: camera_transform.rotation,
            scale: ortho.scale,
            viewport: viewport_size,
            world_tiles,
            step: incoming_step,
        };
        ensure_visible(
            &mut visible_caches.incoming,
            incoming_key,
            incoming_step,
            incoming_blocks_per_side,
            world_tiles,
            camera_transform,
            half_view,
            margin,
            ortho.near,
            ortho.far,
        );
        let incoming = render.incoming.as_mut().unwrap();
        let mut blocks = std::mem::take(&mut incoming.blocks);
        let budget = build_budget(incoming_lod, incoming_step);
        let mut missing = visible_caches
            .incoming
            .blocks
            .iter()
            .filter(|block| !blocks.contains_key(block))
            .count();
        let mut built_now = 0_usize;
        for block in visible_caches.incoming.blocks.iter().copied() {
            if built_now >= budget {
                break;
            }
            if blocks.contains_key(&block) {
                continue;
            }
            let built = build_block(
                &mut commands, &mut meshes, &mut materials, &mut sprite_assets,
                world, &material, with_color,
                camera_transform.rotation, &atlases, &font, &mut metrics, block, incoming_lod,
                &lens_tints, &mut mesh_scratch, &mut mesh_pool,
            );
            commands.entity(built.entity).insert(Visibility::Hidden);
            blocks.insert(block, built);
            built_now += 1;
            missing -= 1;
        }
        swap_ready = missing == 0;
        render.incoming.as_mut().unwrap().blocks = blocks;
    }
    if swap_ready {
        promote_incoming(
            &mut commands,
            &mut meshes,
            &mut render,
            &mut mesh_pool,
            &visible_caches.incoming.set,
        );
    }

    // Абсолютные счётчики текущего кадра (панель показывает их без пересчёта).
    metrics.visible_blocks = render.active.len() as u32;
    // Обход всех детей блоков дорог (десятки тысяч). Он нужен только для
    // расширенной панели F3 и обновляется раз в 2 секунды.
    metrics.sprites_refresh_secs -= time.delta_secs();
    if metrics.extended && metrics.sprites_refresh_secs <= 0.0 {
        metrics.sprites_refresh_secs = 2.0;
        metrics.block_sprites = render
            .active
            .values()
            .filter_map(|block| terrain_blocks.get(block.entity).ok())
            .map(|children| children.len() as u32)
            .sum();
    }
}

/// Две видимые области: активная сцена и строящаяся сцена другого LOD.
/// Сетки блоков у них разные (16/32/64 тайлов), поэтому кэши раздельные.
#[derive(Default)]
pub(crate) struct VisibleCaches {
    active: VisibleCache,
    incoming: VisibleCache,
}

/// Пересобирает видимую область, если камера/окно/LOD изменились.
fn ensure_visible(
    cache: &mut VisibleCache,
    key: VisibleKey,
    step: i32,
    blocks_per_side: i32,
    world_tiles: i32,
    camera: &Transform,
    half_view: Vec2,
    margin: f32,
    near: f32,
    far: f32,
) {
    if cache.key == Some(key) {
        return;
    }
    let forward = camera.rotation * -Vec3::Z;
    let view_center = camera.translation + forward * (-camera.translation.z / forward.z);
    let view_center = view_center.truncate();
    cache.blocks.clear();
    collect_visible_blocks(
        0,
        0,
        blocks_per_side,
        blocks_per_side,
        world_tiles,
        step,
        camera,
        half_view,
        margin,
        near,
        far,
        &mut cache.blocks,
    );
    // Строим недостающие блоки от центра экрана наружу.
    cache.blocks.sort_by(|a, b| {
        block_distance_sq(*a, view_center, step).total_cmp(&block_distance_sq(*b, view_center, step))
    });
    cache.set.clear();
    cache.set.extend(cache.blocks.iter().copied());
    cache.key = Some(key);
}

/// Бюджет построения блоков за кадр. Меш ближнего блока в 16 раз меньше
/// меша 64×64-блока, поэтому бюджет считается «по площади»: одинаковый
/// объём геометрии за кадр независимо от размера блока, с потолком по
/// числу сущностей.
fn build_budget(lod: u32, step: i32) -> usize {
    let area_scale = (MAX_BLOCK_TILES * MAX_BLOCK_TILES) / (step * step);
    let base = (BUILD_BLOCKS_BASE << lod.min(3)).min(BUILD_BLOCKS_MAX);
    (base * area_scale as usize).min(BUILD_BLOCKS_MAX * 2)
}

/// Пауза между обновлениями dirty-мешей на LOD > 2.
fn dirty_flush_interval(lod: u32) -> f32 {
    DIRTY_FLUSH_INTERVAL_SECS[(lod.saturating_sub(3) as usize).min(DIRTY_FLUSH_INTERVAL_SECS.len() - 1)]
}

/// Сколько dirty-тайлов обработать за одно окно на LOD > 2.
fn dirty_flush_tiles(lod: u32) -> usize {
    if lod <= 2 {
        usize::MAX
    } else {
        DIRTY_FLUSH_TILES[(lod as usize - 3).min(DIRTY_FLUSH_TILES.len() - 1)]
    }
}

/// Удаляет строящуюся сцену нового LOD (отмена перехода или перезапуск).
fn drop_incoming(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    render: &mut TerrainRender,
    pool: &mut MeshBufferPool,
) {
    if let Some(incoming) = render.incoming.take() {
        for (_, block) in incoming.blocks {
            despawn_block(commands, meshes, pool, block);
        }
    }
}

/// Атомарно подменяет активную сцену строящейся: старая удаляется, новая
/// становится текущей. Блоки за пределами видимой области не переносятся.
fn promote_incoming(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    render: &mut TerrainRender,
    pool: &mut MeshBufferPool,
    visible: &HashSet<BlockPos>,
) {
    let Some(incoming) = render.incoming.take() else {
        return;
    };
    for (_, block) in render.active.drain() {
        despawn_block(commands, meshes, pool, block);
    }
    for (pos, block) in incoming.blocks {
        if !visible.contains(&pos) {
            despawn_block(commands, meshes, pool, block);
        } else {
            commands.entity(block.entity).insert(Visibility::Visible);
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
    pool: &mut MeshBufferPool,
) {
    for (_, block) in render.active.drain() {
        despawn_block(commands, meshes, pool, block);
    }
    drop_incoming(commands, meshes, render, pool);
}

/// Перестраивает terrain-блоки, содержащие изменённые тайлы, и блоки справа
/// или сверху, чьи видимые боковые грани зависят от изменённой высоты.
/// Размер блока берётся из LOD сцены: на LOD 0 правка пересобирает меш
/// чанка 16×16, а не блок 64×64. Вне бюджета построения: видимость правки
/// должна быть мгновенной на ближних LOD; на дальних частые вызовы
/// разрежает очередь `pending_terrain`.
fn rebuild_dirty_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut super::sprites::WorldSpriteAssets,
    world: &World,
    material: &TerrainMaterialHandle,
    with_color: bool,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Res<UiFont>,
    dirty: &[TilePos],
    render: &mut TerrainRender,
    metrics: &mut RenderMetrics,
    lens_tints: &HashMap<ChunkPos, [f32; 4]>,
    scratch: &mut BlockMeshScratch,
    pool: &mut MeshBufferPool,
) {
    let active_lod = render.lod;
    rebuild_dirty_scene(
        &mut render.active,
        active_lod,
        false,
        commands,
        meshes,
        materials,
        sprite_assets,
        world,
        material,
        with_color,
        camera_rotation,
        atlases,
        font,
        dirty,
        metrics,
        lens_tints,
        scratch,
        pool,
    );
    if let Some(incoming) = render.incoming.as_mut() {
        let lod = incoming.lod;
        rebuild_dirty_scene(
            &mut incoming.blocks,
            lod,
            true,
            commands,
            meshes,
            materials,
            sprite_assets,
            world,
            material,
            with_color,
            camera_rotation,
            atlases,
            font,
            dirty,
            metrics,
            lens_tints,
            scratch,
            pool,
        );
    }
}

/// Пересобирает затронутые блоки одной сцены (сетка block-позиций своя).
fn rebuild_dirty_scene(
    scene: &mut HashMap<BlockPos, BuiltBlock>,
    scene_lod: u32,
    hidden: bool,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut super::sprites::WorldSpriteAssets,
    world: &World,
    material: &TerrainMaterialHandle,
    with_color: bool,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Res<UiFont>,
    dirty: &[TilePos],
    metrics: &mut RenderMetrics,
    lens_tints: &HashMap<ChunkPos, [f32; 4]>,
    scratch: &mut BlockMeshScratch,
    pool: &mut MeshBufferPool,
) {
    let step = block_tiles_for_lod(scene_lod);
    // Буферы берутся из scratch и возвращаются обратно после перестройки:
    // LOD-0 правки рельефа случаются часто, аллокация HashSet на каждую —
    // заметный мусор для аллокатора.
    let mut blocks = std::mem::take(&mut scratch.blocks);
    let mut stale = std::mem::take(&mut scratch.stale);
    blocks.clear();
    stale.clear();
    blocks.reserve(dirty.len());
    for tile in dirty {
        let block = BlockPos {
            x: tile.x.div_euclid(step),
            y: tile.y.div_euclid(step),
        };
        blocks.insert(block);
        // Изменённая высота на краю блока видна в боковой грани соседа
        // справа/сверху (камера смотрит только на −X/−Y).
        let local_x = tile.x.rem_euclid(step);
        let local_y = tile.y.rem_euclid(step);
        if local_x == step - 1 {
            blocks.insert(BlockPos { x: block.x + 1, y: block.y });
        }
        if local_y == step - 1 {
            blocks.insert(BlockPos { x: block.x, y: block.y + 1 });
        }
    }
    stale.extend(scene.keys().copied().filter(|pos| blocks.contains(pos)));

    for &pos in &stale {
        // Fallback-блок — дешёвый placeholder: его заменит incoming-сцена
        // или бюджетная достройка. Полная сборка здесь вернула бы декор
        // старого LOD посреди отдаления (именно это и давало десятки тысяч
        // спрайтов при массовых правках рельефа).
        let Some(current) = scene.get(&pos) else {
            continue;
        };
        if current.fallback {
            continue;
        }
        let lod = current.lod;
        if let Some(block) = scene.remove(&pos) {
            despawn_block(commands, meshes, pool, block);
            let built = build_block(
                commands, meshes, materials, sprite_assets, world, material,
                with_color, camera_rotation, atlases, font, metrics, pos, lod,
                lens_tints, scratch, pool,
            );
            if hidden {
                commands.entity(built.entity).insert(Visibility::Hidden);
            }
            scene.insert(pos, built);
            metrics.rebuilds += 1;
        }
    }
    blocks.clear();
    stale.clear();
    scratch.blocks = blocks;
    scratch.stale = stale;
}

/// Обновляет декор (объекты и предметы) блоков с изменёнными тайлами, не
/// трогая terrain-меш: изменение объекта/предмета не меняет геометрию.
/// Дети блока снимаются целиком и спавнятся заново — тот же путь, что при
/// постройке блока. Декор рисуется только на LOD ≤ `DECOR_MAX_LOD`: на
/// дальних блоках и в fallback-блоках обновлять нечего.
fn refresh_decor(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut super::sprites::WorldSpriteAssets,
    world: &World,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Res<UiFont>,
    decor: &[TilePos],
    render: &TerrainRender,
    scratch: &mut BlockMeshScratch,
) {
    refresh_decor_scene(
        &render.active,
        render.lod,
        commands,
        meshes,
        materials,
        sprite_assets,
        world,
        camera_rotation,
        atlases,
        font,
        decor,
        scratch,
    );
    if let Some(incoming) = render.incoming.as_ref() {
        refresh_decor_scene(
            &incoming.blocks,
            incoming.lod,
            commands,
            meshes,
            materials,
            sprite_assets,
            world,
            camera_rotation,
            atlases,
            font,
            decor,
            scratch,
        );
    }
}

/// Пересобирает декор затронутых блоков одной сцены.
fn refresh_decor_scene(
    scene: &HashMap<BlockPos, BuiltBlock>,
    scene_lod: u32,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut super::sprites::WorldSpriteAssets,
    world: &World,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Res<UiFont>,
    decor: &[TilePos],
    scratch: &mut BlockMeshScratch,
) {
    let step = block_tiles_for_lod(scene_lod);
    let mut blocks = std::mem::take(&mut scratch.blocks);
    blocks.clear();
    blocks.reserve(decor.len());
    for tile in decor {
        blocks.insert(BlockPos {
            x: tile.x.div_euclid(step),
            y: tile.y.div_euclid(step),
        });
    }
    for &block in &blocks {
        if let Some(built) = scene.get(&block) {
            refresh_block_decor(
                built, commands, meshes, materials, sprite_assets, world,
                camera_rotation, atlases, font, block, step,
            );
        }
    }
    blocks.clear();
    scratch.blocks = blocks;
}

/// Пересобирает детей одного блока (объекты и предметы на земле).
fn refresh_block_decor(
    built: &BuiltBlock,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut super::sprites::WorldSpriteAssets,
    world: &World,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Res<UiFont>,
    block: BlockPos,
    step: i32,
) {
    if built.fallback || !decor_visible_at(built.lod) {
        return;
    }
    let Ok(mut entity) = commands.get_entity(built.entity) else {
        return;
    };
    entity.despawn_related::<Children>();
    let origin = block_origin(block, step);
    spawn_block_decor(
        &mut entity, meshes, materials, sprite_assets, world, block, step, origin, camera_rotation,
        atlases, &font.0,
    );
}

/// Деспавнит сущность блока, возвращает буферы меша в пул и освобождает
/// сам меш.
pub(crate) fn despawn_block(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    pool: &mut MeshBufferPool,
    block: BuiltBlock,
) {
    commands.entity(block.entity).despawn();
    let Some(handle) = block.mesh else {
        return;
    };
    // Fallback-меш имеет RenderAssetUsages::RENDER_WORLD: после extraction
    // атрибуты в MainWorld уже удалены, забирать их в пул нельзя.
    if block.fallback {
        meshes.remove(&handle);
    } else if let Some(mesh) = meshes.remove(&handle) {
        pool.reclaim(mesh);
    }
}

/// Строит один блок: меш по LOD, сущность с материалом и объекты блока.
fn build_block(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    sprite_assets: &mut super::sprites::WorldSpriteAssets,
    world: &World,
    material: &TerrainMaterialHandle,
    with_color: bool,
    camera_rotation: Quat,
    atlases: &TextureAtlases,
    font: &Res<UiFont>,
    metrics: &mut RenderMetrics,
    block: BlockPos,
    lod: u32,
    lens_tints: &HashMap<ChunkPos, [f32; 4]>,
    scratch: &mut BlockMeshScratch,
    pool: &mut MeshBufferPool,
) -> BuiltBlock {
    let started = std::time::Instant::now();
    let step = block_tiles_for_lod(lod);
    let mesh = build_block_mesh_cached(
        world, block, lod, step, with_color, atlases.tile_uv(), scratch, pool,
    )
    .map(|mut mesh| {
        if with_color && lod <= 4 && block_has_lens_tint(block, step, lens_tints) {
            set_block_lens_colors(&mut mesh, block, step, lens_tints);
        }
        meshes.add(mesh)
    });
    metrics.mesh_build_ms += started.elapsed().as_secs_f32() * 1000.0;
    let origin = block_origin(block, step);

    let mut entity = commands.spawn((
        Transform::from_translation(origin),
        Visibility::default(),
        TerrainBlock,
    ));
    if let Some(handle) = mesh.clone() {
        entity.insert(Mesh3d(handle));
        material.insert(&mut entity);
    }
    // Объекты рисуются только вплотную к камере: дальше они мельче ячейки
    // сетки, а их количество на большой карте растёт квадратично.
    if decor_visible_at(lod) {
        spawn_block_decor(
            &mut entity, meshes, materials, sprite_assets, world, block, step, origin,
            camera_rotation, atlases, &font.0,
        );
    }
    BuiltBlock {
        entity: entity.id(),
        mesh,
        lod,
        fallback: false,
    }
}

fn block_has_lens_tint(block: BlockPos, step: i32, tints: &HashMap<ChunkPos, [f32; 4]>) -> bool {
    let chunks_per_block = step / CHUNK_SIZE as i32;
    let chunk_x0 = (block.x * step).div_euclid(CHUNK_SIZE as i32);
    let chunk_y0 = (block.y * step).div_euclid(CHUNK_SIZE as i32);
    (0..chunks_per_block).any(|y| {
        (0..chunks_per_block).any(|x| {
            tints.contains_key(&ChunkPos {
                x: (chunk_x0 + x) as u32,
                y: (chunk_y0 + y) as u32,
            })
        })
    })
}

fn build_fallback_block(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    world: &World,
    material: &TerrainMaterialHandle,
    block: BlockPos,
    lod: u32,
    with_color: bool,
    tile_uv: &[[f32; 4]],
) -> BuiltBlock {
    let step = block_tiles_for_lod(lod);
    let mesh = build_fallback_block_mesh(world, block, step, with_color, tile_uv)
        .map(|mesh| meshes.add(mesh));
    let mut entity = commands.spawn((
        Transform::from_translation(block_origin(block, step)),
        Visibility::default(),
        TerrainBlock,
    ));
    if let Some(handle) = mesh.clone() {
        entity.insert(Mesh3d(handle));
        material.insert(&mut entity);
    }
    BuiltBlock {
        entity: entity.id(),
        mesh,
        // LOD fallback-блока — реальный LOD сетки: по нему выводится размер
        // блока (шаг) и решается, нужна ли полная пересборка.
        lod,
        fallback: true,
    }
}

fn build_fallback_block_mesh(
    world: &World,
    block: BlockPos,
    step: i32,
    with_color: bool,
    tile_uv: &[[f32; 4]],
) -> Option<Mesh> {
    let sample_step = (step / 8).max(1);
    let base_x = block.x * step;
    let base_y = block.y * step;
    let world_tiles = world.size.tiles() as i32;
    let tile_kinds = TileType::non_empty_count();
    let mut counts = vec![0_u32; tile_kinds];

    for y in (0..step).step_by(sample_step as usize) {
        for x in (0..step).step_by(sample_step as usize) {
            let tile = TilePos {
                x: base_x + x,
                y: base_y + y,
            };
            if tile.x >= world_tiles || tile.y >= world_tiles {
                continue;
            }
            if let Some(index) = tile_atlas_index(world.terrain_kind(tile)) {
                counts[index as usize] += 1;
            }
        }
    }
    let kind = kind_for_atlas_index(dominant_atlas_index(&counts)?)?;
    let index = tile_atlas_index(kind)?;
    let elevation = tile_elevation(kind);
    let width_tiles = (world_tiles - base_x).clamp(0, step) as f32;
    let height_tiles = (world_tiles - base_y).clamp(0, step) as f32;
    let points = [
        [0.0, 0.0, elevation],
        [width_tiles * WORLD_TILE_SIZE, 0.0, elevation],
        [
            width_tiles * WORLD_TILE_SIZE,
            height_tiles * WORLD_TILE_SIZE,
            elevation,
        ],
        [0.0, height_tiles * WORLD_TILE_SIZE, elevation],
    ];
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, points.to_vec());
    if with_color {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[1.0; 4]; points.len()]);
    }
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_UV_0,
        atlas_uv_coordinates(tile_uv[index as usize]).to_vec(),
    );
    mesh.insert_indices(Indices::U16([0, 1, 2, 0, 2, 3].into()));
    // Нормали не считаем: материал terrain unlit (см. ensure_ground_material),
    // шейдер без атрибута нормали компилируется без VERTEX_NORMALS.
    Some(mesh)
}

/// Сколько блоков по стороне нужно для мира из `tiles` тайлов.
fn blocks_per_side(tiles: i32, step: i32) -> i32 {
    (tiles + step - 1) / step
}

fn collect_visible_blocks(
    bx0: i32,
    by0: i32,
    bx1: i32,
    by1: i32,
    world_tiles: i32,
    step: i32,
    camera: &Transform,
    half_view: Vec2,
    margin: f32,
    near: f32,
    far: f32,
    visible: &mut Vec<BlockPos>,
) {
    if bx0 >= bx1 || by0 >= by1 || !block_region_intersects_view( bx0, by0, bx1, by1, world_tiles, step, camera, half_view, margin, near, far ) {
        return;
    }
    if bx1 - bx0 == 1 && by1 - by0 == 1 {
        visible.push(BlockPos { x: bx0, y: by0 });
        return;
    }

    let mid_x = bx0 + (bx1 - bx0 + 1) / 2;
    let mid_y = by0 + (by1 - by0 + 1) / 2;
    let x_ranges = if bx1 - bx0 > 1 {
        [(bx0, mid_x), (mid_x, bx1)]
    } else {
        [(bx0, bx1), (bx1, bx1)]
    };
    let y_ranges = if by1 - by0 > 1 {
        [(by0, mid_y), (mid_y, by1)]
    } else {
        [(by0, by1), (by1, by1)]
    };

    for (x0, x1) in x_ranges {
        for (y0, y1) in y_ranges {
            collect_visible_blocks(
                x0, y0, x1, y1, world_tiles, step, camera, half_view, margin, near, far, visible,
            );
        }
    }
}

fn block_region_intersects_view(
    bx0: i32,
    by0: i32,
    bx1: i32,
    by1: i32,
    world_tiles: i32,
    step: i32,
    camera: &Transform,
    half_view: Vec2,
    margin: f32,
    near: f32,
    far: f32,
) -> bool {
    let min = Vec3::new(
        (bx0 * step) as f32 * WORLD_TILE_SIZE,
        (by0 * step) as f32 * WORLD_TILE_SIZE,
        0.0,
    );
    let max = Vec3::new(
        (bx1 * step).min(world_tiles) as f32 * WORLD_TILE_SIZE,
        (by1 * step).min(world_tiles) as f32 * WORLD_TILE_SIZE,
        tile_elevation(TileType::Mountain),
    );
    let camera_position = camera.translation;
    let rotation = camera.rotation;
    let right = rotation * Vec3::X;
    let up = rotation * Vec3::Y;
    let forward = rotation * -Vec3::Z;

    let projected_range = |axis: Vec3| {
        let low = axis.x * if axis.x >= 0.0 { min.x } else { max.x }
            + axis.y * if axis.y >= 0.0 { min.y } else { max.y }
            + axis.z * if axis.z >= 0.0 { min.z } else { max.z }
            - axis.dot(camera_position);
        let high = axis.x * if axis.x >= 0.0 { max.x } else { min.x }
            + axis.y * if axis.y >= 0.0 { max.y } else { min.y }
            + axis.z * if axis.z >= 0.0 { max.z } else { min.z }
            - axis.dot(camera_position);
        (low, high)
    };
    let (screen_left, screen_right) = projected_range(right);
    let (screen_bottom, screen_top) = projected_range(up);
    let (depth_near, depth_far) = projected_range(forward);

    screen_right >= -half_view.x - margin
        && screen_left <= half_view.x + margin
        && screen_top >= -half_view.y - margin
        && screen_bottom <= half_view.y + margin
        && depth_far >= near
        && depth_near <= far
}

/// Начало блока в координатах 3D-сетки. `step` — сторона блока в тайлах
/// (зависит от LOD сцены).
pub(crate) fn block_origin(block: BlockPos, step: i32) -> Vec3 {
    Vec3::new(
        (block.x * step) as f32,
        (block.y * step) as f32,
        0.0,
    ) * WORLD_TILE_SIZE
}

/// Квадрат расстояния от центра блока до точки (для порядка построения).
fn block_distance_sq(block: BlockPos, point: Vec2, step: i32) -> f32 {
    let base = Vec2::new(
        (block.x * step) as f32,
        (block.y * step) as f32,
    );
    let center = (base + Vec2::splat(step as f32 * 0.5)) * WORLD_TILE_SIZE;
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

/// Переиспользуемые буферы сборки меша блока: `counts` и `kinds` не
/// зависят от конкретного блока и при массовой перестройке LOD не должны
/// заново выделяться на каждый блок.
#[derive(Default)]
pub(crate) struct BlockMeshScratch {
    counts: Vec<u32>,
    kinds: Vec<Option<TileType>>,
    /// Множество затронутых блоков при dirty-перестройке (не выделяется
    /// на каждую правку тайла).
    blocks: HashSet<BlockPos>,
    /// Буфер списка блоков для удаления/перестройки.
    stale: Vec<BlockPos>,
}

/// Пул вершинных буферов terrain-мешей. `Mesh` владеет своими `Vec`, поэтому
/// при удалении блока атрибуты извлекаются обратно (`despawn_block`) и
/// используются для следующего меша. Без пула каждая пересборка LOD-0 блока
/// аллоцировала позиции/UV/индексы/цвет заново.
#[derive(Default)]
pub(crate) struct MeshBufferPool {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u16>,
    colors: Vec<[f32; 4]>,
}

impl MeshBufferPool {
    /// Забирает буферы под новую сборку (они пустеют, ёмкость сохраняется).
    fn take(&mut self) -> MeshBuffers {
        MeshBuffers {
            positions: std::mem::take(&mut self.positions),
            uvs: std::mem::take(&mut self.uvs),
            indices: std::mem::take(&mut self.indices),
            colors: std::mem::take(&mut self.colors),
        }
    }

    /// Возвращает буферы удалённого меша в пул (берётся самая большая
    /// ёмкость, чтобы пул не деградировал при чередовании LOD).
    fn reclaim(&mut self, mut mesh: Mesh) {
        if let Some(VertexAttributeValues::Float32x3(values)) =
            mesh.remove_attribute(Mesh::ATTRIBUTE_POSITION)
            && values.capacity() > self.positions.capacity()
        {
            self.positions = values;
        }
        if let Some(VertexAttributeValues::Float32x2(values)) =
            mesh.remove_attribute(Mesh::ATTRIBUTE_UV_0)
            && values.capacity() > self.uvs.capacity()
        {
            self.uvs = values;
        }
        if let Some(VertexAttributeValues::Float32x4(values)) =
            mesh.remove_attribute(Mesh::ATTRIBUTE_COLOR)
            && values.capacity() > self.colors.capacity()
        {
            self.colors = values;
        }
        if let Some(indices) = mesh.indices_mut() {
            let old = std::mem::replace(indices, Indices::U16(Vec::new()));
            if let Indices::U16(values) = old
                && values.capacity() > self.indices.capacity()
            {
                self.indices = values;
            }
        }
    }
}

/// Буферы одной сборки, изъятые из пула.
struct MeshBuffers {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u16>,
    colors: Vec<[f32; 4]>,
}

/// Публичная обёртка без внешнего scratch — для тестов и разовых вызовов.
#[allow(dead_code)]
pub(crate) fn build_block_mesh(
    world: &World,
    block: BlockPos,
    lod: u32,
    tile_uv: &[[f32; 4]],
) -> Option<Mesh> {
    let step = block_tiles_for_lod(lod);
    build_block_mesh_cached(
        world,
        block,
        lod,
        step,
        true,
        tile_uv,
        &mut BlockMeshScratch::default(),
        &mut MeshBufferPool::default(),
    )
}

/// Строит меш блока с переиспользованием рабочих буферов. `step` — сторона
/// блока в тайлах для данного LOD (16/32/64): на ближнем LOD dirty-правка
/// пересобирает только меш чанка.
pub(crate) fn build_block_mesh_cached(
    world: &World,
    block: BlockPos,
    lod: u32,
    step: i32,
    with_color: bool,
    tile_uv: &[[f32; 4]],
    scratch: &mut BlockMeshScratch,
    pool: &mut MeshBufferPool,
) -> Option<Mesh> {
    let cells = (step >> lod) as usize;
    let cell_tiles = 1_i32 << lod;
    let world_tiles = world.size.tiles() as i32;
    let base_x = block.x * step;
    let base_y = block.y * step;
    let tile_kinds = TileType::non_empty_count();
    // Один буфер счётчиков на весь блок: `cell_kind` очищает его на месте,
    // поэтому горячий путь не выделяет память на каждую ячейку.
    scratch.counts.clear();
    scratch.counts.resize(tile_kinds, 0);
    scratch.kinds.clear();
    scratch.kinds.resize(cells * cells, None);
    let counts = &mut scratch.counts;
    let kinds = &mut scratch.kinds;

    for cy in 0..cells {
        for cx in 0..cells {
            let x0 = base_x + cx as i32 * cell_tiles;
            let y0 = base_y + cy as i32 * cell_tiles;
            kinds[cy * cells + cx] = cell_kind(world, x0, y0, cell_tiles, world_tiles, counts);
        }
    }
    // Буферы берутся из пула: при пересборке блоков их ёмкость переходит
    // от удалённых мешей, а не выделяется заново.
    let MeshBuffers {
        mut positions,
        mut uvs,
        mut indices,
        mut colors,
    } = pool.take();
    positions.clear();
    uvs.clear();
    indices.clear();
    colors.clear();
    positions.reserve(cells * cells * 4);
    uvs.reserve(cells * cells * 4);
    indices.reserve(cells * cells * 6);
    let mut push_quad = |points: [[f32; 3]; 4], texture_uvs: [[f32; 2]; 4]| {
        let base = positions.len() as u16;
        positions.extend(points);
        uvs.extend(texture_uvs);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    let point = |x: f32, y: f32, elevation: f32| {
        [x * WORLD_TILE_SIZE, y * WORLD_TILE_SIZE, elevation]
    };
    for cy in 0..cells {
        for cx in 0..cells {
            let Some(kind) = kinds[cy * cells + cx] else {
                continue;
            };
            let Some(index) = tile_atlas_index(kind) else {
                continue;
            };
            let x0 = (base_x + cx as i32 * cell_tiles) as f32;
            let y0 = (base_y + cy as i32 * cell_tiles) as f32;
            let lx0 = cx as f32 * cell_tiles as f32;
            let ly0 = cy as f32 * cell_tiles as f32;
            let lx1 = lx0 + cell_tiles as f32;
            let ly1 = ly0 + cell_tiles as f32;
            let elevation = tile_elevation(kind);

            push_quad(
                [
                    point(lx0, ly0, elevation),
                    point(lx1, ly0, elevation),
                    point(lx1, ly1, elevation),
                    point(lx0, ly1, elevation),
                ],
                atlas_uv_coordinates(tile_uv[index as usize]),
            );

            let edges = [
                (cx as i32 - 1, cy as i32, x0 as i32 - cell_tiles, y0 as i32, 0_u8),
                (cx as i32, cy as i32 - 1, x0 as i32, y0 as i32 - cell_tiles, 2),
            ];
            for (nx, ny, neighbor_x, neighbor_y, edge) in edges {
                let neighbor_kind = if nx >= 0
                    && ny >= 0
                    && nx < cells as i32
                    && ny < cells as i32
                {
                    kinds[ny as usize * cells + nx as usize]
                } else {
                    cell_kind(
                        world,
                        neighbor_x,
                        neighbor_y,
                        cell_tiles,
                        world_tiles,
                        counts,
                    )
                };
                let neighbor_elevation = neighbor_kind.map(tile_elevation).unwrap_or(0.0);
                if neighbor_elevation >= elevation {
                    continue;
                }
                let wall = match edge {
                    0 => [
                        point(lx0, ly1, neighbor_elevation),
                        point(lx0, ly0, neighbor_elevation),
                        point(lx0, ly0, elevation),
                        point(lx0, ly1, elevation),
                    ],
                    _ => [
                        point(lx0, ly0, neighbor_elevation),
                        point(lx1, ly0, neighbor_elevation),
                        point(lx1, ly0, elevation),
                        point(lx0, ly0, elevation),
                    ],
                };
                push_quad(wall, atlas_uv_coordinates(tile_uv[index as usize]));
            }
        }
    }

    if positions.is_empty() {
        // Пустой блок: вернуть буферы в пул (иначе ёмкость пропадёт).
        if positions.capacity() > pool.positions.capacity() {
            pool.positions = positions;
        }
        if uvs.capacity() > pool.uvs.capacity() {
            pool.uvs = uvs;
        }
        if indices.capacity() > pool.indices.capacity() {
            pool.indices = indices;
        }
        if colors.capacity() > pool.colors.capacity() {
            pool.colors = colors;
        }
        return None;
    }

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        // Линзы переокрашивают вершины после extraction, поэтому terrain
        // сохраняет атрибуты в MainWorld до удаления блока.
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    );
    let vertex_count = positions.len();
    // Меш блока не превышает 3072 вершин (16×16 ячеек × до 3 квадов):
    // u16-индексы вдвое компактнее u32 и меньше грузят индексовый буфер.
    debug_assert!(vertex_count <= u16::MAX as usize, "terrain block fits u16 indices");
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    if with_color {
        // Цвет вершины нужен только линзам; без них это 16 байт/вершину.
        colors.resize(vertex_count, [1.0; 4]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    } else if colors.capacity() > pool.colors.capacity() {
        // Буфер цвета не понадобился — возвращаем его в пул.
        pool.colors = colors;
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U16(indices));
    // Нормали не считаем: материал terrain unlit, освещение по нормалям
    // не считается, а без атрибута Bevy не включает VERTEX_NORMALS в шейдере.
    // Это ~2/3 стоимости сборки меша (профиль: compute_normals был 1.38%).
    Some(mesh)
}

/// Обновляет только цвета terrain-квадов, не меняя геометрию, UV или
/// глубину. При включённых линзах LOD не выше размера чанка, поэтому каждый
/// квад полностью принадлежит ровно одному чанку.
pub(crate) fn set_block_lens_colors(
    mesh: &mut Mesh,
    block: BlockPos,
    step: i32,
    tints: &HashMap<ChunkPos, [f32; 4]>,
) {
    let positions = mesh
        .attribute(Mesh::ATTRIBUTE_POSITION)
        .and_then(|attribute| attribute.as_float3())
        .expect("terrain mesh has float3 positions");
    assert_eq!(positions.len() % 4, 0, "terrain mesh consists of quads");
    let block_x = (block.x * step) as f32;
    let block_y = (block.y * step) as f32;
    let mut colors = Vec::with_capacity(positions.len());
    for quad in positions.as_chunks::<4>().0 {
        let center_x = quad.iter().map(|point| point[0]).sum::<f32>()
            / (4.0 * WORLD_TILE_SIZE)
            + block_x
            + 1e-4;
        let center_y = quad.iter().map(|point| point[1]).sum::<f32>()
            / (4.0 * WORLD_TILE_SIZE)
            + block_y
            + 1e-4;
        let tile = TilePos {
            x: center_x.floor() as i32,
            y: center_y.floor() as i32,
        };
        let chunk = ChunkPos {
            x: tile.x.div_euclid(CHUNK_SIZE as i32) as u32,
            y: tile.y.div_euclid(CHUNK_SIZE as i32) as u32,
        };
        let color = tints.get(&chunk).copied().unwrap_or([1.0; 4]);
        colors.extend([color; 4]);
    }
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
}

fn atlas_uv_coordinates(uv: [f32; 4]) -> [[f32; 2]; 4] {
    let [u0, v0, u1, v1] = uv;
    [[u0, v1], [u1, v1], [u1, v0], [u0, v0]]
}

fn cell_kind(
    world: &World,
    x0: i32,
    y0: i32,
    cell: i32,
    world_tiles: i32,
    counts: &mut [u32],
) -> Option<TileType> {
    let chunk_side = CHUNK_SIZE as i32;
    // Ячейка LOD 0–4 (1–16 тайлов) целиком лежит в одном чанке: берём
    // `TerrainTiles` один раз и читаем тайлы напрямую, без `split_tile`
    // и повторного поиска чанка на каждый тайл.
    if cell <= chunk_side
        && x0 >= 0
        && y0 >= 0
        && x0 + cell <= world_tiles
        && y0 + cell <= world_tiles
        && x0.rem_euclid(chunk_side) + cell <= chunk_side
        && y0.rem_euclid(chunk_side) + cell <= chunk_side
    {
        let cpos = ChunkPos {
            x: x0.div_euclid(chunk_side) as u32,
            y: y0.div_euclid(chunk_side) as u32,
        };
        let local_x = x0.rem_euclid(chunk_side) as usize;
        let local_y = y0.rem_euclid(chunk_side) as usize;
        return match &world.chunk(cpos).terrain {
            TerrainTiles::Uniform(kind) => tile_atlas_index(*kind).map(|_| *kind),
            TerrainTiles::Full(tiles) => {
                counts.fill(0);
                for y in 0..cell as usize {
                    let row = (local_y + y) * chunk_side as usize;
                    for x in 0..cell as usize {
                        let kind = tiles[row + local_x + x];
                        if let Some(index) = tile_atlas_index(kind) {
                            counts[index as usize] += 1;
                        }
                    }
                }
                dominant_atlas_index(counts).and_then(kind_for_atlas_index)
            }
        };
    }
    // Крупная ячейка (LOD 5–6) пересекает чанки: общий путь с подсчётом.
    counts.fill(0);
    for y in y0..y0 + cell {
        for x in x0..x0 + cell {
            if x < 0 || y < 0 || x >= world_tiles || y >= world_tiles {
                continue;
            }
            if let Some(index) = tile_atlas_index(world.terrain_kind(TilePos { x, y })) {
                counts[index as usize] += 1;
            }
        }
    }
    dominant_atlas_index(counts).and_then(kind_for_atlas_index)
}

/// Выбирает самый частый индекс тайла; при равенстве — меньший индекс.
pub(crate) fn dominant_atlas_index(counts: &[u32]) -> Option<u8> {
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

/// Обратное преобразование индекса атласа в тип тайла. Индексы идут
/// подряд за `Empty` (`tile_atlas_index` = дискриминант − 1), поэтому
/// обратное отображение выводится из `TileType::from_discriminant` —
/// новый вид тайла не требует правки здесь.
fn kind_for_atlas_index(index: u8) -> Option<TileType> {
    TileType::from_discriminant(index + 1)
}
