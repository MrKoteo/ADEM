//! Освещение: ночное затемнение мира и «свет» костра.
//!
//! Тьма — один оверлей поверх мира. Фрагментный шейдер затемняет каждый пиксель
//! чёрным с альфой, поэтому фактура и оттенки мира сохраняются. Он вычисляет
//! ослабление тьмы от каждого костра напрямую, без дискретизации сеткой.
//!
//! UI рисуется отдельным проходом поверх мира и не затемняется.

use std::collections::HashSet;

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Camera2d, Projection};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::{render_resource::AsBindGroup, storage::ShaderBuffer};
use bevy::shader::ShaderRef;
use bevy::sprite_render::{AlphaMode2d, Material2d};

use crate::app::settings::Settings;
use crate::app::state::{ClockRes, GameState};
use crate::rendering::isometric::{WORLD_TILE_SIZE, world_position};
use crate::simulation::moon;
use crate::simulation::{BuildingKind, ChunkPos, ItemKind, ObjectKind, TilePos, World, WorldPos, CHUNK_SIZE};

/// Радиус света костра в тайлах.
pub(crate) const LIGHT_RADIUS_TILES: f32 = 5.0;

/// Радиус света в мировых пикселях.
const LIGHT_RADIUS_PX: f32 = LIGHT_RADIUS_TILES * WORLD_TILE_SIZE;
/// Радиус света факела в тайлах.
const TORCH_LIGHT_RADIUS_TILES: f32 = 2.5;
const TORCH_LIGHT_RADIUS_PX: f32 = TORCH_LIGHT_RADIUS_TILES * WORLD_TILE_SIZE;
/// Сила света костра; shader нормализует силу к этому значению.
const CAMPFIRE_LIGHT_STRENGTH: f32 = 5.0;
/// Период обновления позиций факелов, секунд. Выбор предмета проверяется
/// отдельно каждый кадр для захваченного существа, поэтому 10 Гц достаточно
/// для фонового пересканирования NPC.
const TORCH_POSITION_INTERVAL_SECS: f32 = 0.1;

/// Границы переходов суток в часах: рассвет 5–7, закат 18–20.
const DAWN_START_H: f32 = 5.0;
const DAWN_END_H: f32 = 7.0;
const DUSK_START_H: f32 = 18.0;
const DUSK_END_H: f32 = 20.0;

const DARKNESS_SHADER_PATH: &str = "shaders/darkness.wesl";

/// Светлота мира по доле суток [0, 1): 1.0 днём (7:00–18:00),
/// `night_daylight` глухой ночью (20:00–5:00), между ними — плавные рассвет
/// и закат (smoothstep). Ночной уровень задаёт фаза луны (0.03–0.24).
/// Затенение равно `1 − daylight`.
pub(crate) fn daylight_factor(day_fraction: f32, night_daylight: f32) -> f32 {
    let hour = day_fraction.fract() * 24.0;
    let t = if hour >= DUSK_END_H || hour < DAWN_START_H {
        0.0
    } else if hour < DAWN_END_H {
        smoothstep((hour - DAWN_START_H) / (DAWN_END_H - DAWN_START_H))
    } else if hour < DUSK_START_H {
        1.0
    } else {
        1.0 - smoothstep((hour - DUSK_START_H) / (DUSK_END_H - DUSK_START_H))
    };
    night_daylight + (1.0 - night_daylight) * t
}

/// Классический smoothstep на [0, 1].
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub(crate) struct DarknessMaterial {
    #[uniform(0)]
    params: Vec4,
    #[storage(1, read_only)]
    fire_positions: Handle<ShaderBuffer>,
}

impl Material2d for DarknessMaterial {
    fn fragment_shader() -> ShaderRef {
        DARKNESS_SHADER_PATH.into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

/// Реестр горящих костров мира (тайлы). Костров мало, а изменения
/// объектов проходят через команды и оставляют dirty-тайлы — реестр
/// сверяется по ним, без обхода мира каждый кадр. Факелы в руках — всех
/// разумных существ, не только захваченного: динамический свет принадлежит
/// людям, а не режиму управления.
#[derive(Resource, Default)]
pub(crate) struct CampfireLights {
    positions: HashSet<TilePos>,
    /// Позиции факелов в руках людей (мировые координаты).
    torch_positions: Vec<WorldPos>,
    /// Переиспользуемый буфер пересканирования факелов.
    torch_scratch: Vec<WorldPos>,
    /// Был ли факел в руке захваченного существа на прошлом кадре: смена
    /// слота форсирует пересканирование, не дожидаясь периода.
    controlled_torch: bool,
    torch_refresh_secs: f32,
    /// Счётчик изменений реестра: оверлей тьмы по нему понимает, что набор
    /// огней изменился, не сравнивая множества каждый кадр.
    version: u64,
}

/// Оверлей тьмы: одна Bevy-сущность с мешем и материалом, пересобираемая
/// под видимую область.
#[derive(Resource, Default)]
pub(crate) struct DarknessOverlay {
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
    material: Option<Handle<DarknessMaterial>>,
    fire_positions: Option<Handle<ShaderBuffer>>,
    /// Для какого прямоугольника камеры собран меш оверлея.
    last_view: Option<(Vec2, Vec2)>,
    /// Состояние, от которого зависит содержимое буфера огней.
    last_fires: Option<FiresKey>,
    /// Последняя затемнённость, записанная в материал.
    last_darkness: Option<f32>,
}

/// Ключ содержимого буфера огней: набор огней, камера и видимая область.
#[derive(Clone, Copy, PartialEq)]
struct FiresKey {
    version: u64,
    camera_translation: Vec3,
    view_min: Vec2,
    view_max: Vec2,
}

/// Держит реестр костров в соответствии с миром. Полный пересбор — после
/// перегенерации мира или загрузки сохранения (`pending_rerender`), иначе
/// точечная сверка по изменённым тайлам обоих каналов (рельеф и декор:
/// костёр — объект, но заодно проверяется и рельеф под ним). Запускается
/// до `render_world`, который изымает dirty-тайлы.
///
/// Факелы в руках пересканируются не чаще `TORCH_POSITION_INTERVAL_SECS`:
/// выбор предмета не оставляет события, а ходьба двигает огонь. Обычный
/// проход по `EntityStore` дешевле, чем событийный канал на каждый слот.
pub(crate) fn update_campfire_positions(
    time: Res<Time>,
    state: Res<GameState>,
    mut lights: ResMut<CampfireLights>,
) {
    let controlled_torch = state
        .controlled_mind()
        .is_some_and(|mind| mind.selected_item() == Some(ItemKind::Torch));
    let force = controlled_torch != lights.controlled_torch;
    lights.controlled_torch = controlled_torch;
    lights.torch_refresh_secs -= time.delta_secs();
    if lights.torch_refresh_secs <= 0.0 || force {
        lights.torch_refresh_secs = TORCH_POSITION_INTERVAL_SECS;
        let mut torches = std::mem::take(&mut lights.torch_scratch);
        torches.clear();
        if let Some(world) = state.world.as_ref() {
            for entity in world.entities.iter() {
                // Факел освещает у всех людей с «умом»: у животных его нет.
                if entity
                    .mind
                    .as_ref()
                    .is_some_and(|mind| mind.selected_item() == Some(ItemKind::Torch))
                {
                    torches.push(entity.pos);
                }
            }
        }
        if torches != lights.torch_positions {
            std::mem::swap(&mut lights.torch_positions, &mut torches);
            lights.version += 1;
        }
        // Возвращаем буфер с сохранённой ёмкостью.
        lights.torch_scratch = torches;
    }

    let Some(world) = state.world.as_ref() else {
        if state.saving {
            // Реестр костров сохраняется, пока мир временно в потоке
            // сохранения: иначе после возврата огни пропали бы до
            // следующего полного пересканирования.
            return;
        }
        if !lights.positions.is_empty() {
            lights.positions.clear();
            lights.version += 1;
        }
        return;
    };
    if state.pending_rerender {
        lights.positions = scan_campfires(world);
        lights.version += 1;
        return;
    }
    let mut changed = false;
    for tile in state
        .dirty_tiles
        .terrain
        .iter()
        .chain(state.dirty_tiles.decor.iter())
    {
        let has_fire = matches!(
            world.object_at(*tile),
            Some(obj) if matches!(obj.kind, ObjectKind::Building(BuildingKind::Campfire))
        );
        changed |= has_fire && lights.positions.insert(*tile)
            || !has_fire && lights.positions.remove(tile);
    }
    if changed {
        lights.version += 1;
    }
}

/// Все костры мира. Порядок обхода не важен: результат — множество.
fn scan_campfires(world: &World) -> HashSet<TilePos> {
    let mut found = HashSet::new();
    for cpy in 0..world.chunks_per_side {
        for cpx in 0..world.chunks_per_side {
            let chunk = world.chunk(ChunkPos { x: cpx, y: cpy });
            for (&idx, obj) in &chunk.objects {
                if matches!(obj.kind, ObjectKind::Building(BuildingKind::Campfire)) {
                    found.insert(TilePos {
                        x: (cpx * CHUNK_SIZE as u32) as i32 + (idx % CHUNK_SIZE as u8) as i32,
                        y: (cpy * CHUNK_SIZE as u32) as i32 + (idx / CHUNK_SIZE as u8) as i32,
                    });
                }
            }
        }
    }
    found
}

/// Пересобирает оверлей тьмы под видимую область и время суток. Запускается
/// после `render_world` (камера и мир уже актуальны). Днём оверлей скрыт.
/// Отключённое в настройках освещение (`Settings::lighting`) работает как
/// вечный день: оверлей скрыт, меш и буфер огней не пересобираются вовсе.
pub(crate) fn update_darkness_overlay(
    mut commands: Commands,
    state: Res<GameState>,
    clock: Res<ClockRes>,
    settings: Res<Settings>,
    lights: Res<CampfireLights>,
    cameras: Query<(&Camera, &Transform, &Projection), With<Camera2d>>,
    mut overlay: ResMut<DarknessOverlay>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<DarknessMaterial>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
) {
    if !settings.lighting {
        if let Some(entity) = overlay.entity {
            commands.entity(entity).insert(Visibility::Hidden);
        }
        // После включения оверлей собирается заново, даже если кадр совпадёт.
        overlay.last_view = None;
        overlay.last_fires = None;
        return;
    }

    let daylight = daylight_factor(
        clock.0.time_of_day_fraction(),
        moon::night_daylight(clock.0.tick),
    );
    let darkness = (1.0 - daylight).clamp(0.0, 1.0);

    let Some(world) = state.world.as_ref() else {
        if state.saving {
            return;
        }
        despawn_overlay(&mut commands, &mut overlay);
        return;
    };
    if darkness <= 0.0 {
        if let Some(entity) = overlay.entity {
            commands.entity(entity).insert(Visibility::Hidden);
        }
        // После скрытия оверлей собирается заново, даже если кадр совпадёт.
        overlay.last_view = None;
        overlay.last_fires = None;
        return;
    }

    // Видимая область с запасом на свет костров у края экрана.
    let Ok((camera, camera_transform, projection)) = cameras.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    let Some(viewport_size) = camera.logical_viewport_size() else {
        return;
    };
    let half_size = viewport_size * (ortho.scale * 0.5);
    let margin = Vec2::splat((LIGHT_RADIUS_TILES + 2.0) * WORLD_TILE_SIZE);
    let view_min = -half_size - margin;
    let view_max = half_size + margin;

    // Кадр не изменился (стоячая камера, стабильная тьма, костры те же):
    // собранный оверлей уже корректен и видим — ничего не трогаем.
    let world_to_camera = camera_transform.to_matrix().inverse();

    // Меш оверлея зависит только от видимого прямоугольника. Камера может
    // двигаться каждый кадр — вершины при этом не меняются.
    if overlay.last_view != Some((view_min, view_max)) {
        let handle = overlay
            .mesh
            .get_or_insert_with(|| meshes.add(Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            )))
            .clone();
        let mut mesh = meshes
            .get_mut(&handle)
            .expect("darkness overlay mesh asset must exist");
        rebuild_darkness_mesh(&mut mesh, view_min, view_max);
        overlay.last_view = Some((view_min, view_max));
    }

    // Буфер огней пересобирается только при изменении набора/позиций огней
    // или камеры: сортировка и построение списка не нужны каждый кадр.
    let fires_key = FiresKey {
        version: lights.version,
        camera_translation: camera_transform.translation,
        view_min,
        view_max,
    };
    if overlay.last_fires != Some(fires_key) {
    // Центры костров в мировых пикселях; костры вне области с запасом
    // света всё равно не дают вклада — отсекаем их до построения сетки.
    // Сортировка по X: фрагментный шейдер ищёт окно огней бинарным
    // поиском, а не перебирает все огни на каждый пиксель экрана.
    let mut fires: Vec<(Vec2, f32, f32)> = lights
        .positions
        .iter()
        .map(|tile| {
            let position = world_position(
                world, Vec2::new(tile.x as f32 + 0.5, tile.y as f32 + 0.5),
            );
            (
                world_to_camera.transform_point3(position).truncate(),
                LIGHT_RADIUS_PX,
                CAMPFIRE_LIGHT_STRENGTH,
            )
        })
        .filter(|fire| {
            fire.0.x >= view_min.x - fire.1
                && fire.0.x <= view_max.x + fire.1
                && fire.0.y >= view_min.y - fire.1
                && fire.0.y <= view_max.y + fire.1
        })
        .collect();
    for position in &lights.torch_positions {
        let camera_position = world_position(world, Vec2::new(position.x, position.y));
        let position = world_to_camera.transform_point3(camera_position).truncate();
        if position.x >= view_min.x - TORCH_LIGHT_RADIUS_PX
            && position.x <= view_max.x + TORCH_LIGHT_RADIUS_PX
            && position.y >= view_min.y - TORCH_LIGHT_RADIUS_PX
            && position.y <= view_max.y + TORCH_LIGHT_RADIUS_PX
        {
            fires.push((position, TORCH_LIGHT_RADIUS_PX, CAMPFIRE_LIGHT_STRENGTH));
        }
    }
    fires.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));

    if overlay.fire_positions.is_none() {
        overlay.fire_positions = Some(buffers.add(ShaderBuffer::from(vec![[0.0; 4]])));
    }
    let fire_positions = overlay
        .fire_positions
        .as_ref()
        .expect("fire position buffer was just initialized")
        .clone();
    let mut fire_buffer = buffers
        .get_mut(&fire_positions)
        .expect("darkness fire position buffer asset must exist");
    fire_buffer.clear();
    if fires.is_empty() {
        // Storage buffers cannot be bound with zero bytes.
        fire_buffer.extend([[0.0; 4]]);
    } else {
        fire_buffer.extend(
            fires
                .iter()
                .map(|(position, radius, strength)| {
                    [position.x, position.y, *radius, *strength]
                }),
        );
    }

    }

    // Материал создаётся один раз, затем меняются только параметры
    // затемнения (плавно в течение суток).
    if overlay.material.is_none() {
        let fire_positions = overlay
            .fire_positions
            .as_ref()
            .expect("fire position buffer exists after the first fire update")
            .clone();
        overlay.material = Some(materials.add(DarknessMaterial {
            params: Vec4::new(darkness, LIGHT_RADIUS_PX, CAMPFIRE_LIGHT_STRENGTH, 0.0),
            fire_positions,
        }));
        overlay.last_darkness = Some(darkness);
    } else if overlay.last_darkness != Some(darkness) {
        let handle = overlay
            .material
            .as_ref()
            .expect("darkness material was just initialized")
            .clone();
        materials
            .get_mut(&handle)
            .expect("darkness material asset must exist")
            .params = Vec4::new(darkness, LIGHT_RADIUS_PX, CAMPFIRE_LIGHT_STRENGTH, 0.0);
        overlay.last_darkness = Some(darkness);
    }

    let mesh = overlay
        .mesh
        .as_ref()
        .expect("darkness mesh exists after the view rebuild")
        .clone();
    let material = overlay
        .material
        .as_ref()
        .expect("darkness material was just initialized")
        .clone();
    let transform = *camera_transform;
    match overlay.entity {
        Some(entity) => {
            commands.entity(entity).insert((transform, Visibility::Visible));
        }
        None => {
            let entity = commands
                .spawn((
                    Mesh2d(mesh),
                    MeshMaterial2d(material),
                    transform,
                    Visibility::Visible,
                ))
                .id();
            overlay.entity = Some(entity);
        }
    }
}

/// Деспавнит оверлей (мир отсутствует).
fn despawn_overlay(commands: &mut Commands, overlay: &mut DarknessOverlay) {
    if let Some(entity) = overlay.entity.take() {
        commands.entity(entity).despawn();
    }
    overlay.last_view = None;
    overlay.last_fires = None;
    overlay.last_darkness = None;
}

/// Четыре вершины и два треугольника покрывают видимую область с запасом.
/// Локальные UV передают шейдеру точные координаты камеры для расчёта света.
fn rebuild_darkness_mesh(mesh: &mut Mesh, view_min: Vec2, view_max: Vec2) {
    let corners = [
        view_min,
        Vec2::new(view_max.x, view_min.y),
        view_max,
        Vec2::new(view_min.x, view_max.y),
    ];
    let positions = corners
        .iter()
        .map(|point| [point.x, point.y, -1.0])
        .collect::<Vec<_>>();
    let uvs = corners
        .iter()
        .map(|point| [point.x, point.y])
        .collect::<Vec<_>>();

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
}
