//! Общие правила движения существ по миру.
//!
//! Базовая скорость ходьбы — параметр самого существа (у вида — в
//! [`EntityKind::walk_speed`](super::entities::EntityKind::walk_speed), у игрока —
//! `PLAYER_WALK_SPEED`). А модификаторы рельефа и проходимость тайлов —
//! правило мира: они применяются одинаково к игроку и к NPC. Новые
//! модификаторы (грязь, снег, густая растительность и т. п.) и запреты
//! движения добавляются здесь, чтобы у игрока и существ не появилось двух
//! расходящихся реализаций движения.

use super::tile::TileType;

/// Замедление на мелководье: движение вплавь втрое медленнее.
pub const WADE_SLOWDOWN: f32 = 3.0;

/// Замедление на горе: подъём вдвое медленнее.
pub const MOUNTAIN_SLOWDOWN: f32 = 2.0;

/// Замедление на грязи: вязкая почва вдвое медленнее.
pub const MUD_SLOWDOWN: f32 = 2.0;

/// Непроходим ли рельеф. `None` (вне мира) — суша.
pub fn blocks_movement(terrain: Option<TileType>) -> bool {
    matches!(terrain, Some(TileType::Lava))
}

/// Можно ли ступить с рельефа `from` на `to`. Непроходимый тайл можно
/// покинуть: существо, поставленное туда редактором, не застревает.
pub fn can_enter(from: Option<TileType>, to: Option<TileType>) -> bool {
    !blocks_movement(to) || blocks_movement(from)
}

/// Множитель скорости для рельефа: `1.0` — без замедления,
/// `1 / WADE_SLOWDOWN` — вброд, `1 / MOUNTAIN_SLOWDOWN` — подъём в гору.
/// `None` (позиция вне мира) замедления не даёт.
pub fn terrain_speed_multiplier(terrain: Option<TileType>) -> f32 {
    match terrain {
        Some(TileType::Ocean | TileType::Water | TileType::DeepWater | TileType::River) => {
            1.0 / WADE_SLOWDOWN
        }
        Some(TileType::Mountain) => 1.0 / MOUNTAIN_SLOWDOWN,
        Some(TileType::Mud) => 1.0 / MUD_SLOWDOWN,
        _ => 1.0,
    }
}

/// Скорость ходьбы существа с учётом рельефа под ним, тайлов за тик.
/// `base_speed` — индивидуальная скорость существа (вид, травмы, груз).
pub fn effective_walk_speed(base_speed: f32, terrain: Option<TileType>) -> f32 {
    base_speed * terrain_speed_multiplier(terrain)
}
