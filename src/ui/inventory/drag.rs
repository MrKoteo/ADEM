//! Перетаскивание стеков между слотами инвентаря, хотбара, сетки крафта
//! и мира: зажать ЛКМ на слоте и отпустить над целевым. Пока стек «в руке»,
//! за курсором следует иконка-призрак; отпускание мимо слота возвращает
//! стек на место или бросает его в мир, ПКМ отменяет перетаскивание.
//! Стак, отпущенный над клеткой сетки крафта, кладётся в неё целиком.

use bevy::input::mouse::MouseButton;
use bevy::prelude::*;

use crate::app::input::cursor_world_tile;
use crate::app::state::{CommandQueueRes, GameMode, GameState, MenuKind, WindowFocusState};
use crate::simulation::commands::Command;
use crate::simulation::items::{Inventory, ItemKind, SlotArea, SlotRef};
use crate::simulation::{ItemStack, TilePos};
use crate::ui::icons::UiIcons;
use crate::ui::cursor_over_node;

use super::{
    CraftSlot, CraftingUi, DragGhost, DragSource, DragState, GHOST_HALF, InventoryPanel, InventoryUi,
    InvSlot,
};

/// Порог сдвига курсора для начала перетаскивания из клетки сетки крафта,
/// px. Ниже порога клик остаётся кликом: положить/выбрать/разделить.
const CRAFT_DRAG_THRESHOLD: f32 = 4.0;

/// Перетаскивание стеков между слотами инвентаря и хотбара: зажать ЛКМ на
/// слоте и отпустить над целевым. Пока стек «в руке», за курсором следует
/// иконка-призрак; отпускание мимо слота возвращает стек на место, ПКМ
/// отменяет перетаскивание. В режиме игрока хотбар перетаскивается и без
/// открытого окна инвентаря. Клик без движения остаётся выбором источника
/// для сетки крафта (см. `on_slot_activated`).
pub(crate) fn inventory_drag(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    focus: Res<WindowFocusState>,
    mut state: ResMut<GameState>,
    mut inv: ResMut<InventoryUi>,
    mut craft: ResMut<CraftingUi>,
    mut craft_press: Local<Option<(usize, Vec2)>>,
    mut queue: ResMut<CommandQueueRes>,
    icons: Res<UiIcons>,
    windows: Query<&Window>,
    cameras: Query<(&Camera, &Transform), With<Camera2d>>,
    slots: Query<(&InvSlot, &ComputedNode, &UiGlobalTransform)>,
    craft_slots: Query<(&CraftSlot, &ComputedNode, &UiGlobalTransform)>,
    panels: Query<(&ComputedNode, &UiGlobalTransform), With<InventoryPanel>>,
    creative_panels: Query<(&ComputedNode, &UiGlobalTransform), With<crate::ui::creative::CreativePanel>>,
    mut ghosts: Query<(Entity, &mut Node), With<DragGhost>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    // Окно потеряло фокус (Alt+Tab): отпускание кнопки могло не дойти до
    // игры, поэтому стек возвращается на место, а не висит «в руке».
    if !focus.focused {
        *craft_press = None;
        if inv.drag.is_some() {
            cancel_drag(&mut commands, &mut inv, &mut craft, &mut state);
        }
        return;
    }
    // Окно инвентаря открыто — его слоты и хотбар; иначе перетаскивание
    // доступно по видимому хотбару, в том числе при открытом творческом
    // меню (бросок стека на плитки удаляет его, как в Minecraft).
    let creative_open = state.active_menu == Some(MenuKind::Creative);
    let hotbar_only =
        !inv.open && state.mode == GameMode::Player && (state.active_menu.is_none() || creative_open);
    if !inv.open && !hotbar_only {
        // Режим сменился/открылось меню посреди перетаскивания — стек
        // возвращается в исходный слот, а не остаётся «в руке» навсегда.
        *craft_press = None;
        if inv.drag.is_some() {
            cancel_drag(&mut commands, &mut inv, &mut craft, &mut state);
        }
        return;
    }
    let Some(cursor) = window.cursor_position() else {
        // Курсор ушёл за пределы окна: отпускание кнопки всё равно
        // нужно обработать, иначе стек останется «в руке».
        if inv.drag.is_some() {
            if mouse.just_released(MouseButton::Right) {
                cancel_drag(&mut commands, &mut inv, &mut craft, &mut state);
            } else if mouse.just_released(MouseButton::Left) {
                finish_drag(&mut commands, &mut inv, &mut craft, &mut state, None);
            }
        }
        return;
    };

    // Старт: ЛКМ нажата над непустым слотом инвентаря/хотбара — стек
    // снимается в «руку» и появляется призрак. Над клеткой сетки крафта
    // перетаскивание начинается только после сдвига курсора: обычный клик
    // по клетке остаётся за наблюдателем активации. Не выходим из системы:
    // если кнопку успели отпустить в том же кадре (быстрый клик между
    // кадрами), ниже сработает и завершение перетаскивания.
    if mouse.just_pressed(MouseButton::Left) && inv.drag.is_none() {
        let cell = inv
            .open
            .then(|| hovered_craft_cell(window, &craft_slots, cursor))
            .flatten();
        if let Some(index) = cell {
            if craft.slots[index].is_some() {
                *craft_press = Some((index, cursor));
            }
        } else if let Some(hover) = hovered_slot(window, &slots, cursor) {
            let target = SlotRef { area: hover.area, index: hover.index };
            let Some(p) = state.controlled_mind_mut() else {
                return;
            };
            let Some(stack) = p.inventory.get(target) else {
                return;
            };
            p.inventory.take(target);
            let ghost = spawn_drag_ghost(&mut commands, &icons, cursor, stack.kind);
            inv.drag = Some(DragState { from: DragSource::Slot(target), stack, ghost });
        } else {
            return;
        }
    }
    // Нажатие началось над клеткой сетки: перетаскивание стартует после
    // сдвига курсора, иначе обычный клик по клетке не дошёл бы до
    // наблюдателя активации (положить/выбрать/разделить).
    if let Some((index, press)) = *craft_press {
        if !mouse.pressed(MouseButton::Left) {
            *craft_press = None;
        } else if inv.drag.is_none() && cursor.distance(press) > CRAFT_DRAG_THRESHOLD {
            *craft_press = None;
            if let Some(stack) = craft.slots[index].take() {
                if craft.selected == Some(index) {
                    craft.selected = None;
                }
                let ghost = spawn_drag_ghost(&mut commands, &icons, cursor, stack.kind);
                inv.drag = Some(DragState { from: DragSource::Craft(index), stack, ghost });
            }
        }
    }
    if inv.drag.is_none() {
        return;
    }
    // ПКМ во время перетаскивания: отмена, стек возвращается на место.
    if mouse.just_released(MouseButton::Right) {
        cancel_drag(&mut commands, &mut inv, &mut craft, &mut state);
        return;
    }
    // Призрак следует за курсором.
    for (_, mut node) in &mut ghosts {
        node.left = Val::Px(cursor.x - GHOST_HALF);
        node.top = Val::Px(cursor.y - GHOST_HALF);
    }
    // Отпускание ЛКМ: перенос в слот под курсором; над панелью творческого
    // меню — удаление стека; за пределами открытого окна — выброс в мир в
    // сторону курсора; иначе возврат на место.
    if mouse.just_released(MouseButton::Left) {
        let hovered = hovered_slot(window, &slots, cursor);
        let target = hovered.map(|s| SlotRef { area: s.area, index: s.index });
        let over_panel = panels
            .iter()
            .any(|(computed, transform)| cursor_over_node(window, computed, transform, cursor));
        let over_creative = creative_panels
            .iter()
            .any(|(computed, transform)| cursor_over_node(window, computed, transform, cursor));
        if let Some(index) = hovered_craft_cell(window, &craft_slots, cursor) {
            // Отпускание над клеткой сетки крафта: весь стак — в клетку
            // (как перетаскивание стака в Minecraft).
            drop_drag_into_craft(&mut commands, &mut inv, &mut craft, &mut state, index);
        } else if let Some(t) = target {
            finish_drag(&mut commands, &mut inv, &mut craft, &mut state, Some(t));
        } else if creative_open && over_creative {
            delete_drag(&mut commands, &mut inv);
        } else if inv.open
            && !over_panel
            && let Some(tile) = cursor_world_tile(window, &cameras, state.world.as_ref())
        {
            throw_drag(&mut commands, &mut inv, &mut state, &mut queue, tile);
        } else {
            finish_drag(&mut commands, &mut inv, &mut craft, &mut state, None);
        }
    }
}

/// Удаляет перетащенный стек (отпускание над панелью творческого меню):
/// стек не возвращается в слот и исчезает, как в Minecraft.
fn delete_drag(commands: &mut Commands, inv: &mut InventoryUi) {
    let Some(drag) = inv.drag.take() else {
        return;
    };
    commands.entity(drag.ghost).despawn();
}

/// Бросает перетащенный стек в мир в сторону курсора. Из слота инвентаря
/// стек возвращается на место, а команда `PlayerThrowStack` списывает его
/// оттуда и бросает (симуляция сама считает дальность и задержку подбора);
/// бросать так можно только предмет из misc. Стек из сетки крафта уже снят
/// с клетки, поэтому бросается командой `PlayerDropStack` напрямую.
fn throw_drag(
    commands: &mut Commands,
    inv: &mut InventoryUi,
    state: &mut GameState,
    queue: &mut CommandQueueRes,
    target: TilePos,
) {
    let Some(drag) = inv.drag.take() else {
        return;
    };
    commands.entity(drag.ghost).despawn();
    match drag.from {
        DragSource::Slot(slot) => {
            let throw_slot = (slot.area == SlotArea::Misc).then_some(slot.index);
            if let Some(p) = state.controlled_mind_mut() {
                p.inventory.put(slot, drag.stack);
            }
            if let Some(slot) = throw_slot {
                queue.0.push(Command::PlayerThrowStack {
                    target,
                    slot: slot as u8,
                });
            }
        }
        DragSource::Craft(_) => {
            queue.0.push(Command::PlayerDropStack {
                pos: target,
                stack: drag.stack,
            });
        }
    }
}

/// Слот инвентаря под курсором, если есть. Хит-тест общий с другими
/// проверками UI (`cursor_over_node`).
fn hovered_slot<'a>(
    window: &Window,
    slots: &'a Query<(&InvSlot, &ComputedNode, &UiGlobalTransform)>,
    cursor: Vec2,
) -> Option<&'a InvSlot> {
    slots
        .iter()
        .find(|(_, computed, transform)| cursor_over_node(window, computed, transform, cursor))
        .map(|(slot, _, _)| slot)
}

/// Клетка сетки крафта под курсором, если есть.
fn hovered_craft_cell(
    window: &Window,
    cells: &Query<(&CraftSlot, &ComputedNode, &UiGlobalTransform)>,
    cursor: Vec2,
) -> Option<usize> {
    cells
        .iter()
        .find(|(_, computed, transform)| cursor_over_node(window, computed, transform, cursor))
        .map(|(slot, _, _)| slot.index)
}

/// Создаёт призрак-иконку перетаскиваемого стека под курсором. Призрак —
/// самостоятельный UI-узел поверх всего: окно инвентаря может закрыться
/// прямо во время перетаскивания, а хотбар вообще живёт без него.
/// `GlobalZIndex` держит иконку над панелями.
fn spawn_drag_ghost(
    commands: &mut Commands,
    icons: &UiIcons,
    cursor: Vec2,
    kind: ItemKind,
) -> Entity {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(cursor.x - GHOST_HALF),
                top: Val::Px(cursor.y - GHOST_HALF),
                width: Val::Px(GHOST_HALF * 2.0),
                height: Val::Px(GHOST_HALF * 2.0),
                ..default()
            },
            ImageNode::new(icons.item(kind).clone()),
            // Призрак висит прямо под курсором: будучи обычным UI-узлом,
            // он перехватывал бы наведение и клики у слота под ним.
            Pickable::IGNORE,
            GlobalZIndex(i32::MAX),
            DragGhost,
        ))
        .id()
}

/// Возвращает стек «из руки» в источник перетаскивания: слот инвентаря
/// или клетку сетки крафта. Клетка освобождается при старте перетаскивания
/// (и не заполняется, пока стек в руке), поэтому обычно она пуста.
fn return_drag_stack(
    craft: &mut CraftingUi,
    state: &mut GameState,
    from: DragSource,
    stack: ItemStack,
) {
    match from {
        DragSource::Slot(slot) => {
            if let Some(p) = state.controlled_mind_mut() {
                p.inventory.put(slot, stack);
            }
        }
        DragSource::Craft(index) => match &mut craft.slots[index] {
            Some(cell) if cell.kind == stack.kind => cell.count += stack.count,
            cell @ None => *cell = Some(stack),
            // Клетку успел занять другой предмет: страховка от потери —
            // стек уходит в инвентарь.
            Some(_) => {
                if let Some(p) = state.controlled_mind_mut() {
                    p.inventory.add(stack.kind, stack.count);
                }
            }
        },
    }
}

/// Завершает перетаскивание над клеткой сетки крафта: весь стак уходит
/// в клетку, доливаясь к тому же виду до предела стака (разные виды не
/// смешиваются — стек просто возвращается). Не поместившийся остаток
/// возвращается в источник, поэтому предметы не теряются.
fn drop_drag_into_craft(
    commands: &mut Commands,
    inv: &mut InventoryUi,
    craft: &mut CraftingUi,
    state: &mut GameState,
    target: usize,
) {
    let Some(drag) = inv.drag.take() else {
        return;
    };
    commands.entity(drag.ghost).despawn();
    // Отпускание над исходной клеткой — просто возврат стека на место.
    if drag.from == DragSource::Craft(target) {
        return_drag_stack(craft, state, drag.from, drag.stack);
        return;
    }
    let kind = drag.stack.kind;
    let max = kind.max_stack();
    let space = match craft.slots[target] {
        None => max,
        Some(cell) if cell.kind == kind => max.saturating_sub(cell.count),
        Some(_) => 0,
    };
    let moved = drag.stack.count.min(space);
    if moved > 0 {
        match &mut craft.slots[target] {
            Some(cell) => cell.count += moved,
            None => craft.slots[target] = Some(ItemStack { kind, count: moved }),
        }
        // Стек «в руке» больше не выбран как источник для кликов.
        inv.selected = None;
    }
    let left = drag.stack.count - moved;
    if left > 0 {
        return_drag_stack(
            craft,
            state,
            drag.from,
            ItemStack { kind, count: left },
        );
    }
}

/// Завершает перетаскивание из клетки сетки крафта в слот инвентаря:
/// стек кладётся в целевой слот, встречный предмет уходит в освободившуюся
/// клетку (свап); мусорка затирает прежнее содержимое. Несовместимый
/// слот возвращает стек в клетку.
fn finish_craft_drag(
    inv: &mut InventoryUi,
    craft: &mut CraftingUi,
    state: &mut GameState,
    index: usize,
    stack: ItemStack,
    target: Option<SlotRef>,
) {
    let Some(t) = target else {
        return_drag_stack(craft, state, DragSource::Craft(index), stack);
        return;
    };
    if !Inventory::accepts(stack.kind, t.area, t.index) {
        return_drag_stack(craft, state, DragSource::Craft(index), stack);
        return;
    }
    let Some(p) = state.controlled_mind_mut() else {
        return_drag_stack(craft, state, DragSource::Craft(index), stack);
        return;
    };
    let previous = p.inventory.take(t);
    p.inventory.put(t, stack);
    // Встречный предмет — в освободившуюся клетку; мусорка его удаляет.
    if t.area != SlotArea::Trash
        && let Some(previous) = previous
    {
        return_drag_stack(craft, state, DragSource::Craft(index), previous);
    }
    inv.selected = None;
}

/// Завершает перетаскивание: стек возвращается в источник, затем
/// переносится в целевой слот (если он указан и отличен от исходного).
/// Для слота инвентаря — через `Inventory::transfer` (правила размещения
/// и свап), для клетки сетки — через `finish_craft_drag`.
fn finish_drag(
    commands: &mut Commands,
    inv: &mut InventoryUi,
    craft: &mut CraftingUi,
    state: &mut GameState,
    target: Option<SlotRef>,
) {
    let Some(drag) = inv.drag.take() else {
        return;
    };
    commands.entity(drag.ghost).despawn();
    match drag.from {
        DragSource::Slot(from) => {
            let Some(p) = state.controlled_mind_mut() else {
                return;
            };
            // Сначала возвращаем стек «в руку» в исходный слот: transfer берёт
            // его оттуда. Если целевой слот несовместим — стек останется на месте.
            p.inventory.put(from, drag.stack);
            if let Some(t) = target
                && t != from
                && p.inventory.transfer(from, t)
            {
                // Перенос завершён — выбор источника больше не нужен.
                inv.selected = None;
            }
        }
        DragSource::Craft(index) => {
            finish_craft_drag(inv, craft, state, index, drag.stack, target);
        }
    }
}

/// Отменяет перетаскивание (ПКМ): стек возвращается в источник, призрак
/// удаляется.
fn cancel_drag(
    commands: &mut Commands,
    inv: &mut InventoryUi,
    craft: &mut CraftingUi,
    state: &mut GameState,
) {
    let Some(drag) = inv.drag.take() else {
        return;
    };
    commands.entity(drag.ghost).despawn();
    return_drag_stack(craft, state, drag.from, drag.stack);
}
