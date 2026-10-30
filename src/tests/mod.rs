//! Тесты на ключевые правила мира, разбитые по доменам (по одному модулю
//! на область: генерация, чанки, планировщик, рендер, сохранения, игрок,
//! растительность, существа, крафт).
//! Включаются только в `cargo test` (через `#[cfg(test)] mod tests` в main.rs).

mod armor;
mod build;
mod clothes;
mod climate;
mod crafts;
mod entities;
mod generation;
mod player;
mod rendering;
mod save;
mod time_scheduler;
mod vegetation;
mod world;
