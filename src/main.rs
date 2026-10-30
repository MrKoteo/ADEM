//! Точка входа. Запуск Bevy-приложения ADEM.

mod app;
mod rendering;
mod simulation;
mod ui;

#[cfg(test)]
mod tests;

fn main() {
    app::run();
}