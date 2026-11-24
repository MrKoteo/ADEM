//! Тесты автономного поведения людей (ИИ выживания, simulation/ai.rs):
//! голодный человек ищет еду, ночью идёт к костру.

use crate::simulation::entities::{EntityKind, HumanGoal};
use crate::simulation::hunger::Hunger;
use crate::simulation::items::ItemKind;
use crate::simulation::objects::{BuildingKind, WorldObject};
use crate::simulation::tile::TileType;
use crate::simulation::world::{TilePos, World, WorldPos, WorldSize};

/// Тик, соответствующий началу часа `hour` первых суток (1 игровой час
/// = 300 тиков: 3600 игровых секунд / 12 секунд на тик).
fn hour_tick(hour: u32) -> u64 {
    u64::from(hour) * 300
}

/// Голодный человек рядом с яблоней: ИИ ставит цель «еда», собирает яблоки
/// в инвентарь и ест их — сытость растёт.
#[test]
fn hungry_human_seeks_and_harvests_apples() {
    let mut world = World::new(WorldSize::new(256), 1);
    let human_pos = TilePos { x: 10, y: 10 };
    let tree_pos = TilePos { x: 11, y: 10 };
    // Суша вокруг: сгенерированный мир пуст, рельеф выставляем явно.
    for pos in [
        human_pos,
        tree_pos,
        TilePos { x: 12, y: 10 },
        TilePos { x: 10, y: 11 },
        TilePos { x: 11, y: 11 },
    ] {
        world.set_terrain(pos, TileType::Grass);
    }
    world.set_object(tree_pos, Some(WorldObject::apple_tree_with_fruit(7)));
    let id = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(human_pos));
    world
        .entities
        .get_mut(id)
        .unwrap()
        .hunger = Hunger::with_value(30.0);

    // Дневные тики (старт мира — 8:00): ИИ ищет еду, собирает яблоки и ест.
    let start = crate::simulation::time::start_tick() + 10;
    let mut saw_food_goal = false;
    let mut saw_apples = false;
    for tick in start..start + 200 {
        world.tick_entities(tick, None);
        let entity = world.entities.get(id).unwrap();
        saw_food_goal |= matches!(
            entity.mind.and_then(|m| m.goal),
            Some(HumanGoal::Food(_))
        );
        saw_apples |= entity
            .mind
            .map(|m| m.inventory.count_of(ItemKind::Apple) > 0)
            .unwrap_or(false);
    }

    assert!(
        saw_food_goal || saw_apples,
        "голодный человек должен был искать еду (яблоня в радиусе поиска)"
    );
    let hunger = world
        .entities
        .get(id)
        .unwrap()
        .hunger
        .value();
    assert!(hunger > 30.0, "человек должен был поесть, hunger = {hunger}");
}

/// Ночью (час ≥ 21) человек в радиусе поиска выбирает цель «костёр»,
/// доходит и стоит рядом до утра.
#[test]
fn human_walks_to_campfire_at_night() {    let mut world = World::new(WorldSize::new(256), 1);
    let human_pos = TilePos { x: 20, y: 20 };
    let fire_pos = TilePos { x: 25, y: 20 };
    for x in 18..=27 {
        world.set_terrain(TilePos { x, y: 20 }, TileType::Grass);
    }
    world.set_object(fire_pos, Some(WorldObject::building(BuildingKind::Campfire)));
    let id = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(human_pos));

    // Тики 22:00 (ночь): ИИ находит костёр и идёт к нему.
    let start = hour_tick(22);
    let mut saw_fire_goal = false;
    for tick in start..start + 200 {
        world.tick_entities(tick, None);
        saw_fire_goal |= matches!(
            world.entities.get(id).unwrap().mind.and_then(|m| m.goal),
            Some(HumanGoal::Campfire(_))
        );
    }
    assert!(saw_fire_goal, "ночью человек должен выбрать цель «костёр»");

    // За ночь человек дошёл и стоит у огня (в кольце прибытия).
    let dist = world
        .entities
        .get(id)
        .unwrap()
        .pos
        .dist(WorldPos::of_tile_center(fire_pos));
    assert!(dist <= 1.5, "человек должен дойти до костра, дистанция {dist}");
}

/// Сытый человек днём без еды вокруг не стоит на месте: ИИ отправляет его
/// в брожение. Регрессия на вечное сброс стояния FSM'ом, из-за которого
/// люди простаивали.
#[test]
fn idle_human_wanders_during_day() {
    let mut world = World::new(WorldSize::new(256), 1);
    let human_pos = TilePos { x: 20, y: 20 };
    for y in 12..=28 {
        for x in 12..=28 {
            world.set_terrain(TilePos { x, y }, TileType::Grass);
        }
    }
    let id = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(human_pos));

    // Дневные тики, еды в радиусе поиска нет: человек должен сместиться.
    let start = crate::simulation::time::start_tick() + 10;
    for tick in start..start + 600 {
        world.tick_entities(tick, None);
    }
    let dist = world
        .entities
        .get(id)
        .unwrap()
        .pos
        .dist(WorldPos::of_tile_center(human_pos));
    assert!(dist > 1.0, "сытый человек должен бродить, сместился на {dist}");
}

/// Изучение идёт в фоне: при непустом каталоге человек выбирает карточку,
/// а по достижении срока знание появляется в его личном наборе.
#[test]
fn human_studies_knowledge_in_background() {
    let mut world = World::new(WorldSize::new(256), 1);
    world.knowledge = crate::simulation::knowledge::KnowledgeCatalog::load_default();
    let pos = TilePos { x: 10, y: 10 };
    for y in 9..=11 {
        for x in 9..=11 {
            world.set_terrain(TilePos { x, y }, TileType::Grass);
        }
    }
    let id = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(pos));
    let tick = crate::simulation::time::start_tick() + 10;
    // Поиск новой технологии разрежен по тикам, чтобы не сканировать
    // каталог каждый тик у каждого человека. Дожидаемся окна этого id.
    let period = 5 * crate::simulation::time::TICKS_PER_SEC as u64;
    let tick = tick + (id % period + period - tick % period) % period;

    world.tick_entities(tick, None);
    let mind = world.entities.get(id).unwrap().mind.unwrap();
    let studying = mind.studying.expect("человек должен начать изучение");
    assert!(mind.study_finish_tick > tick);
    assert!(!mind.knowledge.has(studying));

    world
        .entities
        .get_mut(id)
        .unwrap()
        .mind
        .as_mut()
        .unwrap()
        .study_finish_tick = tick;
    world.tick_entities(tick, None);
    let mind = world.entities.get(id).unwrap().mind.unwrap();
    assert!(mind.knowledge.has(studying));
    assert_ne!(mind.studying, Some(studying));
}

/// Два человека у одного костра обмениваются изученным: это и есть
/// «поселение делится знаниями».
#[test]
fn humans_share_knowledge_at_campfire() {
    let mut world = World::new(WorldSize::new(256), 1);
    world.knowledge = crate::simulation::knowledge::KnowledgeCatalog::load_default();
    let fire = TilePos { x: 10, y: 10 };
    for y in 8..=12 {
        for x in 8..=12 {
            world.set_terrain(TilePos { x, y }, TileType::Grass);
        }
    }
    world.set_object(fire, Some(WorldObject::building(BuildingKind::Campfire)));
    let first = world
        .entities
        .spawn(EntityKind::Human, WorldPos { x: 10.2, y: 10.2 });
    let second = world
        .entities
        .spawn(EntityKind::Human, WorldPos { x: 11.0, y: 10.2 });

    let known = world.knowledge.index_of("stone_knapping").unwrap();
    for id in [first, second] {
        let m = world.entities.get_mut(id).unwrap().mind.as_mut().unwrap();
        m.known_campfire = Some(fire);
        m.share_ready_tick = 0;
    }
    world
        .entities
        .get_mut(first)
        .unwrap()
        .mind
        .as_mut()
        .unwrap()
        .knowledge
        .insert(known);

    let tick = crate::simulation::time::start_tick() + 10;
    world.tick_entities(tick, None);

    assert!(
        world
            .entities
            .get(second)
            .unwrap()
            .mind
            .unwrap()
            .knowledge
            .has(known),
        "второй человек должен получить знание от первого"
    );
}

/// Роль меняется командой и сохраняется в HumanMind.
#[test]
fn role_command_changes_social_role() {
    use crate::simulation::commands::CommandQueue;
    use crate::simulation::entities::SocialRole;
    use crate::simulation::CraftBook;

    let mut world = World::new(WorldSize::new(128), 1);
    let id = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(TilePos { x: 5, y: 5 }));
    let mut queue = CommandQueue::default();
    queue.push(crate::simulation::Command::SetRole {
        id,
        role: SocialRole::Cook,
    });
    let mut dirty = crate::simulation::DirtyTiles::default();
    let mut scheduler = crate::simulation::Scheduler::default();
    queue.apply_all(&mut world, None, &mut dirty, &mut scheduler, 0, &CraftBook::default());
    assert_eq!(
        world.entities.get(id).unwrap().mind.unwrap().social_role,
        SocialRole::Cook
    );
}

/// Строитель с 16+ палками сам ставит костёр, а затем склад рядом с ним.
#[test]
fn builder_builds_campfire_and_storage() {
    use crate::simulation::entities::SocialRole;

    let mut world = World::new(WorldSize::new(128), 1);
    let human_pos = TilePos { x: 20, y: 20 };
    for y in 14..=27 {
        for x in 14..=27 {
            world.set_terrain(TilePos { x, y }, TileType::Grass);
        }
    }
    let id = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(human_pos));
    {
        let mind = world.entities.get_mut(id).unwrap().mind.as_mut().unwrap();
        mind.social_role = SocialRole::Builder;
        mind.inventory.add(ItemKind::Stick, 20);
    }

    let start = crate::simulation::time::start_tick() + 10;
    for tick in start..start + 200 {
        world.tick_entities(tick, None);
    }

    let mut campfire = None;
    for y in 14..=27 {
        for x in 14..=27 {
            let pos = TilePos { x, y };
            if world.object_at(pos).is_some_and(|o| {
                o.kind == crate::simulation::ObjectKind::Building(BuildingKind::Campfire)
            }) {
                campfire = Some(pos);
            }
        }
    }
    let fire = campfire.expect("строитель должен поставить костёр");
    assert_eq!(world.entities.get(id).unwrap().mind.unwrap().known_campfire, Some(fire));
    assert_eq!(world.entities.get(id).unwrap().mind.unwrap().inventory.count_of(ItemKind::Stick), 4);

    // Дадим материалы на склад и запомненный костёр. Склад должен встать
    // в пределах поселения, а не где угодно.
    {
        let mind = world.entities.get_mut(id).unwrap().mind.as_mut().unwrap();
        mind.inventory.add(ItemKind::Stick, 36);
        mind.known_campfire = Some(fire);
        mind.goal = None;
        mind.next_think_tick = 0;
    }
    for tick in start + 200..start + 500 {
        world.tick_entities(tick, None);
    }

    let mut storage = None;
    for y in 14..=27 {
        for x in 14..=27 {
            let pos = TilePos { x, y };
            if world.object_at(pos).is_some_and(|o| {
                o.kind == crate::simulation::ObjectKind::Building(BuildingKind::Storage)
            }) {
                storage = Some(pos);
            }
        }
    }
    let storage = storage.expect("строитель должен поставить склад");
    assert!(world.entities.get(id).unwrap().mind.unwrap().known_storage == Some(storage));
    assert!(
        WorldPos::of_tile_center(storage).dist(WorldPos::of_tile_center(fire)) <= 32.0,
        "склад должен быть в радиусе поселения"
    );
}


/// Полная логистическая цепочка: охотник кладёт мясо на склад, повар
/// берёт его, готовит на костре и возвращает готовую еду, голодный
/// человек берёт готовое со склада.
#[test]
fn hunter_cook_storage_chain() {
    use crate::simulation::entities::SocialRole;

    let mut world = World::new(WorldSize::new(128), 7);
    world.load_static_data();

    let fire = TilePos { x: 20, y: 20 };
    let storage = TilePos { x: 21, y: 20 };
    for y in 16..=24 {
        for x in 16..=25 {
            world.set_terrain(TilePos { x, y }, TileType::Grass);
        }
    }
    world.set_object_with_container(fire, Some(WorldObject::building(BuildingKind::Campfire)));
    world.set_object_with_container(storage, Some(WorldObject::building(BuildingKind::Storage)));

    let hunter = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(TilePos { x: 18, y: 20 }));
    let cook = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(TilePos { x: 19, y: 20 }));
    let eater = world
        .entities
        .spawn(EntityKind::Human, WorldPos::of_tile_center(TilePos { x: 22, y: 20 }));

    let fire_control = world.knowledge.index_of("fire_control").unwrap();
    for id in [hunter, cook, eater] {
        let mind = world.entities.get_mut(id).unwrap().mind.as_mut().unwrap();
        mind.known_campfire = Some(fire);
        mind.known_storage = Some(storage);
    }
    {
        let mind = world.entities.get_mut(hunter).unwrap().mind.as_mut().unwrap();
        mind.social_role = SocialRole::Hunter;
        mind.inventory.add(ItemKind::MeatHerbivoreRaw, 4);
    }
    {
        let mind = world.entities.get_mut(cook).unwrap().mind.as_mut().unwrap();
        mind.social_role = SocialRole::Cook;
        mind.knowledge.insert(fire_control);
    }
    {
        let entity = world.entities.get_mut(eater).unwrap();
        entity.hunger = Hunger::with_value(30.0);
    }

    let count_stored = |world: &World, kind: ItemKind| -> u32 {
        world
            .container_at(storage)
            .map(|c| {
                c.slots
                    .iter()
                    .flatten()
                    .filter(|s| s.kind == kind)
                    .map(|s| s.count)
                    .sum()
            })
            .unwrap_or(0)
    };

    let start = crate::simulation::time::start_tick();
    let mut saw_raw_on_storage = false;
    let mut saw_cooked_on_storage = false;
    let mut saw_eater_food = false;
    for tick in start..start + 2000 {
        world.tick_entities(tick, None);
        saw_raw_on_storage |= count_stored(&world, ItemKind::MeatHerbivoreRaw) > 0;
        saw_cooked_on_storage |= count_stored(&world, ItemKind::MeatHerbivoreCooked) > 0;
        saw_eater_food |= world
            .entities
            .get(eater)
            .and_then(|e| e.mind.as_ref())
            .is_some_and(|m| m.inventory.count_of(ItemKind::MeatHerbivoreCooked) > 0);
    }

    assert!(saw_raw_on_storage, "охотник должен положить мясо на склад");
    assert!(saw_cooked_on_storage, "повар должен приготовить еду на складе");
    assert!(saw_eater_food, "голодный человек должен взять готовую еду со склада");
}
