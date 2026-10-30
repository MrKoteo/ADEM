//! Настройка и запуск Bevy-приложения.

pub mod audio;
pub mod camera;
pub mod input;
pub mod plugin;
pub mod saves;
pub mod settings;
pub mod state;
pub mod window_icon;

use bevy::prelude::*;
use bevy::window::WindowResolution;

pub const GAME_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run() {
    // Настройки читаются до старта плагинов: первый кадр уже должен
    // видеть режим окна, громкости и лимит FPS из settings.json.
    let settings = settings::Settings::load(&settings::settings_path());
    App::new()
        .insert_resource(settings)
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: format!("Aevum del Mundi v{GAME_VERSION}"),
                        resolution: WindowResolution::new(1280, 720),
                        resizable: true,
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins(plugin::AdemPlugin)
        .run();
}
