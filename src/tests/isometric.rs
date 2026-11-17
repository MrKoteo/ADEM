use super::*;
use crate::simulation::{TileType, WorldSize};

/// Луч сверху вниз попадает в верхнюю плоскость тайла на его высоте.
#[test]
fn terrain_ray_hits_the_tile_surface_height() {
    let mut world = World::new(WorldSize::new(256), 1);
    let tile = TilePos { x: 2, y: 3 };
    world.set_terrain(tile, TileType::Mountain);
    world.set_height(tile, 2000);
    let expected = height_to_world(2000);
    let point = intersect_terrain(
        Vec3::new(
            2.5 * WORLD_TILE_SIZE,
            3.5 * WORLD_TILE_SIZE,
            expected + 100.0,
        ),
        Vec3::new(0.0, 0.0, -1.0),
        &world,
    )
    .expect("ray should hit the mountain top");

    assert!((point.point.z - expected).abs() < 1e-4);
    assert_eq!(point.tile, tile);
}

/// Вне мира луч попадает в плоскость моря, а не в z=0: у края карты нет
/// обрыва на высоту моря.
#[test]
fn terrain_ray_outside_world_uses_the_sea_plane() {
    let world = World::new(WorldSize::new(256), 1);
    let point = intersect_terrain(
        Vec3::new(-WORLD_TILE_SIZE, WORLD_TILE_SIZE, 500.0),
        Vec3::new(0.0, 0.0, -1.0),
        &world,
    )
    .expect("ray should hit the sea plane outside the map");

    assert!((point.point.z - SEA_WORLD_HEIGHT).abs() < 1e-4);
    assert_eq!(point.tile.x, -1);
}

/// Луч сбоку попадает в видимую стенку поднятого тайла раньше, чем в его
/// верхнюю плоскость.
#[test]
fn terrain_ray_hits_a_visible_mountain_side() {
    let mut world = World::new(WorldSize::new(256), 1);
    let raised = TilePos { x: 3, y: 2 };
    world.set_terrain(raised, TileType::Mountain);
    world.set_height(raised, 2000);
    let top = height_to_world(2000);
    let point = intersect_terrain(
        Vec3::new(
            2.5 * WORLD_TILE_SIZE,
            2.5 * WORLD_TILE_SIZE,
            top * 0.75,
        ),
        Vec3::new(0.866_025_4, 0.0, -0.5),
        &world,
    )
    .expect("ray should hit the mountain side before the lower tile");

    assert_eq!(point.tile, raised);
    assert!((point.point.z > 0.0) && (point.point.z < top));
}

/// Вторая видимая стенка (−Y) тоже участвует в пикинге.
#[test]
fn terrain_ray_hits_the_other_visible_mountain_side() {
    let mut world = World::new(WorldSize::new(256), 1);
    let raised = TilePos { x: 2, y: 3 };
    world.set_terrain(raised, TileType::Mountain);
    world.set_height(raised, 2000);
    let top = height_to_world(2000);
    let point = intersect_terrain(
        Vec3::new(
            2.5 * WORLD_TILE_SIZE,
            2.5 * WORLD_TILE_SIZE,
            top * 0.75,
        ),
        Vec3::new(0.0, 0.866_025_4, -0.5),
        &world,
    )
    .expect("ray should hit the mountain's other visible side");

    assert_eq!(point.tile, raised);
    assert!((point.point.z > 0.0) && (point.point.z < top));
}

/// Рендер больше не квантует высоты: каждый уровень — 0.5 текселя,
/// 2 уровня — 1 тексель.
#[test]
fn every_height_level_is_rendered() {
    assert_eq!(height_to_world(0), 0.0);
    assert_eq!(height_to_world(1), 0.5);
    assert_eq!(height_to_world(2), 1.0);
    assert_eq!(height_to_world(4), 2.0);
    assert_eq!(height_to_world(4095), 2047.5);
    // Шаг кисти соответствует новой минимальной единице: 2 уровня = 1 тексель.
    assert_eq!(crate::app::state::HEIGHT_BRUSH_STEP, 2);
    assert_eq!(HEIGHT_WORLD_UNIT * 2.0, 1.0);
}

/// Высота в мировых единицах линейна, а плоскость моря соответствует
/// `SEA_LEVEL`; тип тайла на неё больше не влияет.
#[test]
fn heights_are_data_not_tile_types() {
    assert_eq!(height_to_world(0), 0.0);
    assert!((height_to_world(1000) - 500.0).abs() < 1e-4);
    assert!((SEA_WORLD_HEIGHT - height_to_world(SEA_LEVEL)).abs() < 1e-4);

    let mut world = World::new(WorldSize::new(256), 1);
    let tile = TilePos { x: 4, y: 4 };
    world.set_terrain(tile, TileType::Grass);
    assert_eq!(terrain_elevation(&world, tile), 0.0);
    world.set_height(tile, 500);
    assert!((terrain_elevation(&world, tile) - 250.0).abs() < 1e-4);
    world.set_terrain(tile, TileType::Mountain);
    assert!((terrain_elevation(&world, tile) - 250.0).abs() < 1e-4);
}
