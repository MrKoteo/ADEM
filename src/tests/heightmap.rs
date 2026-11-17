//! Тесты слоя высот: плотный массив u8, схлопывание однородных чанков,
//! правка на месте и доступ через `World`.

use crate::simulation::chunk::CHUNK_TILES;
use crate::simulation::commands::{Command, CommandQueue};
use crate::simulation::crafts::CraftBook;
use crate::simulation::dirty::DirtyTiles;
use crate::simulation::heightmap::{Heightmap, HEIGHT_MAX, SEA_LEVEL};
use crate::simulation::scheduler::Scheduler;
use crate::simulation::world::{TilePos, World, WorldSize};

/// Массив с заданным локальным диапазоном: гарантирует неоднородность.
fn tiles_with_range(range: u8) -> [u8; CHUNK_TILES] {
    let base = HEIGHT_MAX - range;
    let mut tiles = [base; CHUNK_TILES];
    tiles[0] = base;
    tiles[1] = base + range;
    for (idx, tile) in tiles.iter_mut().enumerate().skip(2) {
        *tile = base + ((idx as u32 * 37) % (range as u32 + 1)) as u8;
    }
    tiles
}

#[test]
fn uniform_heightmap_returns_same_value_everywhere() {
    let map = Heightmap::uniform(123);
    assert_eq!(map.uniform_value(), Some(123));
    for idx in 0..CHUNK_TILES {
        assert_eq!(map.get(idx), 123);
    }
    assert_eq!(map.memory_bytes(), 0);
}

#[test]
fn empty_heightmap_defaults_to_sea_level() {
    let map = Heightmap::uniform(SEA_LEVEL);
    assert_eq!(map.uniform_value(), Some(0));
    for idx in 0..CHUNK_TILES {
        assert_eq!(map.get(idx), 0, "пустая ячейка — высота по умолчанию");
    }
}

#[test]
fn full_heightmap_is_exact_for_all_ranges() {
    for range in [1_u8, 2, 3, 7, 15, 31, 63, 127, 255] {
        let tiles = tiles_with_range(range);
        let map = Heightmap::from_tiles(&tiles);
        let Heightmap::Full(stored) = &map else {
            panic!("range {range} must produce a full chunk");
        };
        assert_eq!(stored.as_slice(), tiles.as_slice());
        for (idx, &expected) in tiles.iter().enumerate() {
            assert_eq!(map.get(idx), expected, "range={range}, idx={idx}");
        }
        // Точный доступ после правки не теряет ни одного значения.
        let mut roundtrip = map.clone();
        for (idx, &expected) in tiles.iter().enumerate() {
            roundtrip.set(idx, expected);
            assert_eq!(roundtrip.get(idx), expected);
        }
    }
}

#[test]
fn set_inside_full_edits_in_place() {
    let tiles = tiles_with_range(9);
    let mut map = Heightmap::from_tiles(&tiles);
    let before = map.memory_bytes();
    let target = 7;

    let changed = map.set(100, target);
    assert!(changed);
    assert_eq!(map.get(100), target);
    assert_eq!(map.memory_bytes(), before, "in-place edit allocates nothing");
    assert!(!map.set(100, target), "same value is not a change");
}

#[test]
fn set_expands_uniform_and_compress_collapses_back() {
    let mut map = Heightmap::uniform(10);
    assert!(map.set(50, 11));
    assert!(map.uniform_value().is_none());
    assert_eq!(map.get(50), 11);
    assert_eq!(map.get(0), 10);

    map.set(50, 10);
    map.try_compress();
    assert_eq!(map.uniform_value(), Some(10));
    for idx in 0..CHUNK_TILES {
        assert_eq!(map.get(idx), 10);
    }
}

#[test]
fn height_values_cover_full_u8_range() {
    let mut map = Heightmap::uniform(0);
    map.set(0, HEIGHT_MAX);
    assert_eq!(map.get(0), HEIGHT_MAX);
    map.set(1, 0);
    assert_eq!(map.get(1), 0);
}

#[test]
fn world_height_roundtrip_across_chunk_border() {
    let mut world = World::new(WorldSize::new(256), 1);
    let left = TilePos { x: 15, y: 15 };
    let right = TilePos { x: 16, y: 15 };
    assert!(world.set_height(left, 100));
    assert!(world.set_height(right, 200));
    assert_eq!(world.height(left), Some(100));
    assert_eq!(world.height(right), Some(200));
    assert_eq!(world.height_or(TilePos { x: -1, y: 0 }, 7), 7);
    assert_eq!(world.height(TilePos { x: 256, y: 0 }), None);

    let before = world.memory_bytes();
    assert!(world.set_height(right, 150));
    assert!(world.memory_bytes() >= before);
    assert_eq!(world.height(right), Some(150));
}

/// Применяет очередь команд без актора (инструменты редактора).
fn apply_commands(world: &mut World, commands: &[Command]) -> DirtyTiles {
    let mut queue = CommandQueue::default();
    for command in commands {
        queue.push(*command);
    }
    let mut dirty = DirtyTiles::default();
    queue.apply_all(
        world,
        None,
        &mut dirty,
        &mut Scheduler::default(),
        0,
        &CraftBook::default(),
    );
    dirty
}

#[test]
fn add_height_command_clamps_and_marks_dirty() {
    let mut world = World::new(WorldSize::new(256), 1);
    let pos = TilePos { x: 10, y: 10 };

    let dirty = apply_commands(&mut world, &[Command::AddHeight { pos, delta: 10 }]);
    assert_eq!(world.height(pos), Some(10));
    assert!(dirty.terrain.contains(&pos));

    apply_commands(&mut world, &[Command::AddHeight { pos, delta: -100 }]);
    assert_eq!(world.height(pos), Some(0));

    world.set_height(pos, HEIGHT_MAX);
    let dirty = apply_commands(&mut world, &[Command::AddHeight { pos, delta: 100 }]);
    assert_eq!(world.height(pos), Some(HEIGHT_MAX));
    assert!(!dirty.terrain.contains(&pos), "clamped value is not a change");
}

#[test]
fn smooth_height_command_averages_neighbors() {
    let mut world = World::new(WorldSize::new(256), 1);
    let center = TilePos { x: 30, y: 30 };
    for dy in -1..=1 {
        for dx in -1..=1 {
            world.set_height(TilePos { x: center.x + dx, y: center.y + dy }, 100);
        }
    }
    world.set_height(center, 20);

    let dirty = apply_commands(&mut world, &[Command::SmoothHeight { pos: center }]);
    assert_eq!(world.height(center), Some(100));
    assert!(dirty.terrain.contains(&center));

    // Вне мира команда игнорируется.
    let outside = TilePos { x: -5, y: 0 };
    let dirty = apply_commands(&mut world, &[Command::AddHeight { pos: outside, delta: 10 }]);
    assert!(dirty.terrain.is_empty());
}
