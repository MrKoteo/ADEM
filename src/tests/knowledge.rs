//! Тесты каталога знаний: битовый набор, предпосылки, требования крафта.

use crate::simulation::CommandQueue;
use crate::simulation::commands::Command;
use crate::simulation::crafts::{CraftBook, parse_recipe};
use crate::simulation::dirty::DirtyTiles;
use crate::simulation::entities::EntityKind;
use crate::simulation::knowledge::{KnowledgeCatalog, KnowledgeSet};
use crate::simulation::scheduler::Scheduler;
use crate::simulation::world::{World, WorldPos, WorldSize};

#[test]
fn knowledge_set_is_compact_and_iterates() {
    let mut set = KnowledgeSet::new();
    assert!(set.is_empty());
    assert!(set.insert(0));
    assert!(set.insert(77));
    assert!(set.insert(200));
    assert!(!set.insert(77), "повторная вставка не меняет набор");
    assert!(set.has(0) && set.has(77) && set.has(200));
    assert!(!set.has(1));
    assert_eq!(set.count(), 3);

    let mut seen = Vec::new();
    set.for_each(|index| seen.push(index));
    assert_eq!(seen, vec![0, 77, 200]);
}

#[test]
fn catalog_loads_tech_and_social_with_prerequisites() {
    let catalog = KnowledgeCatalog::load_default();
    assert!(catalog.len() > 50, "каталог должен загрузить технологии и SI");

    let stone_knapping = catalog
        .index_of("stone_knapping")
        .expect("технология stone_knapping");
    let stone_tools = catalog.index_of("stone_tools").expect("технология stone_tools");
    assert!(catalog.get(stone_knapping).is_some_and(|info| info.study_hours > 0.0));
    assert!(catalog.base_ticks(stone_knapping) > 0);

    let mut known = KnowledgeSet::new();
    assert!(!catalog.requirements_met(stone_tools, &known));
    known.insert(stone_knapping);
    assert!(catalog.requirements_met(stone_tools, &known));
}

#[test]
fn recipe_knowledge_is_all_or_nothing() {
    let catalog = KnowledgeCatalog::load_default();
    let recipe = parse_recipe(
        r#"{
            "id": "knowledge_test",
            "parent": "manual",
            "shapeless": true,
            "ingredients": [{ "item": "stick", "count": 1 }],
            "knowledge": ["stone_tools"],
            "result": { "item": "stick", "count": 1 }
        }"#,
    )
    .expect("рецепт с knowledge должен разбираться");
    assert_eq!(recipe.knowledge, vec!["stone_tools".to_string()]);

    let mut known = KnowledgeSet::new();
    assert!(!recipe.knowledge_satisfied(&known, &catalog));

    let index = catalog.index_of("stone_tools").unwrap();
    known.insert(index);
    assert!(recipe.knowledge_satisfied(&known, &catalog));

    // Неизвестный id трактуется как невыполнимое требование.
    known.clear();
    known.insert(255);
    assert!(!recipe.knowledge_satisfied(&known, &catalog));
}

/// Игрок выбирает карточку в дереве: команда ставит знание в изучение,
/// но только если изучены предпосылки.
#[test]
fn study_command_respects_prerequisites() {
    let mut world = World::new(WorldSize::new(128), 3);
    world.load_static_data();
    let actor = world
        .entities
        .spawn(EntityKind::Human, WorldPos { x: 10.5, y: 10.5 });
    let stone_tools = world.knowledge.index_of("stone_tools").unwrap();
    let stone_knapping = world.knowledge.index_of("stone_knapping").unwrap();

    let mut queue = CommandQueue::default();
    let mut dirty = DirtyTiles::default();
    let mut scheduler = Scheduler::default();

    queue.push(Command::Study {
        id: actor,
        knowledge: stone_tools,
    });
    queue.apply_all(
        &mut world,
        None,
        &mut dirty,
        &mut scheduler,
        100,
        &CraftBook::default(),
    );
    assert_eq!(
        world.entities.get(actor).unwrap().mind.as_ref().unwrap().studying,
        None,
        "без stone_knapping изучение stone_tools не начинается"
    );

    queue.push(Command::Study {
        id: actor,
        knowledge: stone_knapping,
    });
    queue.apply_all(
        &mut world,
        None,
        &mut dirty,
        &mut scheduler,
        100,
        &CraftBook::default(),
    );
    let mind = world.entities.get(actor).unwrap().mind.as_ref().unwrap();
    assert_eq!(mind.studying, Some(stone_knapping));
    assert!(mind.study_finish_tick > 100);
}

#[test]
fn data_crafts_only_reference_known_knowledge() {
    let catalog = KnowledgeCatalog::load_default();
    let loaded = CraftBook::load_dir("data/crafts");
    assert!(loaded.errors.is_empty(), "ошибки рецептов: {:?}", loaded.errors);
    for recipe in &loaded.book.recipes {
        for id in &recipe.knowledge {
            assert!(
                catalog.index_of(id).is_some(),
                "рецепт {} требует неизвестное знание {id}",
                recipe.id
            );
        }
    }
}
