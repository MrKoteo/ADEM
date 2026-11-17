//! Тесты процедурной генерации: детерминизм, биомы, глубины воды, реки,
//! посев объектов и заселение существ.

use crate::simulation::generation::{generate_world, generate_world_cancellable};
use crate::simulation::objects::{ObjectKind, WorldObject};
use crate::simulation::scheduler::Scheduler;
use crate::simulation::tile::TileType;
use crate::simulation::vegetation::seed_all_cancellable;
use crate::simulation::world::{TilePos, World, WorldSize};

/// Отменяемая генерация уважает флаг с первого же прохода: поток не
/// достраивает карту, а сразу возвращает None. Синхронная обёртка без
/// отмены даёт тот же мир, что и раньше.
#[test]
fn generation_stops_when_cancelled() {
    assert!(generate_world_cancellable(WorldSize::new(256), 42, &|| true).is_none());

    let world = generate_world(WorldSize::new(256), 42);
    assert!(world.size.tiles() == 256);
    // Посев цепочек тоже отменяем: частичный планировщик использовать нельзя.
    let mut scheduler = Scheduler::default();
    assert!(!seed_all_cancellable(&world, &mut scheduler, 100, &|| true));
    let mut scheduler = Scheduler::default();
    assert!(seed_all_cancellable(&world, &mut scheduler, 100, &|| false));
}

/// Считает объекты мира по видам. Обход HashMap не детерминирован,
/// поэтому миры сравниваются по счётчикам, а не по порядку объектов.
fn count_objects(world: &World) -> [u32; ObjectKind::ALL.len()] {
    let mut counts = [0_u32; ObjectKind::ALL.len()];
    for chunk in &world.chunks {
        for obj in chunk.objects.values() {
            counts[obj.kind.discriminant() as usize] += 1;
        }
    }
    counts
}

// Генерация с одинаковым seed детерминирована по тайлам и объектам.
//#[test]

#[test]
#[ignore]
fn water_body_stats() {
    for seed in [42_u64, 999, 7] {
        let world = generate_world(WorldSize::new(256), seed);
        let n = world.size.tiles() as i32;
        let mut visited = vec![false; (n * n) as usize];
        let mut queue = std::collections::VecDeque::new();
        let mut bodies = Vec::new();
        for y in 0..n {
            for x in 0..n {
                let index = (y * n + x) as usize;
                if visited[index] {
                    continue;
                }
                let kind = world.terrain_kind(TilePos { x, y });
                if !kind.is_water() || kind == TileType::River {
                    continue;
                }
                visited[index] = true;
                queue.push_back((x, y));
                let mut size = 0;
                let mut min_h = u8::MAX;
                let mut max_h = 0;
                let mut edge = false;
                let mut has_ocean = false;
                while let Some((cx, cy)) = queue.pop_front() {
                    size += 1;
                    if cx == 0 || cy == 0 || cx + 1 == n || cy + 1 == n {
                        edge = true;
                    }
                    let h = world.height(TilePos { x: cx, y: cy }).unwrap_or(0);
                    min_h = min_h.min(h);
                    max_h = max_h.max(h);
                    if matches!(
                        world.terrain_kind(TilePos { x: cx, y: cy }),
                        TileType::Ocean | TileType::DeepWater
                    ) {
                        has_ocean = true;
                    }
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            let nx = cx + dx;
                            let ny = cy + dy;
                            if nx < 0 || ny < 0 || nx >= n || ny >= n {
                                continue;
                            }
                            let ni = (ny * n + nx) as usize;
                            if visited[ni] {
                                continue;
                            }
                            let nk = world.terrain_kind(TilePos { x: nx, y: ny });
                            if nk.is_water() && nk != TileType::River {
                                visited[ni] = true;
                                queue.push_back((nx, ny));
                            }
                        }
                    }
                }
                if size >= 50 {
                    bodies.push((size, min_h, max_h, edge, has_ocean));
                }
            }
        }
        bodies.sort_by_key(|b| std::cmp::Reverse(b.0));
        println!("seed {seed}: {} bodies", bodies.len());
        for (size, min_h, max_h, edge, ocean) in bodies.iter().take(12) {
            println!(
                "  size={size} min={min_h} max={max_h} edge={edge} ocean={ocean}"
            );
        }
    }
}

/// Ручная статистика состава карты для тюнинга биомов/гор/воды.
/// Запуск: `cargo test generation_stats -- --ignored --nocapture`.
#[test]
#[ignore]
fn generation_stats() {
    for seed in [42_u64, 999, 7] {
        let world = generate_world(WorldSize::new(256), seed);
        let archetype = crate::simulation::generation::height::MapArchetype::for_seed(seed);
        let n = world.size.tiles() as i32;
        let mut counts = std::collections::BTreeMap::new();
        let mut water = 0_u32;
        let mut land = 0_u32;
        let mut max_height = 0_u8;
        for y in 0..n {
            for x in 0..n {
                let kind = world.terrain_kind(TilePos { x, y });
                *counts.entry(format!("{kind:?}")).or_insert(0_u32) += 1;
                if kind.is_water() {
                    water += 1;
                } else {
                    land += 1;
                }
                max_height = max_height.max(world.height(TilePos { x, y }).unwrap_or(0));
            }
        }
        println!(
            "seed {seed} ({archetype:?}): water={water} land={land} max_height={max_height}"
        );
        for (kind, count) in counts {
            println!("  {kind}: {count}");
        }
    }
}

/// Ручной замер генерации и памяти мира. Запуск:
/// `cargo test generation_benchmark -- --ignored --nocapture`.
/// Не входит в обычный прогон: на 4096² занимает секунды.
#[test]
#[ignore]
fn generation_benchmark() {
    for side in [256_u32, 1024, 2048, 4096] {
        let start = std::time::Instant::now();
        let world = generate_world(WorldSize::new(side), 12345);
        println!(
            "{side}x{side}: {:?}, world_mem={} bytes",
            start.elapsed(),
            world.memory_bytes()
        );
    }
}

/// Временный визуальный контроль генерации: PNG по типам тайлов с тинтом
/// по высоте. Запуск: `cargo test generation_preview -- --ignored`.
#[test]
#[ignore]
fn generation_preview() {
    use image::{ImageBuffer, Rgb};

    for seed in [0_u64, 7, 9, 42] {
        let world = generate_world(WorldSize::new(512), seed);
        let n = world.size.tiles() as u32;
        let mut img = ImageBuffer::from_pixel(n, n, Rgb([0_u8, 0, 0]));
        for y in 0..n {
            for x in 0..n {
                let pos = TilePos { x: x as i32, y: y as i32 };
                let kind = world.terrain_kind(pos);
                let height = world.height(pos).unwrap_or(0);
                let base: [f32; 3] = match kind {
                    TileType::Water => [0.35, 0.60, 0.85],
                    TileType::DeepWater => [0.16, 0.36, 0.66],
                    TileType::Ocean => [0.06, 0.18, 0.42],
                    TileType::River => [0.30, 0.70, 0.90],
                    TileType::Sand => [0.86, 0.82, 0.55],
                    TileType::Gravel => [0.62, 0.62, 0.62],
                    TileType::Grass => [0.36, 0.67, 0.28],
                    TileType::ForestGrass => [0.16, 0.47, 0.20],
                    TileType::Moss => [0.24, 0.51, 0.24],
                    TileType::Dirt => [0.55, 0.41, 0.27],
                    TileType::Clay => [0.67, 0.55, 0.43],
                    TileType::Stone => [0.51, 0.51, 0.55],
                    TileType::Mountain => [0.67, 0.67, 0.71],
                    _ => [0.0, 0.0, 0.0],
                };
                let water = kind.is_water();
                let shade = if water || height == 0 {
                    1.0
                } else {
                    0.55 + 0.45 * (height as f32 / 48.0).min(1.0)
                };
                img.put_pixel(
                    x,
                    y,
                    Rgb([
                        (base[0] * shade * 255.0) as u8,
                        (base[1] * shade * 255.0) as u8,
                        (base[2] * shade * 255.0) as u8,
                    ]),
                );
            }
        }
        img.save(format!("target/preview_{seed}.png"))
            .expect("save preview");
    }
}
/// Вода не должна «висеть» над сушей, а река — иметь локальные ямы.
#[test]
fn no_hanging_water_or_river_pits() {
    for seed in [0_u64, 7, 9, 42, 999] {
        let world = generate_world(WorldSize::new(512), seed);
        let n = world.size.tiles() as i32;
        for y in 0..n {
            for x in 0..n {
                let pos = TilePos { x, y };
                let kind = world.terrain_kind(pos);
                if !kind.is_water() {
                    continue;
                }
                let height = world.height(pos).expect("тайл внутри мира");
                let mut river_neighbors = 0_u32;
                let mut river_min = u8::MAX;
                let mut touches_other_water = false;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx < 0 || ny < 0 || nx >= n || ny >= n {
                            continue;
                        }
                        let npos = TilePos { x: nx, y: ny };
                        let nkind = world.terrain_kind(npos);
                        let nheight = world.height(npos).expect("тайл внутри мира");
                        if nkind.is_water() {
                            if kind == TileType::River && nkind == TileType::River {
                                river_neighbors += 1;
                                river_min = river_min.min(nheight);
                            }
                            if nkind != kind {
                                touches_other_water = true;
                            }
                        } else {
                            assert!(
                                height <= nheight,
                                "seed {seed}: вода висит над сушей в ({x}, {y}): {height} > {nheight} ({nkind:?})"
                            );
                        }
                    }
                }
                // Локальная яма: у реки все речные соседи выше, и рядом нет
                // другого водоёма (устье/исток — допустимые исключения).
                if kind == TileType::River
                    && !touches_other_water
                    && river_neighbors > 0
                    && river_min > height
                {
                    panic!("seed {seed}: яма в реке в ({x}, {y}): {height}");
                }
            }
        }
    }
}

/// Стоячая вода согласована: компонента, касающаяся края карты, — море на
/// уровне 0; внутренняя — озеро на одном уровне.
#[test]
fn standing_water_components_are_consistent() {
    use std::collections::{HashSet, VecDeque};

    for seed in [0_u64, 7, 9, 42, 999] {
        let world = generate_world(WorldSize::new(512), seed);
        let n = world.size.tiles() as i32;
        let mut seen: HashSet<(i32, i32)> = HashSet::new();
        for sy in 0..n {
            for sx in 0..n {
                let start = TilePos { x: sx, y: sy };
                let start_kind = world.terrain_kind(start);
                if !start_kind.is_water() || start_kind == TileType::River || !seen.insert((sx, sy))
                {
                    continue;
                }
                let mut queue = VecDeque::new();
                queue.push_back((sx, sy));
                let mut touches_edge = false;
                let mut heights = Vec::new();
                while let Some((x, y)) = queue.pop_front() {
                    if x == 0 || y == 0 || x + 1 == n || y + 1 == n {
                        touches_edge = true;
                    }
                    let pos = TilePos { x, y };
                    heights.push(world.height(pos).unwrap_or(0));
                    for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx < 0 || ny < 0 || nx >= n || ny >= n {
                            continue;
                        }
                        let kind = world.terrain_kind(TilePos { x: nx, y: ny });
                        if kind.is_water() && kind != TileType::River && seen.insert((nx, ny)) {
                            queue.push_back((nx, ny));
                        }
                    }
                }
                let level = heights[0];
                assert!(
                    heights.iter().all(|&h| h == level),
                    "seed {seed}: стоячая вода не плоская в ({sx}, {sy})"
                );
                if touches_edge {
                    assert_eq!(level, 0, "seed {seed}: вода у края не на уровне моря");
                }
            }
        }
    }
}

/// Ручная проверка профиля высот и берегов: максимум, доля рек, крутые
/// соседние перепады у воды. Запуск:
/// `cargo test generation_height_profile -- --ignored --nocapture`.
#[test]
#[ignore]
fn generation_height_profile() {
    use crate::simulation::generation::height::{MOUNTAIN_HEIGHT, STONE_HEIGHT};

    for seed in 1..=12_u64 {
        let world = generate_world(WorldSize::new(256), seed);
        let n = world.size.tiles() as i32;
        let mut max_height = 0_u8;
        let mut river = 0_u32;
        let mut stone = 0_u32;
        let mut mountain = 0_u32;
        let mut water = 0_u32;
        let mut shore_steps = 0_u32;
        let mut shore_cliffs = 0_u32;
        let mut max_shore_delta = 0_u8;
        for y in 0..n {
            for x in 0..n {
                let pos = TilePos { x, y };
                let kind = world.terrain_kind(pos);
                let height = world.height(pos).unwrap_or(0);
                max_height = max_height.max(height);
                if kind == TileType::River {
                    river += 1;
                }
                if kind.is_water() {
                    water += 1;
                }
                if kind == TileType::Stone {
                    stone += 1;
                }
                if kind == TileType::Mountain {
                    mountain += 1;
                }
                // Берега: суша ниже камня рядом с водой. Обрыв — перепад > 2.
                if !kind.is_water() && height < STONE_HEIGHT {
                    let near_water = [(0, -1), (1, 0), (0, 1), (-1, 0)]
                        .iter()
                        .any(|&(dx, dy)| {
                            let nx = x + dx;
                            let ny = y + dy;
                            nx >= 0
                                && ny >= 0
                                && nx < n
                                && ny < n
                                && world
                                    .terrain_kind(TilePos { x: nx, y: ny })
                                    .is_water()
                        });
                    if near_water {
                        for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                            let nx = x + dx;
                            let ny = y + dy;
                            if nx < 0 || ny < 0 || nx >= n || ny >= n {
                                continue;
                            }
                            if !world.terrain_kind(TilePos { x: nx, y: ny }).is_water() {
                                let other = world.height(TilePos { x: nx, y: ny }).unwrap_or(0);
                                // Обрыв допустим только у камня: сравниваем
                                // только почву ниже порога камня.
                                if other >= STONE_HEIGHT {
                                    continue;
                                }
                                let delta = height.abs_diff(other);
                                shore_steps += 1;
                                max_shore_delta = max_shore_delta.max(delta);
                                if delta > 2 {
                                    shore_cliffs += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        println!(
            "seed {seed}: max={max_height} water={water} river={river} stone={stone} mountain={mountain} shore_steps={shore_steps} cliffs={shore_cliffs} max_shore_delta={max_shore_delta} stone_at={STONE_HEIGHT} mountain_at={MOUNTAIN_HEIGHT}"
        );
    }
}

/// Типы тайлов не противоречат итоговой высоте: трава не бывает выше
/// камня, камень — выше гор, а пляжи стоят рядом с водой.
#[test]
fn terrain_types_respect_final_height_and_water() {
    use crate::simulation::generation::height::{MOUNTAIN_HEIGHT, STONE_HEIGHT};

    let world = generate_world(WorldSize::new(256), 42);
    let n = world.size.tiles() as i32;
    let mut sand = 0_u32;
    let mut gravel = 0_u32;
    for y in 0..n {
        for x in 0..n {
            let pos = TilePos { x, y };
            let kind = world.terrain_kind(pos);
            let height = world.height(pos).expect("тайл внутри мира");
            match kind {
                TileType::Grass
                | TileType::ForestGrass
                | TileType::Moss
                | TileType::Dirt
                | TileType::Clay => {
                    assert!(
                        height < STONE_HEIGHT,
                        "трава/земля выше камня в ({x}, {y}): {height}"
                    );
                }
                TileType::Stone => {
                    assert!(height >= STONE_HEIGHT, "камень ниже порога: {height}")
                }
                TileType::Mountain => {
                    assert!(height >= MOUNTAIN_HEIGHT, "гора ниже порога: {height}")
                }
                TileType::Sand => sand += 1,
                TileType::Gravel => gravel += 1,
                _ => {}
            }
            if matches!(kind, TileType::Sand | TileType::Gravel) {
                // Пляж не дальше двух тайлов от воды/льда.
                let mut near_water = false;
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx < 0 || ny < 0 || nx >= n || ny >= n {
                            continue;
                        }
                        if world.terrain_kind(TilePos { x: nx, y: ny }).is_water() {
                            near_water = true;
                        }
                    }
                }
                assert!(near_water, "пляж без воды рядом в ({x}, {y})");
            }
        }
    }
    assert!(sand > 0, "нет песчаных пляжей");
    assert!(gravel > 0, "нет гравийных берегов");
}

/// Климат больше не квантуется по чанкам: в одном чанке могут соседствовать
/// трава и лесная трава (раньше весь чанк получал один биом).
#[test]
fn biomes_are_not_quantized_by_chunk() {
    let world = generate_world(WorldSize::new(256), 999);
    let mut mixed_chunks = 0;
    for chunk in &world.chunks {
        let mut grass = 0;
        let mut forest = 0;
        for idx in 0..crate::simulation::CHUNK_TILES {
            match chunk.terrain.get(idx) {
                TileType::Grass => grass += 1,
                TileType::ForestGrass => forest += 1,
                _ => {}
            }
        }
        if grass > 0 && forest > 0 {
            mixed_chunks += 1;
        }
    }
    assert!(
        mixed_chunks > 0,
        "биомы всё ещё квантуются по чанкам: нет смешанных чанков"
    );
}

/// Генерация больше не ставит лёд вместо воды.
#[test]
fn generation_does_not_place_ice() {
    for seed in [42_u64, 999, 7] {
        let world = generate_world(WorldSize::new(256), seed);
        let n = world.size.tiles() as i32;
        for y in 0..n {
            for x in 0..n {
                assert_ne!(
                    world.terrain_kind(TilePos { x, y }),
                    TileType::Ice,
                    "seed {seed}: лёд в ({x}, {y})"
                );
            }
        }
    }
}

/// Вся вода, кроме рек, плоская в пределах водоёма: у каждого озера/моря
/// своя высота поверхности, но внутри — ни ям, ни ступенек.
#[test]
fn water_bodies_are_flat_except_rivers() {
    let world = generate_world(WorldSize::new(256), 999);
    let n = world.size.tiles() as i32;
    let mut visited = vec![false; (n * n) as usize];
    let mut queue = std::collections::VecDeque::new();
    let mut bodies = 0_u32;
    for y in 0..n {
        for x in 0..n {
            let index = (y * n + x) as usize;
            if visited[index] {
                continue;
            }
            let kind = world.terrain_kind(TilePos { x, y });
            if !kind.is_water() || kind == TileType::River {
                continue;
            }
            bodies += 1;
            visited[index] = true;
            queue.push_back((x, y));
            let mut level = None;
            while let Some((cx, cy)) = queue.pop_front() {
                let h = world.height(TilePos { x: cx, y: cy }).unwrap_or(0);
                match level {
                    None => level = Some(h),
                    Some(expected) => assert_eq!(
                        h, expected,
                        "вода не плоская: {h} вместо {expected} в ({cx}, {cy})"
                    ),
                }
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = cx + dx;
                        let ny = cy + dy;
                        if nx < 0 || ny < 0 || nx >= n || ny >= n {
                            continue;
                        }
                        let ni = (ny * n + nx) as usize;
                        if visited[ni] {
                            continue;
                        }
                        let nk = world.terrain_kind(TilePos { x: nx, y: ny });
                        if nk.is_water() && nk != TileType::River {
                            visited[ni] = true;
                            queue.push_back((nx, ny));
                        }
                    }
                }
            }
        }
    }
    assert!(bodies > 0, "на карте нет водоёмов");
}

/// Поверхность водоёма не выше соседней суши: вода не «висит» над берегом.
#[test]
fn water_surface_is_not_above_banks() {
    let world = generate_world(WorldSize::new(256), 999);
    let n = world.size.tiles() as i32;
    for y in 0..n {
        for x in 0..n {
            let kind = world.terrain_kind(TilePos { x, y });
            if !kind.is_water() || kind == TileType::River {
                continue;
            }
            let water = world.height(TilePos { x, y }).unwrap_or(0);
            for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                let nx = x + dx;
                let ny = y + dy;
                if nx < 0 || ny < 0 || nx >= n || ny >= n {
                    continue;
                }
                let nk = world.terrain_kind(TilePos { x: nx, y: ny });
                if nk.is_water() {
                    continue;
                }
                let bank = world.height(TilePos { x: nx, y: ny }).unwrap_or(0);
                assert!(
                    water <= bank,
                    "вода выше берега в ({x}, {y}): {water} > {bank}"
                );
            }
        }
    }
}

/// Вода, связанная с краем карты, — море и всегда на уровне океана.
#[test]
fn edge_connected_water_is_at_sea_level() {
    use crate::simulation::SEA_LEVEL;

    let world = generate_world(WorldSize::new(256), 999);
    let n = world.size.tiles() as i32;
    let mut visited = vec![false; (n * n) as usize];
    let mut queue = std::collections::VecDeque::new();
    for i in 0..n {
        for (x, y) in [(i, 0), (i, n - 1), (0, i), (n - 1, i)] {
            let kind = world.terrain_kind(TilePos { x, y });
            if !kind.is_water() || kind == TileType::River {
                continue;
            }
            let index = (y * n + x) as usize;
            if !visited[index] {
                visited[index] = true;
                queue.push_back((x, y));
            }
        }
    }
    while let Some((cx, cy)) = queue.pop_front() {
        assert_eq!(
            world.height(TilePos { x: cx, y: cy }),
            Some(SEA_LEVEL),
            "связанная с краем вода не на уровне океана в ({cx}, {cy})"
        );
        for dy in -1..=1 {
            for dx in -1..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let nx = cx + dx;
                let ny = cy + dy;
                if nx < 0 || ny < 0 || nx >= n || ny >= n {
                    continue;
                }
                let ni = (ny * n + nx) as usize;
                if visited[ni] {
                    continue;
                }
                let kind = world.terrain_kind(TilePos { x: nx, y: ny });
                if kind.is_water() && kind != TileType::River {
                    visited[ni] = true;
                    queue.push_back((nx, ny));
                }
            }
        }
    }
}

/// Реки — отдельный тип тайла, а не мелководье.
#[test]
fn generation_creates_river_tiles() {
    let world = generate_world(WorldSize::new(256), 999);
    let mut rivers = 0_u32;
    for y in 0..world.size.tiles() as i32 {
        for x in 0..world.size.tiles() as i32 {
            if world.terrain_kind(TilePos { x, y }) == TileType::River {
                rivers += 1;
            }
        }
    }
    assert!(rivers > 0, "реки не помечены TileType::River");
    assert!(TileType::River.is_water());
}

/// Устье реки совпадает с уровнем водоёма: на стыке нет ступеньки.
#[test]
fn river_mouths_match_water_level() {
    let world = generate_world(WorldSize::new(256), 999);
    let n = world.size.tiles() as i32;
    for y in 0..n {
        for x in 0..n {
            if world.terrain_kind(TilePos { x, y }) != TileType::River {
                continue;
            }
            let mut mouth = None;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= n || ny >= n {
                        continue;
                    }
                    let kind = world.terrain_kind(TilePos { x: nx, y: ny });
                    if kind.is_water() && kind != TileType::River {
                        let h = world.height(TilePos { x: nx, y: ny }).unwrap_or(0);
                        mouth = Some(mouth.map_or(h, |m: u8| m.min(h)));
                    }
                }
            }
            if let Some(mouth) = mouth {
                assert_eq!(
                    world.height(TilePos { x, y }),
                    Some(mouth),
                    "ступенька в устье реки ({x}, {y})"
                );
            }
        }
    }
}

/// Температура локальная: высота снижает её, а карта не требует обновления.
#[test]
fn local_temperature_falls_with_height() {
    let mut world = generate_world(WorldSize::new(256), 42);
    let pos = TilePos { x: 128, y: 128 };
    let low = world.local_temperature_c(pos);
    world.set_height(pos, 255);
    let high = world.local_temperature_c(pos);
    assert!(
        high < low - 5.0,
        "высота должна заметно снижать температуру: {low} -> {high}"
    );
}

/// Генерация содержит все важные виды тайлов (не вырождается в один тип).
#[test]
fn generation_has_variety() {
    let w = generate_world(WorldSize::new(256), 999);
    let mut has_water = false;
    let mut has_land = false;
    for y in 0..w.size.tiles() as i32 {
        for x in 0..w.size.tiles() as i32 {
            match w.terrain_kind(TilePos { x, y }) {
                TileType::Water => has_water = true,
                TileType::Grass | TileType::ForestGrass | TileType::Dirt | TileType::Stone => {
                    has_land = true;
                }
                _ => {}
            }
        }
    }
    assert!(has_water, "нет воды на карте");
    assert!(has_land, "нет суши на карте");
    assert!(
        count_objects(&w)[ObjectKind::Tree.discriminant() as usize] > 0,
        "нет деревьев на карте"
    );
}

/// Генерация сеет все четыре вида объектов.
#[test]
fn generation_places_all_object_kinds() {
    let w = generate_world(WorldSize::new(256), 999);
    let counts = count_objects(&w);
    assert!(counts[ObjectKind::Tree.discriminant() as usize] > 0, "нет деревьев");
    assert!(counts[ObjectKind::Bush.discriminant() as usize] > 0, "нет кустов");
    assert!(counts[ObjectKind::Rock.discriminant() as usize] > 0, "нет валунов");
    assert!(counts[ObjectKind::AppleTree.discriminant() as usize] > 0, "нет яблонь");
}

/// Генерация содержит все три глубины воды и горы (не вырождается).
#[test]
fn generation_has_water_depths_and_mountains() {
    let w = generate_world(WorldSize::new(256), 999);
    let mut has_deep = false;
    let mut has_ocean = false;
    let mut has_mountain = false;
    for y in 0..w.size.tiles() as i32 {
        for x in 0..w.size.tiles() as i32 {
            match w.terrain_kind(TilePos { x, y }) {
                TileType::DeepWater => has_deep = true,
                TileType::Ocean => has_ocean = true,
                TileType::Mountain => has_mountain = true,
                _ => {}
            }
        }
    }
    assert!(has_deep, "нет глубокой воды на карте");
    assert!(has_ocean, "нет океана на карте");
    assert!(has_mountain, "нет гор на карте");
}

/// «Океан» встречается только в морях: каждый водный регион с тайлом
/// Ocean соединён с краем карты. Внутренние водоёмы (озёра) глубина
/// понижает, поэтому озеро не может иметь океанической краски.
#[test]
fn ocean_tiles_only_touch_border_water() {
    use std::collections::HashSet;

    let w = generate_world(WorldSize::new(256), 999);
    let n = w.size.tiles() as i32;

    // BFS от водных тайлов периметра: помечаем всё море.
    let mut sea: HashSet<u32> = HashSet::new();
    let mut queue: std::collections::VecDeque<(i32, i32)> = std::collections::VecDeque::new();
    for i in 0..n {
        for (x, y) in [(i, 0), (i, n - 1), (0, i), (n - 1, i)] {
            if w.terrain_kind(TilePos { x, y }).is_water() && sea.insert((y * n + x) as u32) {
                queue.push_back((x, y));
            }
        }
    }
    while let Some((x, y)) = queue.pop_front() {
        for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
            let (nx, ny) = (x + dx, y + dy);
            if nx < 0 || ny < 0 || nx >= n || ny >= n {
                continue;
            }
            if w.terrain_kind(TilePos { x: nx, y: ny }).is_water()
                && sea.insert((ny * n + nx) as u32)
            {
                queue.push_back((nx, ny));
            }
        }
    }
    // Ни один тайл Ocean не остался вне моря.
    for y in 0..n {
        for x in 0..n {
            if w.terrain_kind(TilePos { x, y }) == TileType::Ocean {
                assert!(
                    sea.contains(&((y * n + x) as u32)),
                    "Ocean внутри озера в ({x}, {y})"
                );
            }
        }
    }
}

/// Реки существуют: вода встречается заметно выше уровня моря — такая
/// могла появиться только carve'ом русла через сушу.
#[test]
fn generation_carves_rivers_through_land() {
    use crate::simulation::generation::elevation_at;

    let mut worlds_with_rivers = 0;
    for seed in 1..=8 {
        let w = generate_world(WorldSize::new(256), seed);
        let n = w.size.tiles() as i32;
        let mut has_river = false;
        for y in 0..n {
            for x in 0..n {
                if w.terrain_kind(TilePos { x, y }) == TileType::River
                    && elevation_at(x, y, n, seed) > 0.52
                {
                    has_river = true;
                    break;
                }
            }
            if has_river {
                break;
            }
        }
        if has_river {
            worlds_with_rivers += 1;
        }
    }
    assert!(
        worlds_with_rivers >= 6,
        "реки не генерируются: {} из 8 миров с реками",
        worlds_with_rivers
    );
}

/// На одном тайле не может быть двух объектов: повторная запись перезаписывает,
/// а очистка убирает объект.
#[test]
fn one_object_per_tile() {
    let mut world = World::new(WorldSize::new(256), 1);
    let pos = TilePos { x: 3, y: 4 };
    world.set_terrain(pos, TileType::Grass);
    world.set_object(pos, Some(WorldObject::tree()));
    world.set_object(pos, Some(WorldObject::bush()));
    assert!(matches!(world.object_at(pos), Some(obj) if obj.kind == ObjectKind::Bush));
    world.set_object(pos, None);
    assert!(world.object_at(pos).is_none());
}

/// Реки всегда заканчиваются водоёмом: любой внутренний водный регион
/// либо соединён с краем карты (море), либо это озеро заметного размера —
/// однотайловых обрывков после заливки котловин не остаётся. Связность
/// восьмисоседняя: русла рек идут и по диагонали.
#[test]
fn rivers_always_end_in_a_water_body() {
    use std::collections::{HashSet, VecDeque};

    for seed in [42_u64, 999, 7] {
        let w = generate_world(WorldSize::new(256), seed);
        let n = w.size.tiles() as i32;

        let mut seen: HashSet<u32> = HashSet::new();
        for start_y in 0..n {
            for start_x in 0..n {
                if !w.terrain_kind(TilePos { x: start_x, y: start_y }).is_water()
                    || seen.contains(&((start_y * n + start_x) as u32))
                {
                    continue;
                }
                // Новый водный регион: размер и выход к краю.
                let mut size = 0_usize;
                let mut touches_edge = false;
                let mut queue: VecDeque<(i32, i32)> = VecDeque::new();
                seen.insert((start_y * n + start_x) as u32);
                queue.push_back((start_x, start_y));
                while let Some((x, y)) = queue.pop_front() {
                    size += 1;
                    if x == 0 || y == 0 || x == n - 1 || y == n - 1 {
                        touches_edge = true;
                    }
                    for dx in -1..=1 {
                        for dy in -1..=1 {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            let (nx, ny) = (x + dx, y + dy);
                            if nx < 0 || ny < 0 || nx >= n || ny >= n {
                                continue;
                            }
                            if w.terrain_kind(TilePos { x: nx, y: ny }).is_water()
                                && seen.insert((ny * n + nx) as u32)
                            {
                                queue.push_back((nx, ny));
                            }
                        }
                    }
                }
                assert!(
                    touches_edge || size >= 12,
                    "seed {seed}: внутренний водный регион из {size} тайлов в ({start_x}, {start_y})"
                );
            }
        }
    }
}

/// Генерация заселяет мир волками только на суше.
#[test]
fn generation_spawns_wolves_on_land() {
    use crate::simulation::entities::EntityKind;

    let world = generate_world(WorldSize::new(256), 42);
    let wolves: Vec<_> = world
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Wolf)
        .collect();
    assert!(!wolves.is_empty(), "на карте нет волков");

    for wolf in wolves {
        let tile = TilePos {
            x: wolf.pos.x.floor() as i32,
            y: wolf.pos.y.floor() as i32,
        };
        assert!(
            !world.terrain_kind(tile).is_water(),
            "волк появился в воде/льду: {:?}",
            tile
        );
        assert_eq!(wolf.hp, wolf.kind.max_hp());
    }
}

/// Генерация заселяет мир кроликами только на траве и лесной траве.
#[test]
fn generation_spawns_rabbits_on_grass() {
    use crate::simulation::entities::EntityKind;

    let world = generate_world(WorldSize::new(256), 42);
    let rabbits: Vec<_> = world
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Rabbit)
        .collect();
    assert!(!rabbits.is_empty(), "на карте нет кроликов");

    for rabbit in rabbits {
        let tile = TilePos {
            x: rabbit.pos.x.floor() as i32,
            y: rabbit.pos.y.floor() as i32,
        };
        assert!(
            matches!(
                world.terrain_kind(tile),
                TileType::Grass | TileType::ForestGrass
            ),
            "кролик появился не на траве: {:?}",
            tile
        );
        assert_eq!(rabbit.hp, 40.0);
        assert_eq!(rabbit.kind.max_hp(), 40.0);
    }
}

/// Генерация даёт яблоням запас плодов 4–10.
#[test]
fn generated_apple_trees_have_four_to_ten_fruits() {
    let world = generate_world(WorldSize::new(256), 42);
    let mut found = 0;
    for chunk in &world.chunks {
        for object in chunk.objects.values() {
            if object.kind != ObjectKind::AppleTree {
                continue;
            }
            found += 1;
            assert!(
                (4..=10).contains(&object.charge),
                "запас плодов вне диапазона: {}",
                object.charge
            );
        }
    }
    assert!(found > 0, "на карте нет яблонь");
}

// --- Слой высот ---

/// Кривая высот согласована с новыми полосами рельефа: море 0, камень
/// от 24, горы от 65, верх диапазона 255.
#[test]
fn height_curve_matches_terrain_bands() {
    use crate::simulation::generation::height::{
        ELEV_HIGH, ELEV_MOUNTAIN, ELEV_SEA, ELEV_STONE, MOUNTAIN_HEIGHT, SNOW_HEIGHT, STONE_HEIGHT,
    };
    use crate::simulation::generation::height_for_elevation;
    use crate::simulation::heightmap::{HEIGHT_MAX, SEA_LEVEL};

    assert_eq!(height_for_elevation(ELEV_SEA), SEA_LEVEL);
    assert_eq!(height_for_elevation(ELEV_STONE), STONE_HEIGHT);
    assert_eq!(height_for_elevation(ELEV_MOUNTAIN), MOUNTAIN_HEIGHT);
    assert_eq!(height_for_elevation(ELEV_HIGH), SNOW_HEIGHT);
    assert_eq!(height_for_elevation(1.0), HEIGHT_MAX);
    assert_eq!(height_for_elevation(0.0), 0);
    // Кривая монотонна и не выходит за диапазон.
    let mut previous = 0;
    for step in 0..=1000 {
        let height = height_for_elevation(step as f32 / 1000.0);
        assert!(height >= previous);
        assert!(height <= HEIGHT_MAX);
        previous = height;
    }
}

/// Высоты всех тайлов лежат в диапазоне, а типы рельефа не противоречат
/// высоте: море плоское, камень и горы — в своих полосах.
#[test]
fn generated_heights_fit_terrain_types() {
    use crate::simulation::generation::height::{MOUNTAIN_HEIGHT, STONE_HEIGHT};
    use crate::simulation::heightmap::{HEIGHT_MAX, SEA_LEVEL};

    let world = generate_world(WorldSize::new(256), 999);
    let mut ocean_count = 0;
    for y in 0..world.size.tiles() as i32 {
        for x in 0..world.size.tiles() as i32 {
            let pos = TilePos { x, y };
            let height = world.height(pos).expect("тайл внутри мира");
            assert!(height <= HEIGHT_MAX);
            match world.terrain_kind(pos) {
                TileType::Ocean => {
                    ocean_count += 1;
                    assert_eq!(height, SEA_LEVEL, "океан должен быть плоским");
                }
                TileType::Stone => {
                    assert!(height >= STONE_HEIGHT, "камень ниже {STONE_HEIGHT}: {height}")
                }
                TileType::Mountain => assert!(
                    height >= MOUNTAIN_HEIGHT,
                    "гора ниже {MOUNTAIN_HEIGHT}: {height}"
                ),
                _ => {}
            }
        }
    }
    assert!(ocean_count > 0, "на карте нет океана");
}

/// Высоты участвуют в детерминизме: два мира с одним seed совпадают
/// полностью, включая слой высот.
#[test]
fn generated_heights_are_deterministic() {
    let first = generate_world(WorldSize::new(256), 12345);
    let second = generate_world(WorldSize::new(256), 12345);
    assert_eq!(first.chunks, second.chunks);
}
