//! Процедурная генерация карты: Heightmap — источник истины.
//!
//! Схема:
//! 1. Базовое поле высот: архетип карты из seed, domain warp, континенты,
//!    гребневый шум гор и спад к краям.
//! 2. Термальная эрозия: сглаживает слишком крутые склоны.
//! 3. Гидрология: бессточное заполнение депрессий, водосборы, реки, озёра
//!    и море. Уровни воды согласованы с локальным рельефом.
//! 4. Классификация: тип тайла выводится из итоговой высоты, климата чанка
//!    и воды. Пляжи — по близости к воде.
//! 5. Объекты, ресурсы и дикая природа — отдельными RNG-потоками.
//!
//! `HeightField` — временный буфер генерации (2 Б/тайл); в чанки он
//! записывается один раз в конце и освобождается до установки мира.

pub(crate) mod height;
mod hydrology;

use std::collections::VecDeque;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng}; // Bevy 0.20 / rand 0.10: RngExt вместо Rng

use super::chunk::{ResourceDeposit, TerrainTiles, CHUNK_SIZE, CHUNK_TILES};
use super::climate::CLIMATE_CELL_TILES;
use super::entities::EntityKind;
use super::heightmap::{Heightmap, HEIGHT_MAX};
use super::objects::WorldObject;
use super::tile::{ResourceKind, TileType};
use super::world::{TilePos, World, WorldPos, WorldSize};

use height::{
    erode_thermal, fill_base_heights, HeightField, MapArchetype, BEACH_BAND, MOUNTAIN_HEIGHT,
    STONE_HEIGHT, TREE_LINE,
};
use hydrology::{build_hydrology, WaterKind};
use super::noise::{fbm, smoothstep};

#[allow(unused_imports)] // используется тестами через `generation::*`
pub(crate) use height::{elevation_at, height_for_elevation};

/// Масштаб шума мха, тайлов: мелкий масштаб даёт пятна-области, а не крап.
const MOSS_SCALE: f32 = 20.0;
/// Порог мшистости: доля лесной травы, зарастающей мхом.
const MOSS_THRESHOLD: f32 = 0.62;
/// Максимальная ширина пляжа от воды, тайлов.
const BEACH_MAX_DIST: u8 = 2;
/// Предел BFS расстояния до воды для прибрежной влажности, ячеек 2×2.
const COAST_MAX_CELLS: u8 = 16;

const NEIGHBORS_4: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];

/// Сгенерировать мир: рельеф + вода + ресурсы + объекты + существа.
#[allow(dead_code)] // синхронный вариант для тестов; игра идёт через cancellable
pub fn generate_world(size: WorldSize, seed: u64) -> World {
    generate_world_cancellable(size, seed, &|| false)
        .expect("generation without cancellation always completes")
}

/// Генерация мира с проверкой отмены на каждом проходе.
pub fn generate_world_cancellable(
    size: WorldSize,
    seed: u64,
    cancelled: &dyn Fn() -> bool,
) -> Option<World> {
    let mut world = World::new(size, seed);
    let n = size.tiles() as i32;
    let archetype = MapArchetype::for_seed(seed);

    let mut heights = HeightField::new(n as usize);
    if !fill_base_heights(&mut heights, n, seed, archetype, cancelled) {
        return None;
    }
    if !erode_thermal(&mut heights, n, cancelled) {
        return None;
    }
    let water = build_hydrology(&mut heights, n, cancelled)?;

    if !classify_and_write(&mut world, &heights, &water, seed, cancelled) {
        return None;
    }
    if !place_content(&mut world, &heights, seed, cancelled) {
        return None;
    }

    let mut rng = StdRng::seed_from_u64(seed ^ 0x5EED_AAAA_BBBB_CCCC);
    if !spawn_wildlife(&mut world, &mut rng, cancelled)
        || !spawn_rabbits(&mut world, &mut rng, cancelled)
    {
        return None;
    }

    // Однородные чанки (океаны, плато) схлопываем в Uniform — экономия ОЗУ.
    let chunks_per_side = world.chunks_per_side as usize;
    for (index, chunk) in world.chunks.iter_mut().enumerate() {
        if index % chunks_per_side == 0 && cancelled() {
            return None;
        }
        chunk.terrain.try_compress();
        chunk.heights.try_compress();
    }

    Some(world)
}

/// Классифицирует тайлы и записывает рельеф/высоты в чанки.
fn classify_and_write(
    world: &mut World,
    heights: &HeightField,
    water: &[WaterKind],
    seed: u64,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let n = world.size.tiles() as i32;
    let width = n as usize;
    let chunks_per_side = world.chunks_per_side as usize;
    let Some(beach) = mark_beaches(heights, water, n, cancelled) else {
        return false;
    };
    let coast = build_coast_distance(water, n);
    let biomes = build_biome_map(n, seed, heights, &coast);
    let climate = &world.climate;

    for cy in 0..chunks_per_side {
        if cy % 8 == 0 && cancelled() {
            return false;
        }
        for cx in 0..chunks_per_side {
            let chunk_index = cy * chunks_per_side + cx;
            let base_x = (cx * CHUNK_SIZE) as i32;
            let base_y = (cy * CHUNK_SIZE) as i32;

            let mut tiles = [TileType::Empty; CHUNK_TILES];
            let mut tile_heights = [0_u16; CHUNK_TILES];
            for ly in 0..CHUNK_SIZE {
                let y = base_y + ly as i32;
                for lx in 0..CHUNK_SIZE {
                    let x = base_x + lx as i32;
                    let index = y as usize * width + x as usize;
                    let local = ly * CHUNK_SIZE + lx;
                    let height = heights.get(x, y);
                    let annual_temperature = climate.annual_temperature_c(x, y, height);
                    let moisture = biomes.moisture_at(x, y);
                    let moss = biomes.moss_at(x, y);
                    tile_heights[local] = height;
                    let beach = beach.kind(y as usize * width + x as usize);
                    tiles[local] = classify_tile(
                        water[index],
                        height,
                        beach,
                        annual_temperature,
                        moisture,
                        moss,
                    );
                }
            }

            let chunk = &mut world.chunks[chunk_index];
            chunk.terrain = TerrainTiles::Full(Box::new(tiles));
            chunk.heights = Heightmap::from_tiles(&tile_heights);
        }
    }
    true
}

/// Тип тайла по итоговой высоте, температуре, влажности и пляжу.
fn classify_tile(
    water: WaterKind,
    height: u16,
    beach: BeachKind,
    annual_temperature_c: f32,
    moisture: f32,
    moss: f32,
) -> TileType {
    if water.is_water() {
        return match water {
            WaterKind::SeaShallow | WaterKind::Lake => TileType::Water,
            WaterKind::SeaDeep | WaterKind::LakeDeep => TileType::DeepWater,
            WaterKind::SeaOcean => TileType::Ocean,
            WaterKind::River => TileType::River,
            WaterKind::None => TileType::Empty,
        };
    }

    match beach {
        // Берега рек — всегда гравий; морские/озёрные — песок, кроме
        // холодных и высоких, где тоже уместнее гравий.
        BeachKind::Gravel => return TileType::Gravel,
        BeachKind::Sand => {
            return if annual_temperature_c < -5.0 || height > MOUNTAIN_HEIGHT {
                TileType::Gravel
            } else {
                TileType::Sand
            };
        }
        BeachKind::None => {}
    }

    if height >= MOUNTAIN_HEIGHT {
        return TileType::Mountain;
    }
    if height >= STONE_HEIGHT {
        return TileType::Stone;
    }

    // Биом по непрерывной влажности; мох — пятнами мелкого шума в лесу.
    if moisture < 0.30 {
        TileType::Dirt
    } else if moisture < 0.58 {
        TileType::Grass
    } else if moisture > 0.55 && annual_temperature_c > -5.0 && moss > MOSS_THRESHOLD {
        TileType::Moss
    } else {
        TileType::ForestGrass
    }
}

/// Расстояние до воды по ячейкам 2×2, в клетках (0 — вода). BFS с
/// ограничением `COAST_MAX_CELLS`; используется только при генерации.
fn build_coast_distance(water: &[WaterKind], n: i32) -> Vec<u8> {
    let cell = CLIMATE_CELL_TILES as usize;
    let side = (n as usize).div_ceil(cell);
    let mut distance = vec![u8::MAX; side * side];
    let mut queue = VecDeque::new();
    for y in 0..n {
        for x in 0..n {
            let index = y as usize * n as usize + x as usize;
            if !water[index].is_water() {
                continue;
            }
            let cx = x as usize / cell;
            let cy = y as usize / cell;
            let ci = cy * side + cx;
            if distance[ci] != 0 {
                distance[ci] = 0;
                queue.push_back((cx as i32, cy as i32));
            }
        }
    }
    while let Some((cx, cy)) = queue.pop_front() {
        let d = distance[cy as usize * side + cx as usize];
        if d >= COAST_MAX_CELLS {
            continue;
        }
        for (dx, dy) in NEIGHBORS_4 {
            let nx = cx + dx;
            let ny = cy + dy;
            if nx < 0 || ny < 0 || nx >= side as i32 || ny >= side as i32 {
                continue;
            }
            let ni = ny as usize * side + nx as usize;
            if distance[ni] == u8::MAX {
                distance[ni] = d + 1;
                queue.push_back((nx, ny));
            }
        }
    }
    distance
}

/// Карта влажности и мха по ячейкам 2×2. Считается один раз на генерацию,
/// поэтому дорогие fbm-вызовы не повторяются на каждый тайл.
struct BioMap {
    side: usize,
    moisture: Vec<u8>,
    moss: Vec<u8>,
}

impl BioMap {
    fn moisture_at(&self, x: i32, y: i32) -> f32 {
        sample_u8_map(&self.moisture, self.side, x, y) / 255.0
    }

    fn moss_at(&self, x: i32, y: i32) -> f32 {
        sample_u8_map(&self.moss, self.side, x, y) / 255.0
    }
}

/// Строит карту влажности/мха в ячейках 2×2 (0.5 Б/тайл временно).
fn build_biome_map(
    n: i32,
    seed: u64,
    heights: &HeightField,
    coast: &[u8],
) -> BioMap {
    let cell = CLIMATE_CELL_TILES as usize;
    let side = (n as usize).div_ceil(cell);
    let mut moisture = vec![0_u8; side * side];
    let mut moss = vec![0_u8; side * side];
    for cy in 0..side {
        for cx in 0..side {
            let x = (cx * cell + cell / 2).min(n as usize - 1) as i32;
            let y = (cy * cell + cell / 2).min(n as usize - 1) as i32;
            let index = cy * side + cx;
            let height = heights.get(x, y);
            let m = moisture_at(x, y, height, coast[index], seed);
            let moss_value = moss_at(x, y, seed);
            moisture[index] = (m * 255.0).round().clamp(0.0, 255.0) as u8;
            moss[index] = (moss_value * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    BioMap {
        side,
        moisture,
        moss,
    }
}

/// Билинейная выборка из карты 0..255 по координате тайла.
fn sample_u8_map(data: &[u8], side: usize, x: i32, y: i32) -> f32 {
    let cell = CLIMATE_CELL_TILES as f32;
    let fx = x as f32 / cell - 0.5;
    let fy = y as f32 / cell - 0.5;
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let sample = |ix: i32, iy: i32| {
        let ix = ix.clamp(0, side as i32 - 1) as usize;
        let iy = iy.clamp(0, side as i32 - 1) as usize;
        data[iy * side + ix] as f32
    };
    let top = sample(x0, y0) + (sample(x0 + 1, y0) - sample(x0, y0)) * tx;
    let bottom = sample(x0, y0 + 1) + (sample(x0 + 1, y0 + 1) - sample(x0, y0 + 1)) * tx;
    top + (bottom - top) * ty
}

/// Влажность ячейки: крупные + средние пятна шума и прибрежная добавка.
fn moisture_at(x: i32, y: i32, height: u16, coast_distance: u8, seed: u64) -> f32 {
    let large = fbm(
        x as f32 / 48.0 + 17.0,
        y as f32 / 48.0 + 61.0,
        seed ^ 0x0A01_5EED,
        2,
    );
    let detail = fbm(
        x as f32 / 16.0,
        y as f32 / 16.0,
        seed ^ 0x0A02_5EED,
        2,
    );
    let coastal = 0.18 * (1.0 - smoothstep(coast_distance as f32 / 12.0));
    let altitude = height as f32 / HEIGHT_MAX as f32;
    (large * 0.72 + detail * 0.28 + coastal - altitude * 0.10).clamp(0.0, 1.0)
}

/// Мшистость тайла: мелкий шум — пятна мха, а не одиночный крап.
fn moss_at(x: i32, y: i32, seed: u64) -> f32 {
    let fx = x as f32 / MOSS_SCALE;
    let fy = y as f32 / MOSS_SCALE;
    fbm(fx + 211.0, fy + 137.0, seed ^ 0x0DD5_1DEA, 2)
}

/// Вид пляжа: обычный песчаный или гравийный (русла рек и холодные берега).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BeachKind {
    None,
    Sand,
    Gravel,
}

/// Две битовые карты берегов: песок и гравий.
struct Beaches {
    sand: BitGrid,
    gravel: BitGrid,
}

impl Beaches {
    fn new(side: usize) -> Self {
        Self {
            sand: BitGrid::new(side),
            gravel: BitGrid::new(side),
        }
    }

    #[inline]
    fn kind(&self, index: usize) -> BeachKind {
        if self.gravel.get(index) {
            BeachKind::Gravel
        } else if self.sand.get(index) {
            BeachKind::Sand
        } else {
            BeachKind::None
        }
    }

    #[inline]
    fn contains(&self, index: usize) -> bool {
        self.gravel.get(index) || self.sand.get(index)
    }
}

/// Битовая карта пляжей: тайлы суши в пределах `BEACH_BAND` от воды.
struct BitGrid {
    words: Vec<u64>,
}

impl BitGrid {
    fn new(side: usize) -> Self {
        Self {
            words: vec![0; side * side / 64 + 1],
        }
    }

    #[inline]
    fn get(&self, index: usize) -> bool {
        self.words[index / 64] & (1 << (index % 64)) != 0
    }

    #[inline]
    fn set(&mut self, index: usize) {
        self.words[index / 64] |= 1 << (index % 64);
    }
}

/// Размечает пляжи ограниченным BFS от воды. Уровень воды передаётся в
/// очереди, поэтому полная карта уровней не нужна.
fn mark_beaches(
    heights: &HeightField,
    water: &[WaterKind],
    n: i32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Beaches> {
    let width = n as usize;
    let mut beaches = Beaches::new(width);
    let mut queue: VecDeque<(i32, i32, u16, u8, BeachKind)> = VecDeque::new();

    for y in 0..n {
        if y % 64 == 0 && cancelled() {
            return None;
        }
        for x in 0..n {
            let index = y as usize * width + x as usize;
            if !water[index].is_water() {
                continue;
            }
            let level = heights.get(x, y);
            // Берега рек — гравий, морей и озёр — песок (холод/высота
            // превратят его в гравий на классификации).
            let kind = if water[index] == WaterKind::River {
                BeachKind::Gravel
            } else {
                BeachKind::Sand
            };
            for (dx, dy) in NEIGHBORS_4 {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= n || ny >= n {
                    continue;
                }
                let ni = ny as usize * width + nx as usize;
                if water[ni].is_water() || beaches.contains(ni) {
                    continue;
                }
                let h = heights.get(nx, ny);
                if h > level && h <= level + BEACH_BAND {
                    mark_beach(&mut beaches, ni, kind);
                    queue.push_back((nx, ny, level, 1, kind));
                }
            }
        }
    }

    let mut steps = 0_u32;
    while let Some((x, y, level, distance, kind)) = queue.pop_front() {
        steps += 1;
        if steps.is_multiple_of(65_536) && cancelled() {
            return None;
        }
        if distance >= BEACH_MAX_DIST {
            continue;
        }
        for (dx, dy) in NEIGHBORS_4 {
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= n || ny >= n {
                continue;
            }
            let ni = ny as usize * width + nx as usize;
            if water[ni].is_water() || beaches.contains(ni) {
                continue;
            }
            let h = heights.get(nx, ny);
            if h > level && h <= level + BEACH_BAND {
                mark_beach(&mut beaches, ni, kind);
                queue.push_back((nx, ny, level, distance + 1, kind));
            }
        }
    }
    Some(beaches)
}

#[inline]
fn mark_beach(beaches: &mut Beaches, index: usize, kind: BeachKind) {
    match kind {
        BeachKind::Gravel => beaches.gravel.set(index),
        BeachKind::Sand => beaches.sand.set(index),
        BeachKind::None => {}
    }
}

/// Объекты, ресурсы и глина. Отдельный RNG-поток, чтобы правки объектов не
/// сдвигали шум рельефа.
fn place_content(
    world: &mut World,
    heights: &HeightField,
    seed: u64,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let chunks_per_side = world.chunks_per_side as usize;
    let mut rng = StdRng::seed_from_u64(seed ^ 0x0B1E_C7A5_1234_5678);
    for cy in 0..chunks_per_side {
        if cancelled() {
            return false;
        }
        for cx in 0..chunks_per_side {
            let chunk = &mut world.chunks[cy * chunks_per_side + cx];
            for ly in 0..CHUNK_SIZE {
                for lx in 0..CHUNK_SIZE {
                    let idx = ly * CHUNK_SIZE + lx;
                    let x = (cx * CHUNK_SIZE + lx) as i32;
                    let y = (cy * CHUNK_SIZE + ly) as i32;
                    let height = heights.get(x, y);
                    match chunk.terrain.get(idx) {
                        TileType::ForestGrass | TileType::Moss
                            if height < TREE_LINE =>
                        {
                            let r = rng.random::<f32>();
                            let object = if r < 0.18 {
                                Some(WorldObject::tree())
                            } else if r < 0.22 {
                                Some(WorldObject::apple_tree_with_fruit(
                                    rng.random_range(4..=10),
                                ))
                            } else if r < 0.25 {
                                Some(WorldObject::bush())
                            } else if r < 0.253 {
                                Some(WorldObject::rock(5 + rng.random_range(0..10)))
                            } else {
                                None
                            };
                            if let Some(object) = object {
                                chunk.objects.insert(idx as u8, object);
                            }
                        }
                        TileType::Grass if height < TREE_LINE => {
                            if rng.random::<f32>() < 0.03 {
                                chunk.objects.insert(idx as u8, WorldObject::bush());
                            }
                        }
                        TileType::Stone | TileType::Mountain | TileType::Gravel
                            if rng.random::<f32>() < 0.04 =>
                        {
                            chunk.objects.insert(
                                idx as u8,
                                WorldObject::rock(5 + rng.random_range(0..10)),
                            );
                        }
                        _ => {}
                    }

                    // Ресурсы: медь на камне, глина на низкой земле/траве.
                    if chunk.terrain.get(idx) == TileType::Stone
                        && rng.random::<f32>() < 0.10
                    {
                        chunk.resources.insert(
                            idx as u8,
                            ResourceDeposit {
                                kind: ResourceKind::Copper,
                                amount: 20 + rng.random_range(0..80),
                            },
                        );
                    } else if matches!(
                        chunk.terrain.get(idx),
                        TileType::Grass | TileType::Dirt
                    ) && height < TREE_LINE
                        && rng.random::<f32>() < 0.02
                    {
                        chunk.terrain.set(idx, TileType::Clay);
                        chunk.resources.insert(
                            idx as u8,
                            ResourceDeposit {
                                kind: ResourceKind::Clay,
                                amount: 30 + rng.random_range(0..50),
                            },
                        );
                    }
                }
            }
        }
    }
    true
}

/// Плотность заселения: одно существо на столько тайлов площади.
const TILES_PER_ANIMAL: u32 = 8192;
const MAX_ANIMALS: u32 = 64;

fn spawn_wildlife(
    world: &mut World,
    rng: &mut StdRng,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let tiles = world.size.tiles();
    let count = (tiles * tiles / TILES_PER_ANIMAL).clamp(1, MAX_ANIMALS);
    let side = tiles as i32;
    let mut placed = 0_u32;
    let mut attempts = 0_u32;

    while placed < count && attempts < count * 100 {
        if cancelled() {
            return false;
        }
        attempts += 1;
        let pos = TilePos {
            x: rng.random_range(0..side),
            y: rng.random_range(0..side),
        };
        if world.terrain_kind(pos).is_water() {
            continue;
        }
        world
            .entities
            .spawn(EntityKind::Wolf, WorldPos::of_tile_center(pos));
        placed += 1;
    }
    true
}

/// Плотность кроликов: одно существо на столько тайлов площади.
const TILES_PER_RABBIT: u32 = 4096;
const MAX_RABBITS: u32 = 128;

fn spawn_rabbits(
    world: &mut World,
    rng: &mut StdRng,
    cancelled: &dyn Fn() -> bool,
) -> bool {
    let tiles = world.size.tiles();
    let count = (tiles * tiles / TILES_PER_RABBIT).clamp(1, MAX_RABBITS);
    let side = tiles as i32;
    let mut placed = 0_u32;
    let mut attempts = 0_u32;

    while placed < count && attempts < count * 100 {
        if cancelled() {
            return false;
        }
        attempts += 1;
        let pos = TilePos {
            x: rng.random_range(0..side),
            y: rng.random_range(0..side),
        };
        if !matches!(
            world.terrain_kind(pos),
            TileType::Grass | TileType::ForestGrass
        ) {
            continue;
        }
        world
            .entities
            .spawn(EntityKind::Rabbit, WorldPos::of_tile_center(pos));
        placed += 1;
    }
    true
}
