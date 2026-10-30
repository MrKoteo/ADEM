//! Рендер боевой системы: эффекты оружия, снаряды в полёте, воткнутые копья.
//!
//! Бой считается в симуляции (`simulation/combat.rs`); здесь только
//! визуализация. Эффекты оружия — косметика по сообщению `WeaponFx`
//! (дуга лезвия и выпад уходят и на промахе). Снаряды и воткнутые копья
//! рисуются только для видимой области — как существа.

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Camera2d, Projection};
use bevy::mesh::{Indices, Mesh, PrimitiveTopology};
use bevy::prelude::*;
use bevy::sprite_render::{AlphaMode2d, ColorMaterial};

use crate::app::input::{WeaponFx, WeaponFxKind};
use crate::app::state::{GameMode, GameState, PlayerRes, SimTick, TickAccumulator, player_render_pos};
use crate::simulation::time::TICKS_PER_SEC;
use crate::simulation::{ItemKind, ProjectileKind, StuckSpear, TILE_SIZE};

use super::textures::TileTextures;

// ---------- Эффекты оружия ----------

/// Длительность анимации взмаха лезвия.
const BLADE_DURATION_SECS: f32 = 0.15;
/// Угол, который проходит след за анимацию (радианы, 90°).
const BLADE_SWEEP: f32 = std::f32::consts::FRAC_PI_2;
/// Угловая ширина сектора следа: втрое уже базовой (45°).
const BLADE_SECTOR: f32 = std::f32::consts::PI / 4.0;
/// Радиусы сектора в мировых пикселях: от игрока наружу.
const SWING_INNER_PX: f32 = TILE_SIZE * 0.6;
const SWING_OUTER_PX: f32 = TILE_SIZE * 0.9;
/// Длительность выпада копья: вперёд и обратно.
const THRUST_DURATION_SECS: f32 = 0.25;
/// Насколько далеко копьё выносится вперёд от центра игрока, мировых px.
const THRUST_REACH_PX: f32 = TILE_SIZE * 1.2;
/// Сколько тиков держится текстура лука после выстрела: ровно кулдаун
/// выстрела (15 тиков = 0.75 с при x1), чтобы анимация не пережидала
/// готовность следующего выстрела.
const BOW_SHOT_FX_TICKS: f32 = 15.0;
/// Дополнительный поворот лука относительно направления выстрела:
/// +90° по часовой стрелке (в математической конвенции — минус 90°).
const BOW_EXTRA_ROTATION: f32 = -std::f32::consts::FRAC_PI_2;
/// Размер текстуры копья в анимации выпада, тайлов (вдвое меньше прежнего
/// показа — решение автора).
const THRUST_SPRITE_TILES: f32 = 0.8;
/// Размер текстуры брошенного копья в полёте, тайлов (в ~1.5 раза меньше
/// прежнего показа 1.4).
const THROWN_SPEAR_SPRITE_TILES: f32 = 0.9;
/// Размер текстуры лука относительно тайла (вдвое меньше прежнего показа).
const BOW_SPRITE_TILES: f32 = 0.5;
/// Насколько эффект оружия поднимается над z-формулой существ: эффект
/// должен рисоваться поверх спрайта игрока.
const FX_Z_LIFT: f32 = 0.05;
/// Запасной z, пока мир не создан (эффекты всё равно не спавнятся).
const FALLBACK_FX_Z: f32 = 2.5;

/// Z эффекта оружия в точке `y` (мировые px): та же формула z-сортировки,
/// что у существ и игрока (`z = 1 + (max_y − y)·0.001`), плюс небольшой
/// подъём. Фиксированная константа была бы ошибкой: z игрока растёт по
/// мере удаления от верхнего края карты, и на большой карте эффект
/// рисовался бы под спрайтом игрока.
fn weapon_fx_z(y: f32, max_y: f32) -> f32 {
    1.0 + (max_y - y) * 0.001 + FX_Z_LIFT
}

/// Верх карты в мировых пикселях (для z-формулы); `None` — мир не создан.
fn world_max_y(state: &GameState) -> Option<f32> {
    state
        .world
        .as_ref()
        .map(|w| w.size.tiles() as f32 * TILE_SIZE)
}
/// Цвет следа: серо-белый, полупрозрачный (альфа 0.5 в начале взмаха).
const SWING_COLOR: Color = Color::srgba(0.85, 0.85, 0.9, 0.5);

/// Эффект оружия: живёт по своему таймеру, форма анимации зависит от вида.
#[derive(Component)]
struct WeaponFxEffect {
    age: f32,
    duration: f32,
    kind: WeaponFxKind,
    /// Точка, откуда эффект (центр игрока при применении).
    origin: Vec2,
    /// Направление на прицел (радианы).
    aim_angle: f32,
    /// Для лезвия: угол в начале анимации (прицел минус полдуги).
    start_angle: f32,
}

/// Общие ресурсы эффектов: меш сектора строится один раз.
#[derive(Resource, Default)]
struct SwingFx {
    mesh: Option<Handle<Mesh>>,
}

/// Меш сектора кольца, направленного вдоль +X (анимация поворачивает
/// сущность); вершины — в мировых пикселях относительно центра игрока.
fn build_swing_mesh() -> Mesh {
    const SEGMENTS: usize = 4;
    let half = BLADE_SECTOR * 0.5;
    let mut positions = Vec::with_capacity((SEGMENTS + 1) * 2);
    let mut indices = Vec::with_capacity(SEGMENTS * 6);
    for i in 0..=SEGMENTS {
        let angle = -half + BLADE_SECTOR * i as f32 / SEGMENTS as f32;
        let (sin, cos) = angle.sin_cos();
        positions.push([cos * SWING_INNER_PX, sin * SWING_INNER_PX, 0.0]);
        positions.push([cos * SWING_OUTER_PX, sin * SWING_OUTER_PX, 0.0]);
        if i < SEGMENTS {
            let base = (i * 2) as u32;
            indices.extend([base, base + 1, base + 3, base, base + 3, base + 2]);
        }
    }
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Спавнит эффект по сообщению применения оружия.
fn spawn_weapon_fx(
    mut messages: MessageReader<WeaponFx>,
    state: Res<GameState>,
    mut fx: ResMut<SwingFx>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    textures: Res<TileTextures>,
    overlay: Query<(Entity, &WeaponFxEffect)>,
    mut commands: Commands,
) {
    let mesh = fx
        .mesh
        .get_or_insert_with(|| meshes.add(build_swing_mesh()))
        .clone();
    let max_y = world_max_y(&state);
    for fx_msg in messages.read() {
        let aim_angle = (fx_msg.aim.y - fx_msg.origin.y).atan2(fx_msg.aim.x - fx_msg.origin.x);
        // Предмет поверх игрока (копьё, лук) показывается одним эффектом:
        // новая анимация прерывает предыдущую, иначе при быстрых кликах
        // текстуры плодились бы поверх игрока.
        if matches!(fx_msg.kind, WeaponFxKind::Thrust | WeaponFxKind::Bow) {
            for (entity, effect) in &overlay {
                if matches!(effect.kind, WeaponFxKind::Thrust | WeaponFxKind::Bow) {
                    commands.entity(entity).despawn();
                }
            }
        }
        match fx_msg.kind {
            WeaponFxKind::Blade => {
                let start_angle = aim_angle - BLADE_SWEEP * 0.5;
                let material = materials.add(ColorMaterial {
                    color: SWING_COLOR,
                    alpha_mode: AlphaMode2d::Blend,
                    ..default()
                });
                commands.spawn((
                    Mesh2d(mesh.clone()),
                    MeshMaterial2d(material.clone()),
                    Transform::from_xyz(
                        fx_msg.origin.x,
                        fx_msg.origin.y,
                        max_y.map(|m| weapon_fx_z(fx_msg.origin.y, m))
                            .unwrap_or(FALLBACK_FX_Z),
                    )
                    .with_rotation(Quat::from_rotation_z(start_angle)),
                    WeaponFxEffect {
                        age: 0.0,
                        duration: BLADE_DURATION_SECS,
                        kind: WeaponFxKind::Blade,
                        origin: fx_msg.origin,
                        aim_angle,
                        start_angle,
                    },
                ));
            }
            WeaponFxKind::Thrust | WeaponFxKind::Bow => {
                // Текстура оружия от центра игрока, повёрнутая к прицелу;
                // выпад двигается в анимации, лук стоит на месте (следует
                // за игроком). У лука — дополнительный поворот на +90° по
                // часовой и вдвое меньший размер.
                let (kind_texture, size, extra_rotation) = match fx_msg.kind {
                    WeaponFxKind::Thrust => (
                        textures.item_texture(ItemKind::Spear),
                        THRUST_SPRITE_TILES,
                        0.0,
                    ),
                    _ => (
                        textures.item_texture(ItemKind::Bow),
                        BOW_SPRITE_TILES,
                        BOW_EXTRA_ROTATION,
                    ),
                };
                let rotation = aim_angle - SPRITE_ANGLE_OFFSET + extra_rotation;
                commands.spawn((
                    Sprite {
                        image: kind_texture.clone(),
                        custom_size: Some(Vec2::splat(TILE_SIZE * size)),
                        ..default()
                    },
                    Transform::from_xyz(
                        fx_msg.origin.x,
                        fx_msg.origin.y,
                        max_y.map(|m| weapon_fx_z(fx_msg.origin.y, m))
                            .unwrap_or(FALLBACK_FX_Z),
                    )
                    .with_rotation(Quat::from_rotation_z(rotation)),
                    WeaponFxEffect {
                        age: 0.0,
                        duration: match fx_msg.kind {
                            WeaponFxKind::Thrust => THRUST_DURATION_SECS,
                            _ => BOW_SHOT_FX_TICKS / TICKS_PER_SEC as f32,
                        },
                        kind: fx_msg.kind,
                        origin: fx_msg.origin,
                        aim_angle,
                        start_angle: aim_angle,
                    },
                ));
            }
        }
    }
}

/// Анимирует и убирает отжившие эффекты: лезвие — поворот по дуге
/// и растворение; выпад — вынос копья вперёд (по синусоиде, плавно
/// вперёд и назад) с сохранением поворота к прицелу; лук — статичная
/// текстура, привязанная к текущей позиции игрока.
fn update_weapon_fx(
    time: Res<Time>,
    state: Res<GameState>,
    player: Res<PlayerRes>,
    acc: Res<TickAccumulator>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut effects: Query<(Entity, &mut WeaponFxEffect, Option<&MeshMaterial2d<ColorMaterial>>)>,
    mut commands: Commands,
) {
    for (entity, mut effect, material) in &mut effects {
        effect.age += time.delta_secs();
        let progress = (effect.age / effect.duration).clamp(0.0, 1.0);
        // Интерполированная позиция игрока для эффектов, привязанных к нему
        // (лук). Нет игрока в режиме игрока — эффекту не к чему привязываться.
        let player_pos = player
            .0
            .as_ref()
            .filter(|_| state.mode == GameMode::Player)
            .map(|p| player_render_pos(p, &acc));
        // Z-сортировка как у существ: эффект всегда чуть выше спрайта
        // игрока на текущей глубине карты.
        let max_y = world_max_y(&state);
        let fx_z = |y: f32| max_y.map(|m| weapon_fx_z(y, m)).unwrap_or(FALLBACK_FX_Z);
        match effect.kind {
            WeaponFxKind::Blade => {
                let angle = effect.start_angle + BLADE_SWEEP * progress;
                commands.entity(entity).insert(
                    Transform::from_xyz(effect.origin.x, effect.origin.y, fx_z(effect.origin.y))
                        .with_rotation(Quat::from_rotation_z(angle)),
                );
                if let Some(material) = material
                    && let Some(mut material) = materials.get_mut(&material.0)
                {
                    material.color = SWING_COLOR.with_alpha(SWING_COLOR.alpha() * (1.0 - progress));
                }
            }
            WeaponFxKind::Thrust => {
                // Синусоида: плавный вынос вперёд и такой же возврат назад.
                // Поворот к прицелу задаётся заново каждый кадр: трансформ
                // перезаписывается целиком, и без этого копьё теряло бы
                // ориентацию после первого кадра анимации.
                let offset = (std::f32::consts::PI * progress).sin() * THRUST_REACH_PX;
                let (sin, cos) = effect.aim_angle.sin_cos();
                let (x, y) = (effect.origin.x + cos * offset, effect.origin.y + sin * offset);
                commands.entity(entity).insert(
                    Transform::from_xyz(x, y, fx_z(y))
                    .with_rotation(Quat::from_rotation_z(effect.aim_angle - SPRITE_ANGLE_OFFSET)),
                );
            }
            WeaponFxKind::Bow => {
                let Some(p) = player_pos else {
                    // Игрок покинул режим: лук не должен висеть в мире один.
                    if let Some(material) = material {
                        materials.remove(&material.0);
                    }
                    commands.entity(entity).despawn();
                    continue;
                };
                let (x, y) = (p.x * TILE_SIZE, p.y * TILE_SIZE);
                commands.entity(entity).insert(
                    Transform::from_xyz(x, y, fx_z(y))
                        .with_rotation(Quat::from_rotation_z(
                            effect.aim_angle - SPRITE_ANGLE_OFFSET + BOW_EXTRA_ROTATION,
                        )),
                );
            }
        }
        if progress >= 1.0 {
            if let Some(material) = material {
                materials.remove(&material.0);
            }
            commands.entity(entity).despawn();
        }
    }
}

// ---------- Снаряды и воткнутые копья ----------

/// Соответствие «id снаряда симуляции → Bevy-сущность».
#[derive(Resource, Default)]
struct ProjectileSprites {
    map: HashMap<u64, Entity>,
}

/// Длина линии стрелы в мировых пикселях (на ~10% короче тайла).
const ARROW_LINE_PX: f32 = 10.0;
/// Толщина линии стрелы на ЭКРАНЕ в пикселях — не масштабируется зумом:
/// мировая толщина пересчитывается из масштаба камеры каждый кадр.
const ARROW_LINE_SCREEN_PX: f32 = 0.25;
/// Насколько снаряд визуально приподнят на экране за высоту полёта:
/// высота в тайлах пересчитывается в мировые пиксели с этим множителем
/// (топ-даун псевдоперспектива).
const HEIGHT_LIFT: f32 = 0.75;

/// Оружейные текстуры (копьё, лук) нарисованы направленными вверх-вправо
/// (≈45°); поправка выравнивает рисунок вдоль направления атаки/полёта.
const SPRITE_ANGLE_OFFSET: f32 = std::f32::consts::FRAC_PI_4;

/// Общие ресурсы снарядов: меш единичного квадрата и материал линии
/// стрелы. Материал кэшируется: создание нового каждый кадр оставляло бы
/// в ассетах десятки неиспользуемых материалов в секунду.
#[derive(Resource, Default)]
struct ProjectileFx {
    arrow_mesh: Option<Handle<Mesh>>,
    arrow_material: Option<Handle<ColorMaterial>>,
}

/// Спрайты воткнутых копьёв: соответствие «копьё → Bevy-сущность». Ключ —
/// битовое представление параметров копья (позиция, угол, отвесность):
/// у `StuckSpear` в симуляции нет стабильного id (список пересобирается
/// `swap_remove` при подборе), а побитово одинаковые копья визуально
/// совпадают — один спрайт честно рисует их оба. Спрайты создаются и
/// убираются точечно по разнице множеств, без пересбора списка каждый кадр.
#[derive(Resource, Default)]
struct StuckSpearSprites {
    map: HashMap<StuckSpearKey, Entity>,
}

/// Ключ спрайта копья: биты четырёх f32-полей записи.
type StuckSpearKey = [u64; 4];

/// Битовый ключ копья (позиция, угол падения, отвесность).
fn spear_key(spear: &StuckSpear) -> StuckSpearKey {
    [
        spear.pos.x.to_bits() as u64,
        spear.pos.y.to_bits() as u64,
        spear.heading.to_bits() as u64,
        spear.pitch.to_bits() as u64,
    ]
}

/// Маркер линии стрелы (масштаб и поворот задаёт трансформ).
#[derive(Component)]
struct ArrowLine;

/// Обновляет спрайты снарядов: создаёт для видимых, двигает по позиции
/// и высоте, убирает закончившие полёт. Стрела — тонкая серо-белая линия
/// (экранная толщина постоянна), копьё — спрайт по направлению полёта.
fn update_projectile_sprites(
    mut commands: Commands,
    state: Res<GameState>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    textures: Res<TileTextures>,
    mut fx: ResMut<ProjectileFx>,
    mut sprites: ResMut<ProjectileSprites>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    let Some(world) = state.world.as_ref() else {
        for (_, entity) in sprites.map.drain() {
            commands.entity(entity).despawn();
        }
        return;
    };
    let Ok((camera_transform, projection)) = cameras.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    let camera_center = camera_transform.translation.truncate();
    let margin = Vec2::splat(2.0 * TILE_SIZE);
    let view_min = Vec2::new(ortho.area.min.x, ortho.area.min.y) + camera_center - margin;
    let view_max = Vec2::new(ortho.area.max.x, ortho.area.max.y) + camera_center + margin;
    // Мировая толщина линии стрелы: экранная, делённая на масштаб камеры.
    let arrow_width = (ARROW_LINE_SCREEN_PX / ortho.scale).clamp(0.1, 64.0);

    let arrow_mesh = fx
        .arrow_mesh
        .get_or_insert_with(|| meshes.add(unit_quad_mesh()))
        .clone();
    let arrow_material = fx
        .arrow_material
        .get_or_insert_with(|| {
            materials.add(ColorMaterial {
                color: Color::srgba(0.85, 0.85, 0.9, 0.9),
                alpha_mode: AlphaMode2d::Blend,
                ..default()
            })
        })
        .clone();

    let max_y = world.size.tiles() as f32 * TILE_SIZE;
    let mut visible = HashSet::new();
    for projectile in &world.projectiles {
        let pos = projectile.pos();
        let x = pos.x * TILE_SIZE;
        let y = pos.y * TILE_SIZE + projectile.height() * TILE_SIZE * HEIGHT_LIFT;
        if x < view_min.x || x > view_max.x || y < view_min.y || y > view_max.y {
            continue;
        }
        visible.insert(projectile.id);
        let heading = projectile.heading();
        let z = 2.0 + (max_y - y) * 0.001;

        match projectile.kind {
            ProjectileKind::Arrow => {
                // Линия: единичный квадрат, растянутый трансформом в отрезок
                // вдоль направления полёта. Меш и материал общие.
                let transform = Transform::from_xyz(x, y, z)
                    .with_rotation(Quat::from_rotation_z(heading))
                    .with_scale(Vec3::new(ARROW_LINE_PX, arrow_width, 1.0));
                match sprites.map.get(&projectile.id).copied() {
                    Some(entity) => {
                        commands.entity(entity).insert(transform);
                    }
                    None => {
                        let entity = commands
                            .spawn((
                                ArrowLine,
                                Mesh2d(arrow_mesh.clone()),
                                MeshMaterial2d(arrow_material.clone()),
                                transform,
                            ))
                            .id();
                        sprites.map.insert(projectile.id, entity);
                    }
                }
            }
            ProjectileKind::ThrownSpear => {
                let sprite = Sprite {
                    image: textures.item_texture(ItemKind::Spear).clone(),
                    custom_size: Some(Vec2::splat(TILE_SIZE * THROWN_SPEAR_SPRITE_TILES)),
                    ..default()
                };
                let transform = Transform::from_xyz(x, y, z)
                    .with_rotation(Quat::from_rotation_z(heading - SPRITE_ANGLE_OFFSET));
                match sprites.map.get(&projectile.id).copied() {
                    Some(entity) => {
                        commands.entity(entity).insert((sprite, transform));
                    }
                    None => {
                        let entity = commands.spawn((sprite, transform)).id();
                        sprites.map.insert(projectile.id, entity);
                    }
                }
            }
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
    }
}

/// Обновляет спрайты воткнутых копьёв инкрементально: копья стоят на месте
/// от падения до подбора, поэтому по разнице множеств ключей создаются
/// только новые спрайты и убираются исчезнувшие — каждый кадр ничего
/// не пересобирается.
fn update_stuck_spear_sprites(
    mut commands: Commands,
    state: Res<GameState>,
    cameras: Query<(&Transform, &Projection), With<Camera2d>>,
    textures: Res<TileTextures>,
    mut sprites: ResMut<StuckSpearSprites>,
) {
    let Some(world) = state.world.as_ref() else {
        for (_, entity) in sprites.map.drain() {
            commands.entity(entity).despawn();
        }
        return;
    };
    let Ok((camera_transform, projection)) = cameras.single() else {
        return;
    };
    let Projection::Orthographic(ortho) = projection else {
        return;
    };
    let camera_center = camera_transform.translation.truncate();
    let margin = Vec2::splat(2.0 * TILE_SIZE);
    let view_min = Vec2::new(ortho.area.min.x, ortho.area.min.y) + camera_center - margin;
    let view_max = Vec2::new(ortho.area.max.x, ortho.area.max.y) + camera_center + margin;

    let max_y = world.size.tiles() as f32 * TILE_SIZE;
    let mut visible = HashSet::new();
    for spear in &world.stuck_spears {
        let x = spear.pos.x * TILE_SIZE;
        let y = spear.pos.y * TILE_SIZE;
        if x < view_min.x || x > view_max.x || y < view_min.y || y > view_max.y {
            continue;
        }
        let key = spear_key(spear);
        if !visible.insert(key) || sprites.map.contains_key(&key) {
            // Побитовый дубль (второе копьё ровно на том же месте) или
            // спрайт уже существует: рисовать второй раз нечего.
            continue;
        }
        let z = 1.0 + (max_y - y) * 0.001;
        // Воткнувшееся копьё смотрит туда, куда летело; наклон входа
        // показан лёгким «укорачиванием» спрайта: чем отвеснее падение,
        // тем короче видимая проекция.
        let steep = (spear.pitch / std::f32::consts::FRAC_PI_2).clamp(0.0, 1.0);
        let sprite = Sprite {
            image: textures.item_texture(ItemKind::Spear).clone(),
            custom_size: Some(Vec2::splat(TILE_SIZE * 1.4 * (1.0 - steep * 0.35))),
            ..default()
        };
        let transform = Transform::from_xyz(x, y, z)
            .with_rotation(Quat::from_rotation_z(spear.heading - SPRITE_ANGLE_OFFSET));
        let entity = commands.spawn((sprite, transform)).id();
        sprites.map.insert(key, entity);
    }

    let stale: Vec<StuckSpearKey> = sprites
        .map
        .keys()
        .copied()
        .filter(|key| !visible.contains(key))
        .collect();
    for key in stale {
        if let Some(entity) = sprites.map.remove(&key) {
            commands.entity(entity).despawn();
        }
    }
}

/// Единичный квадрат с центром в нуле (для линии стрелы).
fn unit_quad_mesh() -> Mesh {
    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default());
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [-0.5_f32, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.5, 0.5, 0.0],
            [-0.5, 0.5, 0.0],
        ],
    );
    mesh.insert_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3]));
    mesh
}

/// Плагин боевых визуализаций.
pub struct CombatFxPlugin;

impl Plugin for CombatFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SwingFx>()
            .init_resource::<ProjectileFx>()
            .init_resource::<ProjectileSprites>()
            .init_resource::<StuckSpearSprites>()
            .add_systems(Update, spawn_weapon_fx)
            .add_systems(Update, update_weapon_fx)
            .add_systems(Update, (update_projectile_sprites, update_stuck_spear_sprites).after(SimTick));
    }
}
