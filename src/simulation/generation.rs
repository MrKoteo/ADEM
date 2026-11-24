//! Процедурная генерация карты: Heightmap — источник истины.
//!
//! Схема:
//! 1. Базовое поле высот: архетип карты из seed, domain warp, континенты,
//!    гребневый шум гор и спад к краям. Всё ниже уровня моря сразу получает
//!    0 — океан/море плоские без постобработки.
//! 2. Сглаживание и термальная эрозия: плавные склоны, пологие берега.
//! 3. Гидрология: бессточное заполнение депрессий, водосборы, реки, озёра.
//!    BFS-выравнивание осталось только у озёр (у моря высота уже 0).
//! 4. Классификация: тип тайла выводится из итоговой высоты, климата
//!    (температура и влажность — постоянные карты 2×2) и воды.
//! 5. Объекты, ресурсы и дикая природа — отдельными RNG-потоками.
//!
//! `HeightField` — временный буфер генерации (1 Б/тайл); в чанки он
//! записывается один раз в конце и освобождается до установки мира.

pub(crate) mod height;
mod coast;
mod hydrology;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng}; // Bevy 0.20 / rand 0.10: RngExt вместо Rng

use super::chunk::{ResourceDeposit, TerrainTiles, CHUNK_SIZE, CHUNK_TILES};
use super::climate::CLIMATE_CELL_TILES;
use super::entities::EntityKind;
use super::heightmap::{Heightmap, HEIGHT_MAX};
use super::objects::{CHERRY_BUSH_CHANCE, WorldObject};
use super::tile::{ResourceKind, TileType};
use super::world::{TilePos, World, WorldPos, WorldSize};

use coast::{
    apply_shore_ramp, apply_water_depth, limit_soil_slope, raise_land_above_water, CoastField,
};
use height::{
    erode_thermal, fill_base_heights, smooth_heights, HeightField, MapArchetype, BEACH_BAND_SUB,
    HEIGHT_SUB, MOUNTAIN_HEIGHT, STONE_HEIGHT, TREE_LINE_SUB,
};
use hydrology::{build_hydrology, WaterKind};
use super::noise::{fbm, smoothstep};

#[allow(unused_imports)] // используется тестами через `generation::*`
pub(crate) use height::{elevation_at, height_for_elevation};

/// Масштаб шума мха, тайлов: мелкий масштаб даёт пятна-области, а не крап.
const MOSS_SCALE: f32 = 20.0;
/// Порог мшистости: доля лесной травы, зарастающей мхом.
/// Меньше - больше мха.
const MOSS_THRESHOLD: f32 = 0.64;
/// Максимальная ширина пляжа от воды, тайлов.
const BEACH_MAX_DIST: u8 = 2;

/// Сгенерировать мир: рельеф + вода + ресурсы + объекты + существа.
#[allow(dead_code)] // синхронный вариант для тестов; игра идёт через cancellable
pub fn generate_world(size: WorldSize, seed: u64) -> World {
    generate_world_cancellable(size, seed, &|| false, &|_| {})
        .expect("generation without cancellation always completes")
}

/// Доли прогресса этапов генерации (аргумент колбэка — 0..=1). Значения
/// подобраны по замерам: долгие проходы — базовые высоты, гидрология и
/// береговая карта; остальные двигают планку мелкими шагами.
const STAGE_HEIGHTS: f32 = 0.25;
const STAGE_SMOOTH: f32 = 0.30;
const STAGE_ERODE: f32 = 0.34;
const STAGE_HYDROLOGY: f32 = 0.62;
const STAGE_COAST: f32 = 0.72;
const STAGE_SHORE_RAMP: f32 = 0.76;
const STAGE_SLOPE: f32 = 0.79;
const STAGE_RAISE: f32 = 0.81;
const STAGE_CLASSIFY: f32 = 0.92;
const STAGE_CONTENT: f32 = 0.96;
const STAGE_WILDLIFE: f32 = 0.98;

/// Генерация мира с проверкой отмены на каждом проходе и отчётом о прогрессе
/// (`progress` получает долю 0..=1 — для прогресс-бара в UI).
pub fn generate_world_cancellable(
    size: WorldSize,
    seed: u64,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32),
) -> Option<World> {
    progress(0.0);
    let mut world = World::new(size, seed);
    let n = size.tiles() as i32;
    let archetype = MapArchetype::for_seed(seed);

    let mut heights = HeightField::new(n as usize);
    // Глубина ниже уровня моря по исходной высоте: только для поиска моря.
    let mut sea_depth = vec![0_u8; (n * n) as usize];
    if !fill_base_heights(
        &mut heights,
        &mut sea_depth,
        n,
        seed,
        archetype,
        cancelled,
        &|fraction| progress(STAGE_HEIGHTS * fraction),
    ) {
        return None;
    }
    progress(STAGE_HEIGHTS);
    // Плавность до квантования: глобальное сглаживание и эрозия в u16.
    if !smooth_heights(&mut heights, &sea_depth, n, cancelled) {
        return None;
    }
    progress(STAGE_SMOOTH);
    if !erode_thermal(&mut heights, &sea_depth, n, cancelled) {
        return None;
    }
    progress(STAGE_ERODE);
    let mut water = build_hydrology(&mut heights, &sea_depth, n, cancelled)?;
    progress(STAGE_HYDROLOGY);
    // Единая карта берега: расстояние до воды/суши, уровень и вид ближайшей
    // воды. Используется глубиной, пляжами, влажностью и береговым пандусом.
    let coast = CoastField::build(&heights, &water, n, cancelled)?;
    progress(STAGE_COAST);
    apply_water_depth(&mut water, &coast);
    if !apply_shore_ramp(&mut heights, &coast, n, cancelled) {
        return None;
    }
    progress(STAGE_SHORE_RAMP);
    if !limit_soil_slope(&mut heights, &water, n, cancelled) {
        return None;
    }
    progress(STAGE_SLOPE);
    if !raise_land_above_water(&mut heights, &water, n, cancelled) {
        return None;
    }
    progress(STAGE_RAISE);

    // Классификация — построчный обход чанков: прогресс внутри этапа, чтобы
    // планка не замирала на самом долгом после гидрологии проходе.
    let span = STAGE_CLASSIFY - STAGE_RAISE;
    let classify_progress = |fraction: f32| progress(STAGE_RAISE + span * fraction);
    if !classify_and_write(
        &mut world,
        &heights,
        &water,
        &coast,
        seed,
        cancelled,
        &classify_progress,
    ) {
        return None;
    }
    progress(STAGE_CLASSIFY);
    if !place_content(&mut world, &heights, seed, cancelled) {
        return None;
    }
    progress(STAGE_CONTENT);

    let mut rng = StdRng::seed_from_u64(seed ^ 0x5EED_AAAA_BBBB_CCCC);
    if !spawn_wildlife(&mut world, &mut rng, cancelled) || !spawn_rabbits(&mut world, &mut rng, cancelled) {
        return None;
    }
    progress(STAGE_WILDLIFE);

    // Однородные чанки (океаны, плато) схлопываем в Uniform — экономия ОЗУ.
    let chunks_per_side = world.chunks_per_side as usize;
    for (index, chunk) in world.chunks.iter_mut().enumerate() {
        if index % chunks_per_side == 0 && cancelled() {
            return None;
        }
        chunk.terrain.try_compress();
        chunk.heights.try_compress();
        if index % chunks_per_side == 0 {
            progress(
                STAGE_WILDLIFE + (1.0 - STAGE_WILDLIFE) * (index / chunks_per_side) as f32 / chunks_per_side as f32,
            );
        }
    }
    // Чанки писались напрямую — индекс объектов строится после генерации.
    world.rebuild_object_index();
    progress(1.0);

    Some(world)
}

/// Классифицирует тайлы и записывает рельеф/высоты в чанки.
fn classify_and_write(
    world: &mut World,
    heights: &HeightField,
    water: &[WaterKind],
    coast: &CoastField,
    seed: u64,
    cancelled: &dyn Fn() -> bool,
    progress: &dyn Fn(f32),
) -> bool {
    let n = world.size.tiles() as i32;
    let width = n as usize;
    let chunks_per_side = world.chunks_per_side as usize;
    let Some(beach) = build_beaches(heights, water, coast, n, cancelled) else {
        return false;
    };
    // Прибрежная добавка влажности — по ячейкам 2×2, как и карта влажности.
    let cell = CLIMATE_CELL_TILES as usize;
    let side = (n as usize).div_ceil(cell);
    let mut coastal = vec![0.0_f32; side * side];
    for cy in 0..side {
        for cx in 0..side {
            let x = (cx * cell + cell / 2).min(width - 1);
            let y = (cy * cell + cell / 2).min(width - 1);
            let distance = coast.land_distance(y * width + x) as f32;
            coastal[cy * side + cx] = 0.18 * (1.0 - smoothstep(distance / 12.0));
        }
    }
    let moss = build_moss_map(n, seed);
    let climate = &world.climate;
    let moisture_map = &world.moisture;

    for cy in 0..chunks_per_side {
        if cy % 8 == 0 && cancelled() {
            return false;
        }
        if cy % 8 == 0 {
            progress(cy as f32 / chunks_per_side as f32);
        }
        for cx in 0..chunks_per_side {
            let chunk_index = cy * chunks_per_side + cx;
            let base_x = (cx * CHUNK_SIZE) as i32;
            let base_y = (cy * CHUNK_SIZE) as i32;

            let mut tiles = [TileType::Empty; CHUNK_TILES];
            let mut tile_heights = [0_u8; CHUNK_TILES];
            for ly in 0..CHUNK_SIZE {
                let y = base_y + ly as i32;
                for lx in 0..CHUNK_SIZE {
                    let x = base_x + lx as i32;
                    let index = y as usize * width + x as usize;
                    let local = ly * CHUNK_SIZE + lx;
                    let height_sub = heights.get(x, y);
                    let height = quantize_height(height_sub);
                    let annual_temperature = climate.annual_temperature_c(x, y, height);
                    let bonus = coastal[(y as usize / cell) * side + (x as usize / cell)];
                    let moisture = (moisture_map.moisture_at(x, y) + bonus
                        - (height as f32 / HEIGHT_MAX as f32) * 0.10)
                        .clamp(0.0, 1.0);
                    let moss = moss.at(x, y);
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

/// Квантует подуровни генерации в итоговый `u8` 0..255.
#[inline]
fn quantize_height(height: u16) -> u8 {
    ((height + HEIGHT_SUB / 2) / HEIGHT_SUB).min(HEIGHT_MAX as u16) as u8
}

/// Тип тайла по итоговой высоте, температуре, влажности и пляжу.
fn classify_tile(
    water: WaterKind,
    height: u8,
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

/// Карта мха по ячейкам 2×2: временная, нужна только классификации.
struct MossMap {
    side: usize,
    values: Vec<u8>,
}

impl MossMap {
    fn at(&self, x: i32, y: i32) -> f32 {
        sample_u8_map(&self.values, self.side, x, y) / 255.0
    }
}

/// Строит карту мха в ячейках 2×2 (0.25 Б/тайл временно).
fn build_moss_map(n: i32, seed: u64) -> MossMap {
    let cell = CLIMATE_CELL_TILES as usize;
    let side = (n as usize).div_ceil(cell);
    let mut values = vec![0_u8; side * side];
    for cy in 0..side {
        for cx in 0..side {
            let x = (cx * cell + cell / 2).min(n as usize - 1) as f32;
            let y = (cy * cell + cell / 2).min(n as usize - 1) as f32;
            let value = moss_at(x, y, seed);
            values[cy * side + cx] = (value * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    MossMap { side, values }
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

/// Мшистость тайла: мелкий шум — пятна мха, а не одиночный крап.
fn moss_at(x: f32, y: f32, seed: u64) -> f32 {
    let fx = x / MOSS_SCALE;
    let fy = y / MOSS_SCALE;
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

/// Размечает пляжи по единой береговой карте: суша в пределах
/// `BEACH_MAX_DIST` от воды и в полосе `BEACH_BAND_SUB` над её уровнем.
fn build_beaches(
    heights: &HeightField,
    water: &[WaterKind],
    coast: &CoastField,
    n: i32,
    cancelled: &dyn Fn() -> bool,
) -> Option<Beaches> {
    let width = n as usize;
    let mut beaches = Beaches::new(width);
    for y in 0..n {
        if y % 64 == 0 && cancelled() {
            return None;
        }
        for x in 0..n {
            let index = y as usize * width + x as usize;
            if water[index].is_water() {
                continue;
            }
            let distance = coast.land_distance(index);
            if distance == 0 || distance > BEACH_MAX_DIST {
                continue;
            }
            let level = coast.max_water_level(index);
            let height = heights.get(x, y);
            if height <= level || height > level + BEACH_BAND_SUB {
                continue;
            }
            // Берега рек — гравий, морей и озёр — песок (холод/высота
            // превратят его в гравий на классификации).
            let kind = if coast.max_water_kind(index) == WaterKind::River {
                BeachKind::Gravel
            } else {
                BeachKind::Sand
            };
            mark_beach(&mut beaches, index, kind);
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
                            if height < TREE_LINE_SUB =>
                        {
                            let r = rng.random::<f32>();
                            let object = if r < 0.18 {
                                Some(WorldObject::tree())
                            } else if r < 0.22 {
                                Some(WorldObject::apple_tree_with_fruit(
                                    rng.random_range(4..=10),
                                ))
                            } else if r < 0.25 {
                                Some(if rng.random::<f32>() < CHERRY_BUSH_CHANCE {
                                    WorldObject::cherry_bush_with_fruit(rng.random_range(6..=15))
                                } else {
                                    WorldObject::bush()
                                })
                            } else if r < 0.253 {
                                Some(WorldObject::rock(5 + rng.random_range(0..10)))
                            } else {
                                None
                            };
                            if let Some(object) = object {
                                chunk.objects.insert(idx as u8, object);
                            }
                        }
                        TileType::Grass if height < TREE_LINE_SUB => {
                            if rng.random::<f32>() < 0.03 {
                                let object = if rng.random::<f32>() < CHERRY_BUSH_CHANCE {
                                    WorldObject::cherry_bush_with_fruit(rng.random_range(6..=15))
                                } else {
                                    WorldObject::bush()
                                };
                                chunk.objects.insert(idx as u8, object);
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
                    ) && height < TREE_LINE_SUB
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
