//! Тесты мира: чанки, слои (рельеф/ресурсы/объекты) и размер мира.

use crate::simulation::chunk::{CHUNK_TILES, Chunk, TerrainTiles};
use crate::simulation::tile::TileType;
use crate::simulation::world::{World, WorldSize};

// Запись и чтение рельефа и ресурса идут через одни и те же слои без перекосов.
//#[test]

/// Пустой чанк: рельеф Empty в любой позиции, ресурсов и объектов нет.
#[test]
fn chunk_empty_by_default() {
    let c = Chunk::empty();
    for idx in 0..CHUNK_TILES {
        assert_eq!(c.terrain.get(idx), TileType::Empty);
    }
    assert!(c.resources.is_empty());
    assert!(c.objects.is_empty());
}

/// Рельеф чанка разворачивается из Uniform в Full при первом отличии
/// и схлопывается обратно, когда тайлы снова стали одинаковыми.
#[test]
fn chunk_terrain_compresses_and_expands() {
    let mut c = Chunk::empty();
    c.terrain = TerrainTiles::uniform(TileType::Water);
    assert!(matches!(c.terrain, TerrainTiles::Uniform(TileType::Water)));

    c.terrain.set(5, TileType::Sand);
    assert!(matches!(c.terrain, TerrainTiles::Full(_)));
    assert_eq!(c.terrain.get(5), TileType::Sand);
    assert_eq!(c.terrain.get(6), TileType::Water);

    c.terrain.set(5, TileType::Water);
    c.terrain.try_compress();
    assert!(matches!(c.terrain, TerrainTiles::Uniform(TileType::Water)));
}

/// Все чанки мира имеют одинаковый размер.
#[test]
fn chunks_per_side_matches_size() {
    for size in [WorldSize::new(256), WorldSize::new(512), WorldSize::new(1024)] {
        let w = World::new(size, 1);
        assert_eq!(w.chunks_per_side, size.tiles() / 16);
        assert_eq!(w.chunks.len(), (w.chunks_per_side * w.chunks_per_side) as usize);
    }
}

/// Размер мира зажимается в пределы и выравнивается на кратность чанку.
#[test]
fn world_size_clamped_and_snapped() {
    assert_eq!(WorldSize::new(300).tiles(), 288);
    assert_eq!(WorldSize::new(0).tiles(), WorldSize::MIN);
    assert_eq!(WorldSize::new(u32::MAX).tiles(), WorldSize::MAX);
    assert_eq!(WorldSize::new(256).tiles(), 256);
    assert_eq!(WorldSize::new(1024).label(), "1024×1024");
    assert_eq!(WorldSize::MIN % 16, 0);
    assert_eq!(WorldSize::MAX % 16, 0);
}

/// Повторная пометка того же тайла в канале dirty не дублируется: иначе
/// рендер и освещение получали бы лишние записи.
#[test]
fn dirty_tiles_deduplicate_within_channel() {
    use crate::simulation::dirty::DirtyTiles;
    use crate::simulation::world::TilePos;

    let mut dirty = DirtyTiles::default();
    let a = TilePos { x: 3, y: 4 };
    dirty.terrain(a);
    dirty.terrain(a);
    dirty.decor(a);
    dirty.decor(a);
    assert_eq!(dirty.terrain, vec![a]);
    assert_eq!(dirty.decor, vec![a]);
    // Один и тот же тайл в разных каналах — не дубль (каналы независимы).
    assert_eq!(dirty.terrain.len() + dirty.decor.len(), 2);
    dirty.clear();
    assert!(dirty.is_empty());
}
