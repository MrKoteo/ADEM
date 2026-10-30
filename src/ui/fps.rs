//! FPS-счётчик, время тика и игровая дата в левом верхнем углу,
//! обновляются раз в 0.5 секунды (GAME_DESIGN 9).

use bevy::prelude::*;

use crate::app::state::{ClockRes, GameState, TickTiming};
use crate::ui::style;
use crate::ui::UiFont;

pub struct FpsPlugin;

impl Plugin for FpsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FpsCounter::new())
            .add_systems(Startup, spawn_fps_text)
            .add_systems(Update, update_fps);
    }
}

#[derive(Resource)]
struct FpsCounter {
    frames: u32,
    timer: Timer,
    current_fps: f32,
}

impl FpsCounter {
    fn new() -> Self {
        Self {
            frames: 0,
            timer: Timer::from_seconds(0.5, TimerMode::Repeating),
            current_fps: 0.0,
        }
    }
}

#[derive(Component)]
struct FpsText;

/// Создаёт текстовый узел FPS в верхнем левом углу.
fn spawn_fps_text(mut commands: Commands, font: Res<UiFont>) {
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(style::background()),
        children![(
            Text::new("FPS: --"),
            TextFont {
                font: font.0.clone().into(),
                font_size: 14.0.into(),
                ..default()
            },
            TextColor(style::text()),
            FpsText,
        )],
    ));
}

/// Считает кадры и раз в 0.5 с обновляет текст: FPS, время тика, игровая дата
/// и температура в точке, где стоит игрок (пока — заглушка 24 °C из мира).
fn update_fps(
    time: Res<Time>,
    mut fps: ResMut<FpsCounter>,
    clock: Res<ClockRes>,
    timing: Res<TickTiming>,
    state: Res<GameState>,
    mut text: Query<&mut Text, With<FpsText>>,
) {
    fps.frames = fps.frames.saturating_add(1);
    if fps.timer.tick(time.delta()).just_finished() {
        let secs = fps.timer.duration().as_secs_f32();
        fps.current_fps = fps.frames as f32 / secs;
        fps.frames = 0;

        let clock = &clock.0;
        // Календарные величины отображаются с единицы: в симуляции они с нуля.
        // Время тика показываем только после первого исполненного тика.
        let tick_ms = if timing.avg_ms > 0.0 {
            format!("   тик: {:.2} мс", timing.avg_ms)
        } else {
            String::new()
        };
        // Температура в месте игрока: пока это общая заглушка мира
        // (ambient_temperature_c); с погодой значение станет локальным.
        let temperature = state
            .world
            .as_ref()
            .map(|world| format!("{:.1} °C", world.ambient_temperature_c))
            .unwrap_or_else(|| "--".to_string());
        for mut t in &mut text {
            **t = format!(
                "FPS: {:.1}{tick_ms}\nГод {}, {}, день {}, {:02}:{:02}\nТемпература: {temperature}",
                fps.current_fps,
                clock.year() + 1,
                clock.season().label(),
                clock.day_of_month() + 1,
                clock.hour_of_day(),
                clock.minute_of_hour(),
            );
        }
    }
}
