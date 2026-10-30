//! Освещение: ночное затемнение мира и «свет» костра.
//!
//! Тьма — один оверлей поверх мира: меш-сетка с вершинными цветами
//! (чёрный с альфой), растянутая на видимую область. Альфа вершины —
//! сила затенения в этой точке: ночью 0.5 («темнее на 50%»), днём 0.
//! Смешивание стандартное альфа-блендинговое (`out = dst·(1−a)`), поэтому
//! оверлей именно **ослабляет видимость мира**, не подмешивая своего
//! цвета: фактура и оттенки сохраняются на любой глубине ночи.
//!
//! «Свет» костра — это область, где альфа оверлея плавно падает до нуля:
//! в точке костра затенения нет вовсе (мир выглядит как днём), к краю
//! радиуса затенение линейно-квадратично возвращается. Значение считается
//! на вершинах сетки и интерполируется GPU — свечение мягкое, без
//! привязки к сетке тайлов. (Аддитивного смешивания в 2D Bevy 0.20 нет,
//! поэтому «доливать» яркость нельзя — только ослаблять тьму.)
//!
//! UI рисуется отдельным проходом поверх мира и не затемняется.

use std::collections::HashSet;

use bevy::camera::{Camera2d, Projection};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::sprite_render::AlphaMode2d;

use crate::app::state::{ClockRes, GameState};
use crate::simulation::{ChunkPos, ObjectKind, TilePos, World, CHUNK_SIZE, TILE_SIZE};

/// Светлота мира глухой ночью: 0.5 — «темнее на 50%» относительно дня.
pub(crate) const NIGHT_DAYLIGHT: f32 = 0.08;

/// Радиус света костра в тайлах.
pub(crate) const LIGHT_RADIUS_TILES: f32 = 6.0;

/// Радиус света в мировых пикселях.
const LIGHT_RADIUS_PX: f32 = LIGHT_RADIUS_TILES * TILE_SIZE;

/// Границы переходов суток в часах: рассвет 5–7, закат 18–20.
const DAWN_START_H: f32 = 5.0;
const DAWN_END_H: f32 = 7.0;
const DUSK_START_H: f32 = 18.0;
const DUSK_END_H: f32 = 20.0;

/// Параметры сетки оверлея. Вершинная плотность — компромисс: шаг должен
/// быть заметно меньше радиуса света (иначе «дырка» света теряется между
/// вершинами), но число вершин ограничено — при сильном отдалении шаг
/// растёт и мелкие источники света перестают различаться (там костёр и
/// сам субпиксельный).
const GRID_MAX_COLS: usize = 96;
const GRID_MAX_ROWS: usize = 60;
/// Минимальный шаг сетки: радиус света / 4 — по вершине на каждые ~полтора
/// тайла, квадратичный спад остаётся плавным.
const GRID_MIN_SPACING_PX: f32 = LIGHT_RADIUS_PX * 0.25;

/// Z оверлея: над объектами (1.0) и счётчиками предметов (2.0) при любом
/// Y; UI-проход рисуется позже и поверх.
const DARKNESS_Z_BASE: f32 = 3.0;

/// Светлота мира по доле суток [0, 1): 1.0 днём (7:00–18:00), 0.5 глухой
/// ночью (20:00–5:00), между ними — плавные рассвет и закат (smoothstep).
/// Затенение равно `1 − daylight`.
pub(crate) fn daylight_factor(day_fraction: f32) -> f32 {
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
    NIGHT_DAYLIGHT + (1.0 - NIGHT_DAYLIGHT) * t
}

/// Классический smoothstep на [0, 1].
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Сила света в точке от списка костров (позиции — центры тайлов, в
/// мировых пикселях): максимум по источникам квадратичного спада
/// `(1 − d/R)²`. 1.0 — затенения нет, 0.0 — обычная тьма.
pub(crate) fn light_strength(point: Vec2, fires: &[Vec2]) -> f32 {
    let mut strength = 0.0_f32;
    for fire in fires {
        let t = 1.0 - point.distance(*fire) / LIGHT_RADIUS_PX;
        if t > 0.0 {
            strength = strength.max(t * t);
        }
    }
    strength
}

/// Реестр горящих костров мира (тайлы). Костров мало, а изменения
/// объектов проходят через команды и оставляют dirty-тайлы — реестр
/// сверяется по ним, без обхода мира каждый кадр.
#[derive(Resource, Default)]
pub(crate) struct CampfireLights {
    positions: HashSet<TilePos>,
}

/// Оверлей тьмы: одна Bevy-сущность с мешем и материалом, пересобираемая
/// под видимую область.
#[derive(Resource, Default)]
pub(crate) struct DarknessOverlay {
    entity: Option<Entity>,
    mesh: Option<Handle<Mesh>>,
    material: Option<Handle<ColorMaterial>>,
}

/// Держит реестр костров в соответствии с миром. Полный пересбор — после
/// перегенерации мира или загрузки сохранения (`pending_rerender`), иначе
/// точечная сверка по изменённым тайлам. Запускается до `render_world`,
/// который изымает dirty-тайлы.
pub(crate) fn update_campfire_positions(
    state: Res<GameState>,
    mut lights: ResMut<CampfireLights>,
) {
    let Some(world) = state.world.as_ref() else {
        lights.positions.clear();
        return;
    };
    if state.pending_rerender {
        lights.positions = scan_campfires(world);
        return;
    }
    for tile in &state.dirty_tiles {
        let has_fire = matches!(
            world.object_at(*tile),
            Some(obj) if matches!(obj.kind, ObjectKind::Campfire)
        );
        if has_fire {
            lights.positions.insert(*tile);
        } else {
            lights.positions.remove(tile);
        }
    }
}

/// Все костры мира. Порядок обхода не важен: результат — множество.
fn scan_campfires(world: &World) -> HashSet<TilePos> {
    let mut found = HashSet::new();
    for cpy in 0..world.chunks_per_side {
        for cpx in 0..world.chunks_per_side {
            let chunk = world.chunk(ChunkPos { x: cpx, y: cpy });
            for (&idx, obj) in &chunk.objects {
                if matches!(obj.kind, ObjectKind::Campfire) {
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
/// после `render_world` (камера и мир уже актуальны). Днём оверлей скрыт и
/// меш не пересобирается — работы за кадр нет.
pub(crate) fn update_darkness_overlay(
    mut commands: Commands,
    state: Res<GameState>,
    clock: Res<ClockRes>,
    lights: Res<CampfireLights>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    mut overlay: ResMut<DarknessOverlay>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let daylight = daylight_factor(clock.0.time_of_day_fraction());
    let darkness = (1.0 - daylight).clamp(0.0, 1.0);

    let Some(world) = state.world.as_ref() else {
        despawn_overlay(&mut commands, &mut overlay);
        return;
    };
    if darkness <= 0.0 {
        if let Some(entity) = overlay.entity {
            commands.entity(entity).insert(Visibility::Hidden);
        }
        return;
    }

    // Видимая область с запасом на свет костров у края экрана.
    let Ok((camera_transform, projection)) = cameras.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    let camera_center = camera_transform.translation.truncate();
    let margin = Vec2::splat((LIGHT_RADIUS_TILES + 2.0) * TILE_SIZE);
    let view_min = Vec2::new(ortho.area.min.x, ortho.area.min.y) + camera_center - margin;
    let view_max = Vec2::new(ortho.area.max.x, ortho.area.max.y) + camera_center + margin;

    // Центры костров в мировых пикселях; костры вне области с запасом
    // света всё равно не дают вклада — отсекаем их до построения сетки.
    let fires: Vec<Vec2> = lights
        .positions
        .iter()
        .map(|tile| {
            Vec2::new(
                tile.x as f32 * TILE_SIZE + TILE_SIZE * 0.5,
                tile.y as f32 * TILE_SIZE + TILE_SIZE * 0.5,
            )
        })
        .filter(|fire| {
            fire.x >= view_min.x - LIGHT_RADIUS_PX
                && fire.x <= view_max.x + LIGHT_RADIUS_PX
                && fire.y >= view_min.y - LIGHT_RADIUS_PX
                && fire.y <= view_max.y + LIGHT_RADIUS_PX
        })
        .collect();

    // Шаг сетки: достаточно мелкий для света, но не гуще минимума и
    // не плотнее лимитов по числу вершин.
    let width = (view_max.x - view_min.x).max(1.0);
    let height = (view_max.y - view_min.y).max(1.0);
    let spacing = GRID_MIN_SPACING_PX
        .max(width / GRID_MAX_COLS as f32)
        .max(height / GRID_MAX_ROWS as f32);
    let cols = ((width / spacing).ceil() as usize).min(GRID_MAX_COLS);
    let rows = ((height / spacing).ceil() as usize).min(GRID_MAX_ROWS);

    let mesh = overlay
        .mesh
        .get_or_insert_with(|| meshes.add(Mesh::new(
            PrimitiveTopology::TriangleList,
            // MAIN_WORLD нужен: меш перезаписывается каждый кадр на CPU.
            bevy::asset::RenderAssetUsages::default(),
        )));
    let handle = mesh.clone();
    if let Some(mut mesh) = meshes.get_mut(&handle) {
        rebuild_darkness_mesh(&mut mesh, view_min, spacing, cols, rows, darkness, &fires);
    }

    if overlay.material.is_none() {
        overlay.material = Some(materials.add(ColorMaterial {
            color: Color::WHITE,
            alpha_mode: AlphaMode2d::Blend,
            ..default()
        }));
    }
    let max_y = world.size.tiles() as f32 * TILE_SIZE;
    let transform = Transform::from_xyz(0.0, 0.0, DARKNESS_Z_BASE + max_y * 0.001);
    match overlay.entity {
        Some(entity) => {
            commands.entity(entity).insert((transform, Visibility::Visible));
        }
        None => {
            let material = overlay.material.clone().unwrap();
            let entity = commands
                .spawn((
                    Mesh2d(handle),
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
}

/// Перезаписывает меш оверлея: сетка `(cols+1)×(rows+1)` вершин над
/// областью [view_min, view_min + cols·spacing × rows·spacing]. Цвет
/// вершины — чёрный с альфой затенения, ослабленного светом костров.
fn rebuild_darkness_mesh(
    mesh: &mut Mesh,
    origin: Vec2,
    spacing: f32,
    cols: usize,
    rows: usize,
    darkness: f32,
    fires: &[Vec2],
) {
    let mut positions = Vec::with_capacity((cols + 1) * (rows + 1));
    let mut colors = Vec::with_capacity((cols + 1) * (rows + 1));
    for row in 0..=rows {
        for col in 0..=cols {
            let point = origin + Vec2::new(col as f32 * spacing, row as f32 * spacing);
            positions.push([point.x, point.y, 0.0]);
            let alpha = darkness * (1.0 - light_strength(point, fires));
            colors.push([0.0, 0.0, 0.0, alpha]);
        }
    }

    let mut indices = Vec::with_capacity(cols * rows * 6);
    let stride = (cols + 1) as u32;
    for row in 0..rows as u32 {
        for col in 0..cols as u32 {
            let base = row * stride + col;
            indices.extend([base, base + 1, base + stride + 1, base, base + stride + 1, base + stride]);
        }
    }

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
    mesh.insert_indices(Indices::U32(indices));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daylight_full_day_and_night() {
        // Полдень — полная светлота (тьмы нет), глухая ночь — ровно на 50% темнее.
        assert_eq!(daylight_factor(0.5), 1.0);
        assert_eq!(daylight_factor(0.0), NIGHT_DAYLIGHT);
        assert_eq!(daylight_factor(0.999), NIGHT_DAYLIGHT);
    }

    #[test]
    fn daylight_transitions_are_monotonic() {
        // Рассвет 5–7: светлота растёт; закат 18–20: падает. Шаги по 6 минут,
        // строго внутри интервала перехода (за его пределами плато).
        const STEP_H: f32 = 0.1;

        let mut hour = DAWN_START_H;
        let mut prev = daylight_factor(hour / 24.0);
        assert_eq!(prev, NIGHT_DAYLIGHT);
        while hour + STEP_H <= DAWN_END_H {
            hour += STEP_H;
            let next = daylight_factor(hour / 24.0);
            assert!(next > prev, "рассвет должен светлеть: {hour}");
            prev = next;
        }
        assert_eq!(daylight_factor(DAWN_END_H / 24.0), 1.0);

        let mut hour = DUSK_START_H;
        let mut prev = daylight_factor(hour / 24.0);
        assert_eq!(prev, 1.0);
        while hour + STEP_H <= DUSK_END_H {
            hour += STEP_H;
            let next = daylight_factor(hour / 24.0);
            assert!(next < prev, "закат должен темнеть: {hour}");
            prev = next;
        }
        assert_eq!(daylight_factor(DUSK_END_H / 24.0), NIGHT_DAYLIGHT);
    }

    #[test]
    fn fire_light_falls_off_and_saturates() {
        let fire = Vec2::ZERO;
        // В точке костра света максимум, за радиусом — ноль, в половине
        // радиуса — четверть (квадратичный спад).
        assert_eq!(light_strength(fire, &[fire]), 1.0);
        assert_eq!(light_strength(Vec2::new(LIGHT_RADIUS_PX, 0.0), &[fire]), 0.0);
        assert_eq!(
            light_strength(Vec2::new(LIGHT_RADIUS_PX * 0.5, 0.0), &[fire]),
            0.25
        );
        // Два дальних костра не дают света вдали от обоих.
        let far = Vec2::new(LIGHT_RADIUS_PX * 10.0, 0.0);
        assert_eq!(light_strength(far, &[fire, Vec2::new(1.0, 1.0)]), 0.0);
    }
}
