//! Тултипы предметов: всплывающее название при наведении на слот
//! инвентаря, клетку крафта или слот хотбара, в котором лежит предмет.
//!
//! Названия предметов — данные в `info/items.json` (каталог описаний по
//! GAME_DESIGN раздел 4.1): предметов будет много, правки не должны
//! требовать перекомпиляции. Отсутствующий или битый файл не роняет игру —
//! остаётся технический идентификатор предмета.

use std::collections::HashMap;

use bevy::picking::hover::Hovered;
use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::app::state::{GameMode, GameState, MenuKind};
use crate::simulation::containers;
use crate::ui::bottom_panel::{self, PanelAction};
use crate::simulation::items::{ItemKind, SlotRef, ITEM_KIND_COUNT};
use crate::ui::UiFont;
use crate::ui::creative::CreativeTile;
use crate::ui::stations::{SlotIndex, StationPlayerSlot, StationSlot, StationUi};
use crate::ui::inventory::{CraftBookRes, CraftResultSlot, CraftingUi, InventoryUi, InventoryWindowSlot};
use crate::ui::inventory::{CraftSlot, InvSlot};
use crate::ui::style;
use serde::Deserialize;

pub struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ItemNames>()
            .add_systems(Startup, load_item_names)
            .add_systems(Update, update_tooltip);
    }
}

/// Названия предметов, разложенные по дискриминанту `ItemKind`: вместо
/// поиска в `HashMap` по строковому id на каждый кадр/тултип — прямой
/// индекс массива. `lowercase` — те же названия в нижнем регистре для
/// поиска в творческом меню: `to_lowercase` на каждый ввод/кадр по 91
/// предмету был лишней работой в UI-потоке.
#[derive(Resource, Default)]
pub struct ItemNames {
    names: Vec<Option<String>>,
    lowercase: Vec<Option<String>>,
}

impl ItemNames {
    /// Название предмета. Нет записи в файле — технический id, чтобы
    /// новый предмет не оставался безымянным до правки данных.
    /// Возвращается заимствование: тултипы не аллоцируют строку на каждое
    /// наведение (текст копируется только при фактической смене названия).
    pub fn name(&self, kind: ItemKind) -> &str {
        self.names
            .get(kind as usize)
            .and_then(|name| name.as_deref())
            .unwrap_or_else(|| kind.id())
    }

    /// Название в нижнем регистре (кэш для поиска творческого меню).
    pub fn lower_name(&self, kind: ItemKind) -> &str {
        self.lowercase
            .get(kind as usize)
            .and_then(|name| name.as_deref())
            .unwrap_or_else(|| kind.id())
    }
}

#[derive(Deserialize, Default)]
struct ItemsFile {
    #[serde(default)]
    items: Vec<ItemEntry>,
}

#[derive(Deserialize)]
struct ItemEntry {
    id: String,
    name: String,
}

/// Читает названия один раз при старте. Ошибка файла — предупреждение в
/// лог, игра работает с id-названиями.
fn load_item_names(mut commands: Commands) {
    let mut names = ItemNames {
        names: vec![None; ITEM_KIND_COUNT],
        lowercase: vec![None; ITEM_KIND_COUNT],
    };
    match std::fs::read_to_string("info/items.json")
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<ItemsFile>(&text).map_err(|e| e.to_string()))
    {
        Ok(file) => {
            let mut by_id: HashMap<String, String> = file
                .items
                .into_iter()
                .map(|entry| (entry.id, entry.name))
                .collect();
            let mut loaded = 0;
            for kind in ItemKind::ALL {
                if let Some(name) = by_id.remove(kind.id()) {
                    // Поиск творческого меню идёт по предпосчитанному
                    // нижнему регистру: `to_lowercase` на каждый ввод по
                    // всем 91 предметам не нужен.
                    names.lowercase[kind as usize] = Some(name.to_lowercase());
                    names.names[kind as usize] = Some(name);
                    loaded += 1;
                }
            }
            debug!("Загружено названий предметов: {loaded}");
        }
        Err(error) => {
            warn!("Тултипы: info/items.json не прочитан ({error}); названия предметов = id");
        }
    }
    commands.insert_resource(names);
}

/// Маркер узла тултипа. Один на кадр: создаётся при наведении, удаляется,
/// когда курсор ушёл со слота или предмет кончился.
#[derive(Component)]
struct TooltipNode;

/// Текст названия внутри тултипа.
#[derive(Component)]
struct TooltipText;

/// Смещение тултипа от курсора, px (право-низ, как подсказка у курсора).
const TOOLTIP_OFFSET: f32 = 16.0;

/// Показывает тултип над слотом с предметом. Кадр без наведённого слота с
/// предметом — тултипа нет. Перетаскивание стека скрывает тултип: под
/// курсором «виртуальная рука», а не содержимое слота.
fn update_tooltip(
    mut commands: Commands,
    windows: Query<&Window>,
    state: Res<GameState>,
    inv: Res<InventoryUi>,
    craft: Res<CraftingUi>,
    book: Res<CraftBookRes>,
    station: Res<StationUi>,
    inspector: Res<crate::ui::inspector::InspectorUi>,
    names: Res<ItemNames>,
    font: Res<UiFont>,
    inv_slots: Query<(&InvSlot, Has<InventoryWindowSlot>, &Hovered)>,
    craft_slots: Query<(&CraftSlot, &Hovered)>,
    result_slots: Query<&Hovered, With<CraftResultSlot>>,
    // Слоты всех окон и кнопки панели редактора одним параметром-кортежем:
    // у систем Bevy предел в 16 параметров.
    (creative_tiles, station_slots, station_player_slots, inspector_slots, panel_buttons): (
        Query<(&CreativeTile, &Hovered)>,
        Query<(&StationSlot, &Hovered)>,
        Query<(&StationPlayerSlot, &SlotIndex, &Hovered)>,
        Query<(&crate::ui::inspector::InspectorSlot, &Hovered)>,
        Query<(&PanelAction, &Hovered)>,
    ),
    mut tooltips: Query<(Entity, &mut Node, Option<&Children>), With<TooltipNode>>,
    mut texts: Query<&mut Text>,
) {
    // Модальные меню поверх всего (деревья, пауза): тултип слота под ними
    // вылезал бы поверх полноэкранного фона — спрайтов под курсором там
    // нет и показывать нечего. Творческое меню — исключение: у его плиток
    // есть тултипы.
    if state.active_menu.is_some() && state.active_menu != Some(MenuKind::Creative) {
        for (entity, _, _) in &mut tooltips {
            commands.entity(entity).despawn();
        }
        return;
    }
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        for (entity, _, _) in &mut tooltips {
            commands.entity(entity).despawn();
        }
        return;
    };
    let name = hovered_item_name(
        &state,
        &inv,
        &craft,
        &book,
        state.controlled_mind(),
        &station,
        inspector.open,
        &names,
        &inv_slots,
        &craft_slots,
        &result_slots,
        &creative_tiles,
        &station_slots,
        &inspector_slots,
        &station_player_slots,
        &panel_buttons,
    );

    let Some(name) = name else {
        for (entity, _, _) in &mut tooltips {
            commands.entity(entity).despawn();
        }
        return;
    };

    let left = cursor.x + TOOLTIP_OFFSET;
    let top = cursor.y + TOOLTIP_OFFSET;
    match tooltips.iter().next() {
        Some((_, _, _)) => {
            // Тултип уже на экране: двигаем узел и обновляем текст.
            for (_, mut node, _) in &mut tooltips {
                node.left = Val::Px(left);
                node.top = Val::Px(top);
            }
            for (_, _, children) in &mut tooltips {
                if let Some(children) = children
                    && let Some(child) = children.iter().next()
                    && let Ok(mut t) = texts.get_mut(child)
                    && t.as_str() != name
                {
                    **t = name.to_string();
                }
            }
        }
        None => {
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(left),
                        top: Val::Px(top),
                        padding: UiRect::all(Val::Px(6.0)),
                        ..default()
                    },
                    BackgroundColor(style::background()),
                    BorderColor::all(style::border()),
                    // Тултип следует за курсором и не должен перехватывать
                    // наведение/клики у слота под ним.
                    Pickable::IGNORE,
                    // Поверх панелей и модальных меню (творческое меню тоже
                    // показывает тултипы своих плиток).
                    GlobalZIndex(i32::MAX - 1),
                    TooltipNode,
                ))
                .with_children(|tip| {
                    tip.spawn((
                        Text::new(name),
                        TextFont {
                            font: font.0.clone().into(),
                            font_size: 14.0.into(),
                            ..default()
                        },
                        TextColor(style::text()),
                        TooltipText,
                    ));
                });
        }
    }
}

/// Название предмета в слоте под курсором, если такой есть.
fn hovered_item_name<'a>(
    state: &GameState,
    inv: &InventoryUi,
    craft: &CraftingUi,
    book: &CraftBookRes,
    mind: Option<&crate::simulation::entities::HumanMind>,
    station: &StationUi,
    inspector_open: Option<u64>,
    names: &'a ItemNames,
    inv_slots: &Query<(&InvSlot, Has<InventoryWindowSlot>, &Hovered)>,
    craft_slots: &Query<(&CraftSlot, &Hovered)>,
    result_slots: &Query<&Hovered, With<CraftResultSlot>>,
    creative_tiles: &Query<(&CreativeTile, &Hovered)>,
    station_slots: &Query<(&StationSlot, &Hovered)>,
    inspector_slots: &Query<(&crate::ui::inspector::InspectorSlot, &Hovered)>,
    station_player_slots: &Query<(&StationPlayerSlot, &SlotIndex, &Hovered)>,
    panel_buttons: &Query<(&PanelAction, &Hovered)>,
) -> Option<&'a str> {
    if inv.drag.is_some() || station.drag.is_some() {
        return None;
    }
    // Ховер берётся из Bevy picking (`Hovered`): слоты и кнопки уже носят
    // этот компонент, и он обновляется движком. Ручной хит-тест всех слотов
    // (в творческом меню — 91 плитки) каждый кадр был самой дорогой частью
    // тултипов.

    // Нижняя панель редактора: названия категорий, инструментов и линз.
    // Панель живёт в наблюдателе (в режиме игрока она скрыта), тултип
    // не нужен поверх модальных меню.
    if state.mode == GameMode::Observer
        && state.active_menu.is_none()
        && let Some((action, _)) = panel_buttons.iter().find(|(_, hovered)| hovered.get())
    {
        // Над кнопкой панели показывается только её название (у плиток
        // времени названия нет — они подписаны сами).
        return bottom_panel::panel_action_tooltip(action);
    }

    // Окно инспектора: слот показывает название предмета из инвентаря
    // выбранного существа.
    if let Some(id) = inspector_open
        && let Some((slot, _)) = inspector_slots
            .iter()
            .find(|(_, hovered)| hovered.get())
    {
        let slot_ref = crate::simulation::items::SlotRef {
            area: slot.area,
            index: slot.index,
        };
        if let Some(stack) = state
            .world
            .as_ref()
            .and_then(|world| world.entities.get(id))
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.inventory.get(slot_ref))
        {
            return Some(names.name(stack.kind));
        }
        return None;
    }

    // Творческое меню: плитка показывает название своего предмета.
    if state.active_menu == Some(MenuKind::Creative) {
        let (tile, _) = creative_tiles
            .iter()
            .find(|(_, hovered)| hovered.get())?;
        return Some(names.name(tile.kind));
    }
    if state.active_menu.is_some() {
        return None;
    }

    // Окно станции: пустой слот показывает роль («Топливо», «Только уголь»),
    // занятый — название предмета, как остальные слоты.
    if station.open {
        if let Some(kind) = station.kind
            && let Some((slot, _)) = station_slots
                .iter()
                .find(|(_, hovered)| hovered.get())
        {
            let stack = state
                .world
                .as_ref()
                .and_then(|world| containers::container_at(world, station.pos?))
                .and_then(|container| container.get(slot.index));
            return Some(match stack {
                Some(stack) => names.name(stack.kind),
                None => containers::slot_hint(kind, slot.index)?,
            });
        }
        if let Some((_, index, _)) = station_player_slots
            .iter()
            .find(|(_, _, hovered)| hovered.get())
            && let Some(stack) = mind.and_then(|m| m.inventory.items.get(index.0).copied().flatten())
        {
            return Some(names.name(stack.kind));
        }
        return None;
    }

    let p = mind?;

    // Слот результата крафта: название подобранного рецепта.
    if inv.open
        && result_slots.iter().any(|hovered| hovered.get())
        && let Some(recipe) = book.0.matching_recipe(&craft.slots)
    {
        return Some(names.name(recipe.result.kind));
    }

    // Клетки крафта (окно инвентаря открыто, иначе клеток не существует).
    if inv.open
        && let Some(stack) = craft_slots
            .iter()
            .find(|(_, hovered)| hovered.get())
            .and_then(|(slot, _)| craft.slots.get(slot.index).copied().flatten())
    {
        return Some(names.name(stack.kind));
    }

    // Слоты инвентаря и хотбара (оба носят InvSlot). Слоты окна живут,
    // только пока окно открыто; хотбар виден в режиме игрока — скрытый
    // (Display::None) хотбар picking не подсвечивает.
    for (slot, window_slot, hovered) in inv_slots {
        if !hovered.get() {
            continue;
        }
        let visible = if window_slot { inv.open } else { state.mode == GameMode::Player };
        if !visible {
            continue;
        }
        let stack = p.inventory.get(SlotRef {
            area: slot.area,
            index: slot.index,
        })?;
        return Some(names.name(stack.kind));
    }
    None
}
