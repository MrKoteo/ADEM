//! Поля мира справа на панели: размер мира и сид. Pending-текст живёт
//! в виджете до Enter; Escape откатывает ввод, без фокуса поле показывает
//! применённое значение. Применение перегенерирует мир целиком.

use bevy::input_focus::InputFocus;
use bevy::input_focus::tab_navigation::TabIndex;
use bevy::prelude::*;
use bevy::text::{EditableText, EditableTextFilter, TextCursorStyle, TextEdit, TextLayout};
use bevy::ui_widgets::TextInput;

use crate::app::generation::{WorldGeneration, request_world};
use crate::app::state::{ClockRes, GameState};
use crate::simulation::world::WorldSize;
use crate::ui::UiFont;
use crate::ui::style;

use super::{column_node, row_node};

/// Маркер поля ввода размера мира.
#[derive(Component)]
pub(crate) struct WorldSizeInput;

/// Маркер поля ввода сида мира.
#[derive(Component)]
pub(crate) struct WorldSeedInput;

/// Ссылка на текст подписи сида мира.
#[derive(Component)]
pub(crate) struct WorldSeedLabel;

/// Ссылка на текст подписи размера мира (показывает применённое значение).
#[derive(Component)]
pub(crate) struct WorldSizeLabel;

/// Спавнит столбец полей «Мир» и «Сид» (вызывает `spawn_bottom_panel`).
pub(crate) fn spawn_world_fields(
    panel: &mut ChildSpawnerCommands,
    state: &GameState,
    font: &UiFont,
) {
    panel.spawn(column_node()).with_children(|col| {
        col.spawn(row_node()).with_children(|size| {
            size.spawn((
                Text::new(format!("Мир: {}", state.world_size.label())),
                TextFont {
                    font: font.0.clone().into(),
                    font_size: 14.0.into(),
                    ..default()
                },
                TextColor(style::text()),
                WorldSizeLabel,
            ));

            let mut editable = EditableText::new(state.world_size.tiles().to_string());
            editable.max_characters = Some(5);
            size.spawn((
                Node {
                    width: Val::Px(76.0),
                    padding: UiRect::all(Val::Px(7.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(style::surface()),
                BorderColor::from(style::border()),
                editable,
                EditableTextFilter::new(|c| c.is_ascii_digit()),
                TextLayout::no_wrap(),
                TextCursorStyle::default(),
                TabIndex(0),
                TextInput,
                TextFont {
                    font: font.0.clone().into(),
                    font_size: 14.0.into(),
                    ..default()
                },
                TextColor(style::text()),
                WorldSizeInput,
            ));
        });

        col.spawn(row_node()).with_children(|seed_row| {
            seed_row.spawn((
                Text::new(format!("Сид: {}", state.seed)),
                TextFont {
                    font: font.0.clone().into(),
                    font_size: 14.0.into(),
                    ..default()
                },
                TextColor(style::text()),
                WorldSeedLabel,
            ));

            let mut editable = EditableText::new(state.seed.to_string());
            editable.max_characters = Some(20);
            seed_row.spawn((
                Node {
                    width: Val::Px(120.0),
                    padding: UiRect::all(Val::Px(7.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(style::surface()),
                BorderColor::from(style::border()),
                editable,
                EditableTextFilter::new(|c| c.is_ascii_digit()),
                TextLayout::no_wrap(),
                TextCursorStyle::default(),
                TabIndex(0),
                TextInput,
                TextFont {
                    font: font.0.clone().into(),
                    font_size: 14.0.into(),
                    ..default()
                },
                TextColor(style::text()),
                WorldSeedInput,
            ));
        });
    });
}

/// Поле ввода размера мира: pending-текст живёт в виджете до применения.
///
/// Enter — применить (значение зажимается в лимиты мира и округляется до
/// кратности чанку, мир перегенерируется в фоновом потоке). Escape — отменить
/// ввод и вернуть применённое значение. Без фокуса поле всегда показывает
/// применённый размер; во время генерации подпись добавляет «→ размер
/// (генерация…)».
pub(crate) fn handle_world_size_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<GameState>,
    mut generation: ResMut<WorldGeneration>,
    clock: Res<ClockRes>,
    focus: Res<InputFocus>,
    mut inputs: Query<(Entity, &mut EditableText), With<WorldSizeInput>>,
    mut size_label: Query<&mut Text, With<WorldSizeLabel>>,
) {
    // Подпись отражает применённый размер; во время генерации добавляется
    // индикатор, чтобы было видно, что запрос ещё строится в фоне.
    if state.is_changed() || generation.is_changed() {
        let mut want = format!("Мир: {}", state.world_size.label());
        if let Some(request) = generation.requested {
            want.push_str(&format!(" → {} (генерация…)", request.size.label()));
        }
        for t in &mut size_label {
            crate::ui::set_text_if_neq(t, &want);
        }
    }

    let Ok((entity, mut editable)) = inputs.single_mut() else {
        return;
    };
    handle_world_field(
        &keys,
        &focus,
        entity,
        &mut editable,
        state.world_size.tiles(),
        |text| text.parse::<u32>().ok(),
        |side| {
            let size = WorldSize::new(side);
            // Запрос уходит в фоновый поток; повторный Enter с теми же
            // параметрами не перезапускает карту, смена — отменяет уже
            // идущую генерацию (ARCHITECTURE 4.1). Сид берётся из pending:
            // если уже строится другой размер, ввод размера не должен
            // откатывать его на применённый старый сид.
            let seed = generation.pending_params(&state).seed;
            request_world(&mut state, &mut generation, size, seed, clock.0.tick);
            size.tiles().to_string()
        },
    );
}

/// Поле ввода сида мира: применяется по Enter.
pub(crate) fn handle_world_seed_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<GameState>,
    mut generation: ResMut<WorldGeneration>,
    clock: Res<ClockRes>,
    focus: Res<InputFocus>,
    mut inputs: Query<(Entity, &mut EditableText), With<WorldSeedInput>>,
    mut seed_label: Query<&mut Text, With<WorldSeedLabel>>,
) {
    if state.is_changed() || generation.is_changed() {
        let mut want = format!("Сид: {}", state.seed);
        if let Some(request) = generation.requested {
            want.push_str(&format!(" → {}", request.seed));
        }
        for t in &mut seed_label {
            crate::ui::set_text_if_neq(t, &want);
        }
    }

    let Ok((entity, mut editable)) = inputs.single_mut() else {
        return;
    };
    handle_world_field(
        &keys,
        &focus,
        entity,
        &mut editable,
        state.seed,
        |text| text.parse::<u64>().ok(),
        |seed| {
            let size = generation.pending_params(&state).size;
            request_world(&mut state, &mut generation, size, seed, clock.0.tick);
            seed.to_string()
        },
    );
}

/// Общий каркас полей ввода «размер мира»/«сид»: pending-текст живёт в
/// виджете до Enter, Escape откатывает, без фокуса поле показывает
/// применённое значение. Разные у полей только тип значения (`parse`) и
/// `apply`, который применяет его и возвращает новый текст для поля.
fn handle_world_field<T>(
    keys: &ButtonInput<KeyCode>,
    focus: &InputFocus,
    entity: Entity,
    editable: &mut EditableText,
    committed: T,
    parse: fn(&str) -> Option<T>,
    apply: impl FnOnce(T) -> String,
) where
    T: ToString,
{
    let committed = committed.to_string();
    if focus.get() != Some(entity) {
        // Отменённый/неприменённый ввод не сохраняем.
        if editable.value().to_string() != committed {
            reset_editable(editable, &committed);
        }
        return;
    }

    if keys.just_pressed(KeyCode::Enter) {
        match parse(editable.value().to_string().trim()) {
            // Пустое поле или превышение разрядности откатывает ввод.
            Some(value) => {
                let applied = apply(value);
                reset_editable(editable, &applied);
            }
            None => reset_editable(editable, &committed),
        }
    } else if keys.just_pressed(KeyCode::Escape) {
        reset_editable(editable, &committed);
    }
}

/// Сбрасывает текстовое поле на значение: pending-правки стираются,
/// курсор — в конец.
pub(crate) fn reset_editable(editable: &mut EditableText, value: &str) {
    editable.pending_edits.clear();
    editable.pending_paste = None;
    editable.editor_mut().set_text(value);
    // Курсор в конец — то же, что делает EditableText::new.
    editable.queue_edit(TextEdit::TextEnd(false));
}
