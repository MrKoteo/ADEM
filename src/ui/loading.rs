//! Экран загрузки нового мира: прогресс-бар в центре экрана.
//!
//! Старый мир на время генерации выгружается (экономия ОЗУ), поэтому
//! показывать больше нечего: по центру — название, полоса прогресса,
//! процент и оценка остатка по таблице прошлых замеров
//! (`app/generation_times.rs`).

use bevy::prelude::*;

use crate::app::generation::WorldGeneration;
use crate::app::generation_times::GenerationTimes;
use crate::app::state::GameState;
use crate::ui::{UiFont, set_text_if_neq, style};

/// Размер полосы прогресса, логические пиксели.
const BAR_WIDTH: f32 = 360.0;
const BAR_HEIGHT: f32 = 16.0;

/// Корень оверлея загрузки.
#[derive(Component)]
struct LoadingOverlay;

/// Заполненная часть полосы (ширина — проценты).
#[derive(Component)]
struct LoadingFill;

/// Подпись с процентом и оценкой остатка.
#[derive(Component)]
struct LoadingLabel;

pub struct LoadingPlugin;

impl Plugin for LoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_loading_overlay)
            .add_systems(Update, update_loading_overlay);
    }
}

/// Создаёт скрытый оверлей загрузки: он показывается только на время
/// генерации мира, поэтому в обычной игре не участвует в раскладке.
fn spawn_loading_overlay(mut commands: Commands, font: Res<UiFont>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            Visibility::Hidden,
            LoadingOverlay,
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(10.0),
                    padding: UiRect::all(Val::Px(20.0)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(style::background()),
            ))
            .with_children(|panel| {
                panel.spawn((
                    Text::new("Генерация мира"),
                    TextFont {
                        font: font.0.clone().into(),
                        font_size: 22.0.into(),
                        ..default()
                    },
                    TextColor(style::text()),
                ));
                // Внешняя полоса: тёмная подложка с рамкой.
                panel
                    .spawn((
                        Node {
                            width: Val::Px(BAR_WIDTH),
                            height: Val::Px(BAR_HEIGHT),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(style::surface()),
                        BorderColor::all(style::border()),
                    ))
                    .with_children(|bar| {
                        bar.spawn((
                            Node {
                                width: Val::Percent(0.0),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                            BackgroundColor(style::surface_active()),
                            LoadingFill,
                        ));
                    });
                panel.spawn((
                    Text::new("0 %"),
                    TextFont {
                        font: font.0.clone().into(),
                        font_size: 15.0.into(),
                        ..default()
                    },
                    TextColor(style::text_dim()),
                    LoadingLabel,
                ));
            });
        });
}

/// Показывает оверлей во время генерации и обновляет полосу/подпись.
/// Текст переписывается только при изменении: раз в кадр `format!` +
/// переразметка глифов не нужны.
fn update_loading_overlay(
    state: Res<GameState>,
    generation: Res<WorldGeneration>,
    times: Res<GenerationTimes>,
    mut overlay: Query<&mut Visibility, With<LoadingOverlay>>,
    mut fill: Query<&mut Node, With<LoadingFill>>,
    mut label: Query<&mut Text, With<LoadingLabel>>,
) {
    let visible = state.generating;
    for mut visibility in &mut overlay {
        let want = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != want {
            *visibility = want;
        }
    }
    if !visible {
        return;
    }

    let progress = generation.progress.clamp(0.0, 1.0);
    let percent = (progress * 100.0).round() as u32;
    let want = match times.estimate_secs(generation.pending_params(&state).size.tiles()) {
        // Оценка — полное время генерации; остаток считаем по часам, а не
        // по проценту: поток отчитывается этапами, и процент «замирает»
        // внутри долгого прохода.
        Some(estimate) => {
            let left = (estimate - generation.elapsed_secs()).max(0.0);
            format!("{percent} % · осталось ~{} с", left.ceil() as u32)
        }
        None => format!("{percent} %"),
    };
    for text in &mut label {
        set_text_if_neq(text, &want);
    }
    for mut node in &mut fill {
        let width = Val::Percent(progress * 100.0);
        if node.width != width {
            node.width = width;
        }
    }
}
