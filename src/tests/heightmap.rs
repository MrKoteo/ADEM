//! Тесты слоя высот: точность упаковки, правка на месте, переупаковка,
//! сжатие однородных чанков и доступ через `World`.

use crate::simulation::commands::{Command, CommandQueue};
use crate::simulation::dirty::DirtyTiles;
use crate::simulation::scheduler::Scheduler;
use crate::simulation::crafts::CraftBook;
use crate::simulation::chunk::CHUNK_TILES;
use crate::simulation::heightmap::{Heightmap, HEIGHT_MAX};
use crate::simulation::world::{TilePos, World, WorldSize};

/// Массив с заданным локальным диапазоном: гарантирует минимальную
/// битовую ширину `bits`.
fn tiles_with_range(bits: u8) -> [u16; CHUNK_TILES] {
    let range = (1_u32 << bits) - 1;
    let base = HEIGHT_MAX - range as u16;
    let mut tiles = [base; CHUNK_TILES];
    tiles[0] = base;
    tiles[1] = base + range as u16;
    for (idx, tile) in tiles.iter_mut().enumerate().skip(2) {
        *tile = base + ((idx as u32 * 37) % (range + 1)) as u16;
    }
    tiles
}

#[test]
fn uniform_heightmap_returns_same_value_everywhere() {
    let map = Heightmap::uniform(1234);
    assert_eq!(map.uniform_value(), Some(1234));
    for idx in 0..CHUNK_TILES {
        assert_eq!(map.get(idx), 1234);
    }
    assert_eq!(map.memory_bytes(), 0);
}

#[test]
fn packed_heightmap_is_exact_for_all_bit_widths() {
    for bits in 1..=12 {
        let tiles = tiles_with_range(bits);
        let map = Heightmap::from_tiles(&tiles);
        let Heightmap::Packed(packed) = &map else {
            panic!("range {bits} must produce a packed chunk");
        };
        assert_eq!(packed.bits, bits, "minimal bit width");
        for (idx, &expected) in tiles.iter().enumerate() {
            assert_eq!(map.get(idx), expected, "bits={bits}, idx={idx}");
        }
        // Точный доступ после переупаковки не теряет ни одного значения.
        let mut roundtrip = map.clone();
        for (idx, &expected) in tiles.iter().enumerate() {
            roundtrip.set(idx, expected);
            assert_eq!(roundtrip.get(idx), expected);
        }
    }
}

#[test]
fn set_inside_range_edits_in_place() {
    let tiles = tiles_with_range(9);
    let mut map = Heightmap::from_tiles(&tiles);
    let before = map.memory_bytes();
    let base = tiles[0];
    let target = base + 7;

    let changed = map.set(100, target);
    assert!(changed);
    assert_eq!(map.get(100), target);
    assert_eq!(map.memory_bytes(), before, "in-place edit allocates nothing");
    assert!(!map.set(100, target), "same value is not a change");
}

#[test]
fn set_outside_range_repacks_chunk() {
    let mut map = Heightmap::from_tiles(&tiles_with_range(4));
    let changed = map.set(50, 0);
    assert!(changed);
    assert_eq!(map.get(50), 0);
    // Новый диапазон 0..base+15, значения остальных тайлов сохранены.
    let original = tiles_with_range(4);
    for (idx, &expected) in original.iter().enumerate() {
        if idx != 50 {
            assert_eq!(map.get(idx), expected, "idx={idx}");
        }
    }
}

#[test]
fn try_compress_collapses_equal_chunk() {
    let mut tiles = tiles_with_range(2);
    let value = tiles[0];
    tiles.fill(value);
    let map = Heightmap::from_tiles(&tiles);
    // from_tiles уже вернул Uniform; проверим путь через правку.
    assert_eq!(map.uniform_value(), Some(value));

    let mut non_uniform = [value; CHUNK_TILES];
    non_uniform[7] = value + 1;
    let mut map = Heightmap::from_tiles(&non_uniform);
    assert!(map.uniform_value().is_none());
    map.set(7, value);
    map.try_compress();
    assert_eq!(map.uniform_value(), Some(value));
    for idx in 0..CHUNK_TILES {
        assert_eq!(map.get(idx), value);
    }
}

#[test]
fn height_values_are_clamped_to_range() {
    let mut map = Heightmap::uniform(0);
    map.set(0, HEIGHT_MAX + 100);
    assert_eq!(map.get(0), HEIGHT_MAX);
    map.set(1, 0);
    assert_eq!(map.get(1), 0);
}

#[test]
fn world_height_roundtrip_across_chunk_border() {
    let mut world = World::new(WorldSize::new(256), 1);
    let left = TilePos { x: 15, y: 15 };
    let right = TilePos { x: 16, y: 15 };
    assert!(world.set_height(left, 1000));
    assert!(world.set_height(right, 2000));
    assert_eq!(world.height(left), Some(1000));
    assert_eq!(world.height(right), Some(2000));
    assert_eq!(world.height_or(TilePos { x: -1, y: 0 }, 7), 7);
    assert_eq!(world.height(TilePos { x: 256, y: 0 }), None);

    let before = world.memory_bytes();
    assert!(world.set_height(right, 1500));
    assert!(world.memory_bytes() >= before);
    assert_eq!(world.height(right), Some(1500));
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
            world.set_height(TilePos { x: center.x + dx, y: center.y + dy }, 200);
        }
    }
    world.set_height(center, 100);

    let dirty = apply_commands(&mut world, &[Command::SmoothHeight { pos: center }]);
    assert_eq!(world.height(center), Some(200));
    assert!(dirty.terrain.contains(&center));

    // Вне мира команда игнорируется.
    let outside = TilePos { x: -5, y: 0 };
    let dirty = apply_commands(&mut world, &[Command::AddHeight { pos: outside, delta: 10 }]);
    assert!(dirty.terrain.is_empty());
}
