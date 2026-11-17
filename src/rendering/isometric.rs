use bevy::camera::Camera;
use bevy::math::{Vec2, Vec3};
use bevy::prelude::GlobalTransform;

use crate::simulation::heightmap::{HEIGHT_MAX, SEA_LEVEL};
use crate::simulation::{TilePos, TileType, World, TILE_SIZE};

/// Grid spacing is tuned so a 30°-above-horizon, 45° azimuth camera
/// reproduces the previous 2:1 isometric diamond.
pub(crate) const WORLD_TILE_SIZE: f32 = TILE_SIZE * std::f32::consts::SQRT_2;

/// Мировые единицы на один уровень высоты. 1 уровень = 1/16 тайла
/// (1 мировой юнит); полный диапазон 0..255 даёт 255 юнитов (≈16 тайлов).
/// Рендер не квантует высоты: каждый уровень рисуется отдельно.
pub(crate) const HEIGHT_WORLD_UNIT: f32 = TILE_SIZE / 16.0;

/// Верхняя граница рельефа в мировых единицах (для отсечения блоков).
pub(crate) const MAX_WORLD_HEIGHT: f32 = HEIGHT_MAX as f32 * HEIGHT_WORLD_UNIT;

/// Плоскость вне мира: уровень моря, чтобы у края карты не было обрыва.
pub(crate) const SEA_WORLD_HEIGHT: f32 = SEA_LEVEL as f32 * HEIGHT_WORLD_UNIT;

/// Высота слоя в мировых единицах. Все потребители — меш, пикинг, спрайты,
/// камера — обязаны использовать эту функцию, иначе объекты «поплывут»
/// относительно поверхности.
#[inline]
pub(crate) fn height_to_world(level: u8) -> f32 {
    level as f32 * HEIGHT_WORLD_UNIT
}

/// Высота поверхности тайла в мировых единицах. За пределами мира —
/// уровень моря (там всегда океан).
pub(crate) fn terrain_elevation(world: &World, tile: TilePos) -> f32 {
    height_to_world(world.height_or(tile, SEA_LEVEL))
}

pub(crate) fn world_position(world: &World, grid: Vec2) -> Vec3 {
    let tile = TilePos {
        x: grid.x.floor() as i32,
        y: grid.y.floor() as i32,
    };
    Vec3::new(
        grid.x * WORLD_TILE_SIZE,
        grid.y * WORLD_TILE_SIZE,
        terrain_elevation(world, tile),
    )
}

pub(crate) fn ground_position_from_viewport(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    viewport: Vec2,
    world: &World,
) -> Option<Vec3> {
    let ray = camera.viewport_to_world(camera_transform, viewport).ok()?;
    Some(intersect_terrain(ray.origin, ray.direction.as_vec3(), world)?.point)
}

pub(crate) fn terrain_tile_from_viewport(
    camera: &Camera,
    camera_transform: &GlobalTransform,
    viewport: Vec2,
    world: &World,
) -> Option<TilePos> {
    let ray = camera.viewport_to_world(camera_transform, viewport).ok()?;
    Some(intersect_terrain(ray.origin, ray.direction.as_vec3(), world)?.tile)
}

#[derive(Clone, Copy)]
struct TerrainHit {
    point: Vec3,
    tile: TilePos,
}

/// Допуск при сравнении t и координат: лучи из ортокамеры почти всегда
/// попадают в общую границу тайлов, а не в середину.
const PICK_EPS: f32 = 1e-4;

/// Пересечение луча с рельефом (ступени + стенки).
///
/// Высоты ограничены `[0, MAX_WORLD_HEIGHT]`, поэтому луч может пересечь
/// рельеф только на ограниченном участке: при наклоне камеры 30° это порядка
/// 150–180 тайлов на полной амплитуде (не зависит от размера мира). DDA идёт
/// по тайлам этого участка и проверяет верхнюю плоскость и две видимые
/// стенки (−X/−Y) каждого тайла.
fn intersect_terrain(origin: Vec3, direction: Vec3, world: &World) -> Option<TerrainHit> {
    if direction.z.abs() < f32::EPSILON {
        return None;
    }
    let side_tiles = world.size.tiles() as i32;
    let side = side_tiles as f32 * WORLD_TILE_SIZE;

    // Интервал t, где луч находится в высотном слое [0, MAX].
    let (layer_start, layer_end) = if direction.z < 0.0 {
        (
            (MAX_WORLD_HEIGHT - origin.z) / direction.z,
            (0.0 - origin.z) / direction.z,
        )
    } else {
        (
            (0.0 - origin.z) / direction.z,
            (MAX_WORLD_HEIGHT - origin.z) / direction.z,
        )
    };
    let mut t_start = layer_start.max(0.0);
    let mut t_end = layer_end;
    if t_end <= t_start {
        return base_plane_hit(origin, direction);
    }

    // Ограничиваем участок горизонтальными границами мира.
    let Some((x0, x1)) = axis_interval(origin.x, direction.x, 0.0, side) else {
        return base_plane_hit(origin, direction);
    };
    let Some((y0, y1)) = axis_interval(origin.y, direction.y, 0.0, side) else {
        return base_plane_hit(origin, direction);
    };
    t_start = t_start.max(x0).max(y0);
    t_end = t_end.min(x1).min(y1);
    if t_end <= t_start + PICK_EPS {
        return base_plane_hit(origin, direction);
    }

    let start_point = origin + direction * t_start;
    let mut gx = (start_point.x / WORLD_TILE_SIZE).floor() as i32;
    let mut gy = (start_point.y / WORLD_TILE_SIZE).floor() as i32;
    gx = gx.clamp(0, side_tiles - 1);
    gy = gy.clamp(0, side_tiles - 1);

    let step_x = if direction.x > PICK_EPS {
        1
    } else if direction.x < -PICK_EPS {
        -1
    } else {
        0
    };
    let step_y = if direction.y > PICK_EPS {
        1
    } else if direction.y < -PICK_EPS {
        -1
    } else {
        0
    };
    let t_delta_x = if step_x == 0 {
        f32::INFINITY
    } else {
        WORLD_TILE_SIZE / direction.x.abs()
    };
    let t_delta_y = if step_y == 0 {
        f32::INFINITY
    } else {
        WORLD_TILE_SIZE / direction.y.abs()
    };
    let next_x = if step_x > 0 {
        (gx + 1) as f32 * WORLD_TILE_SIZE
    } else {
        gx as f32 * WORLD_TILE_SIZE
    };
    let next_y = if step_y > 0 {
        (gy + 1) as f32 * WORLD_TILE_SIZE
    } else {
        gy as f32 * WORLD_TILE_SIZE
    };
    let mut t_max_x = if step_x == 0 {
        f32::INFINITY
    } else {
        (next_x - origin.x) / direction.x
    };
    let mut t_max_y = if step_y == 0 {
        f32::INFINITY
    } else {
        (next_y - origin.y) / direction.y
    };
    t_max_x = t_max_x.max(t_start);
    t_max_y = t_max_y.max(t_start);

    let mut best: Option<(f32, Vec3, TilePos)> = None;
    let mut t = t_start;
    while t <= t_end + PICK_EPS {
        if gx < 0 || gy < 0 || gx >= side_tiles || gy >= side_tiles {
            break;
        }
        let tile = TilePos { x: gx, y: gy };
        let cell_exit = t_max_x.min(t_max_y).min(t_end);
        // Пустой тайл не рисуется; его стенки никому не принадлежат.
        if world.terrain_kind(tile) != TileType::Empty
            && let Some((hit_distance, hit_point)) =
                tile_surface_hit(origin, direction, world, tile, t, cell_exit)
            && best.is_none_or(|(best_distance, _, _)| hit_distance < best_distance)
        {
            best = Some((hit_distance, hit_point, tile));
        }

        if t_max_x <= t_max_y {
            if step_x == 0 {
                break;
            }
            gx += step_x;
            t = t_max_x;
            t_max_x += t_delta_x;
        } else {
            if step_y == 0 {
                break;
            }
            gy += step_y;
            t = t_max_y;
            t_max_y += t_delta_y;
        }
    }

    let (_, point, tile) = best?;
    Some(TerrainHit { point, tile })
}

/// Пересечение луча с поверхностью одного тайла: верхняя плоскость и две
/// стенки, которые реально строит меш (−X и −Y).
fn tile_surface_hit(
    origin: Vec3,
    direction: Vec3,
    world: &World,
    tile: TilePos,
    t_enter: f32,
    t_exit: f32,
) -> Option<(f32, Vec3)> {
    let x0 = tile.x as f32 * WORLD_TILE_SIZE;
    let y0 = tile.y as f32 * WORLD_TILE_SIZE;
    let x1 = x0 + WORLD_TILE_SIZE;
    let y1 = y0 + WORLD_TILE_SIZE;
    let elevation = terrain_elevation(world, tile);
    let mut best: Option<(f32, Vec3)> = None;

    // Верхняя плоскость.
    let distance = (elevation - origin.z) / direction.z;
    if distance >= t_enter - PICK_EPS && distance <= t_exit + PICK_EPS {
        let point = origin + direction * distance;
        if point.x >= x0 - PICK_EPS
            && point.x <= x1 + PICK_EPS
            && point.y >= y0 - PICK_EPS
            && point.y <= y1 + PICK_EPS
        {
            best = Some((distance, point));
        }
    }

    // Стенка −X: есть, если сосед слева ниже. У края мира сосед — море.
    let left = if tile.x > 0 {
        terrain_elevation(world, TilePos { x: tile.x - 1, y: tile.y })
    } else {
        SEA_WORLD_HEIGHT
    };
    if elevation > left + PICK_EPS && direction.x.abs() > PICK_EPS {
        let distance = (x0 - origin.x) / direction.x;
        if distance >= t_enter - PICK_EPS && distance <= t_exit + PICK_EPS {
            let point = origin + direction * distance;
            if point.y >= y0 - PICK_EPS
                && point.y <= y1 + PICK_EPS
                && point.z >= left - PICK_EPS
                && point.z <= elevation + PICK_EPS
                && best.is_none_or(|(best_distance, _)| distance < best_distance)
            {
                best = Some((distance, point));
            }
        }
    }

    // Стенка −Y.
    let up = if tile.y > 0 {
        terrain_elevation(world, TilePos { x: tile.x, y: tile.y - 1 })
    } else {
        SEA_WORLD_HEIGHT
    };
    if elevation > up + PICK_EPS && direction.y.abs() > PICK_EPS {
        let distance = (y0 - origin.y) / direction.y;
        if distance >= t_enter - PICK_EPS && distance <= t_exit + PICK_EPS {
            let point = origin + direction * distance;
            if point.x >= x0 - PICK_EPS
                && point.x <= x1 + PICK_EPS
                && point.z >= up - PICK_EPS
                && point.z <= elevation + PICK_EPS
                && best.is_none_or(|(best_distance, _)| distance < best_distance)
            {
                best = Some((distance, point));
            }
        }
    }

    best
}

/// Интервал t, на котором координата луча остаётся в `[min, max]`.
/// Для неподвижной оси — вся числовая прямая, если origin внутри полосы.
fn axis_interval(origin: f32, direction: f32, min: f32, max: f32) -> Option<(f32, f32)> {
    if direction.abs() < PICK_EPS {
        if origin >= min && origin <= max {
            return Some((f32::NEG_INFINITY, f32::INFINITY));
        }
        return None;
    }
    let a = (min - origin) / direction;
    let b = (max - origin) / direction;
    Some((a.min(b), a.max(b)))
}

/// Резервное попадание вне мира/при отсутствии рельефа — плоскость моря.
fn base_plane_hit(origin: Vec3, direction: Vec3) -> Option<TerrainHit> {
    if direction.z.abs() < f32::EPSILON {
        return None;
    }
    let distance = (SEA_WORLD_HEIGHT - origin.z) / direction.z;
    if distance < 0.0 {
        return None;
    }
    let point = origin + direction * distance;
    Some(TerrainHit {
        point,
        tile: TilePos {
            x: (point.x / WORLD_TILE_SIZE).floor() as i32,
            y: (point.y / WORLD_TILE_SIZE).floor() as i32,
        },
    })
}



#[cfg(test)]
#[path = "../tests/isometric.rs"]
mod tests;
