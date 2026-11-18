//! Тесты мира: чанки, слои (рельеф/ресурсы/объекты) и размер мира.

use crate::simulation::chunk::{CHUNK_TILES, Chunk, TerrainTiles};
use crate::simulation::heightmap::{Heightmap, SEA_LEVEL};
use crate::simulation::tile::TileType;
use crate::simulation::world::{World, WorldSize};

// Запись и чтение рельефа и ресурса идут через одни и те же слои без перекосов.
//#[test]

/// Пустой чанк: рельеф Empty в любой позиции, ресурсов и объектов нет,
/// высота по умолчанию — уровень моря (океан).
#[test]
fn chunk_empty_by_default() {
    let c = Chunk::empty();
    for idx in 0..CHUNK_TILES {
        assert_eq!(c.terrain.get(idx), TileType::Empty);
        assert_eq!(c.heights.get(idx), SEA_LEVEL);
    }
    assert_eq!(c.heights.uniform_value(), Some(SEA_LEVEL));
    assert!(c.resources.is_empty());
    assert!(c.objects.is_empty());
}

/// Слой высот разворачивается в плотный массив при первой правке и
/// схлопывается обратно, когда все тайлы снова сравнялись.
#[test]
fn chunk_heights_compress_and_expand() {
    let mut c = Chunk::empty();
    c.heights = Heightmap::uniform(SEA_LEVEL);
    c.heights.set(5, 7);
    assert!(matches!(c.heights, Heightmap::Full(_)));
    assert_eq!(c.heights.get(5), 7);
    assert_eq!(c.heights.get(6), SEA_LEVEL);
    assert!(c.heights.memory_bytes() > 0);

    c.heights.set(5, SEA_LEVEL);
    c.heights.try_compress();
    assert!(matches!(c.heights, Heightmap::Uniform(SEA_LEVEL)));
    assert_eq!(c.heights.memory_bytes(), 0);
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

/// Порционный забор dirty-рельефа (разрежение пересборок на дальних LOD):
/// остаток остаётся в очереди, повторный забор не теряет тайлы и дедупликация
/// продолжает работать.
#[test]
fn dirty_tiles_batch_keeps_remainder() {
    use crate::simulation::dirty::DirtyTiles;
    use crate::simulation::world::TilePos;

    let mut dirty = DirtyTiles::default();
    for x in 0..5 {
        dirty.terrain(TilePos { x, y: 0 });
    }
    let first = dirty.take_terrain_batch(2);
    assert_eq!(first.len(), 2);
    assert_eq!(dirty.terrain.len(), 3);
    // Тайл, уже отданный рендеру, можно пометить заново.
    dirty.terrain(first[0]);
    assert_eq!(dirty.terrain.len(), 4);
    let rest = dirty.take_terrain_batch(usize::MAX);
    assert_eq!(rest.len(), 4);
    assert!(dirty.terrain.is_empty());
}

/// Разреженный слой чанка не занимает таблицу, пока пуст, и ведёт себя как
/// карта: вставка/чтение/удаление, обход по ссылке, сохранение порядка
/// (сортировка ключей делается потребителем). Загрузка сейва кладёт готовую
/// таблицу через `from_map`, пустая — не аллоцирует.
#[test]
fn sparse_layer_behaves_like_map() {
    use crate::simulation::chunk::SparseLayer;

    let mut layer: SparseLayer<u16> = SparseLayer::new();
    assert!(layer.is_empty());
    assert_eq!(layer.get(&3), None);
    assert_eq!(layer.insert(3, 30), None);
    assert_eq!(layer.insert(7, 70), None);
    assert_eq!(layer.insert(3, 31), Some(30));
    assert_eq!(layer.len(), 2);
    assert_eq!(layer.get(&3), Some(&31));
    assert!(layer.contains_key(&7));
    assert_eq!(layer.remove(&3), Some(31));
    assert_eq!(layer.remove(&3), None);
    assert_eq!(layer.len(), 1);

    let mut visited: Vec<(u8, u16)> = (&layer).into_iter().map(|(k, v)| (*k, *v)).collect();
    visited.sort_unstable();
    assert_eq!(visited, vec![(7, 70)]);
    assert_eq!(layer.keys().copied().collect::<Vec<_>>(), vec![7]);
    assert_eq!(layer.values().copied().collect::<Vec<_>>(), vec![70]);

    layer.retain(|_, value| *value > 100);
    assert!(layer.is_empty());

    let mut from_map = std::collections::HashMap::new();
    assert!(SparseLayer::from_map(from_map.clone()).is_empty());
    from_map.insert(1, 10);
    assert_eq!(SparseLayer::from_map(from_map).len(), 1);

    layer.insert(5, 50);
    layer.clear();
    assert!(layer.is_empty());
    assert_eq!(layer.get(&5), None);
}
