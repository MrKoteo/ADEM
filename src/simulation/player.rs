//! Игрок как существо в мире: позиция в непрерывных координатах, движение
//! по тикам, инвентарь и взаимодействие с тайлами (добыча).
//!
//! Данные игрока живут вне мира (в Bevy-ресурсе app-слоя): режим наблюдателя
//! просто «паркует» игрока, при повторном входе инвентарь сохраняется.
//! Правила движения и добычи — здесь, чтобы остались в симуляции и были
//! покрыты тестами.

use super::hunger::{Activity, Hunger, APPLE_FOOD_VALUE};
use super::items::{Inventory, ItemKind, ItemStack, MISC_SLOTS};
use super::objects::{ObjectKind, WorldObject};
use super::tile::TileType;
use super::world::{TilePos, World};

/// Скорость ходьбы, тайлов за тик (GAME_DESIGN 3.4: 0.2 тайла/тик).
const WALK_SPEED: f32 = 0.2;

/// Замедление на мелководье: движение вплавь втрое медленнее.
const WADE_SLOWDOWN: f32 = 3.0;

/// Замедление на горе: подъём вдвое медленнее.
const MOUNTAIN_SLOWDOWN: f32 = 2.0;

/// Радиус взаимодействия с тайлом: от игрока до центра тайла, в тайлах.
pub const INTERACT_REACH: f32 = 2.0;

/// Максимальный запас здоровья игрока.
pub const PLAYER_MAX_HP: f32 = 100.0;

/// Сколько пучков травы даёт один травяной тайл: единица, как и у камня.
const GRASS_YIELD_PER_TILE: u32 = 1;

/// Позиция в непрерывных координатах тайлов. Сущности не привязаны к сетке
/// (GAME_DESIGN 3.2); свой тип вместо Bevy `Vec2` — в симуляции нет Bevy.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct WorldPos {
    pub x: f32,
    pub y: f32,
}

impl WorldPos {
    /// Позиция в центре тайла.
    pub fn of_tile_center(pos: TilePos) -> Self {
        Self {
            x: pos.x as f32 + 0.5,
            y: pos.y as f32 + 0.5,
        }
    }

    /// Расстояние до другой позиции, тайлов.
    pub fn dist(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

/// Данные управляемого существа. Общая система существ появится позже (ROADMAP).
pub struct Player {
    /// Позиция в тайлах (непрерывная).
    pub pos: WorldPos,
    /// Позиция на предыдущем тике: рендер интерполирует между ней и `pos`,
    /// чтобы движение выглядело плавным при 20 тиках в секунду.
    pub prev_pos: WorldPos,
    /// Инвентарь разумного существа: 8 предметов + 4 брони + 4 косметики.
    pub inventory: Inventory,
    /// Выбранный слот хотбара (индекс в секции misc), 0..MISC_SLOTS.
    pub selected_slot: usize,
    /// Текущее здоровье; максимум — `PLAYER_MAX_HP`.
    pub hp: f32,
    /// Потребность голода (общая система, simulation/hunger.rs). Урон от
    /// голода применяется к `hp` на тике в `tick_hunger`.
    pub hunger: Hunger,
    /// Тик, с которого снова можно есть (кулдаун поедания, общая константа
    /// `EAT_COOLDOWN_TICKS`). Transient: игрок не сохраняется — поле не
    /// входит в сейв.
    pub eat_ready_tick: u64,
    /// Тик, с которого снова доступна атака в ближнем бою (кулдаун оружия).
    /// Атака без попадания кулдаун не списывает, поэтому поле просто
    /// хранит границу. Игрок не сохраняется — поле не входит в сейв.
    pub attack_ready_tick: u64,
}

impl Player {
    /// Новый игрок с пустым инвентарём в позиции `pos`.
    pub fn new(pos: WorldPos) -> Self {
        Self {
            pos,
            prev_pos: pos,
            inventory: Inventory::default(),
            selected_slot: 0,
            hp: PLAYER_MAX_HP,
            hunger: Hunger::new(),
            eat_ready_tick: 0,
            attack_ready_tick: 0,
        }
    }

    /// Предмет в выбранном слоте хотбара, если там что-то лежит.
    pub fn selected_item(&self) -> Option<ItemKind> {
        let slot = self.selected_slot.min(MISC_SLOTS - 1);
        self.inventory.items[slot].map(|stack| stack.kind)
    }

    /// Съедает одну единицу предмета из выбранного слота хотбара и
    /// восстанавливает сытость. `false` — в руке не еда (пока еда только
    /// яблоко). Есть можно и при полной сытости: излишек пропадает.
    /// Кулдаун поедания проверяет команда `PlayerEat` (commands.rs).
    pub fn eat_selected(&mut self) -> bool {
        let slot = self.selected_slot.min(MISC_SLOTS - 1);
        let Some(stack) = self.inventory.items[slot] else {
            return false;
        };
        if stack.kind != ItemKind::Apple {
            return false;
        }
        if stack.count == 1 {
            self.inventory.items[slot] = None;
        } else {
            self.inventory.items[slot] = Some(ItemStack { count: stack.count - 1, ..stack });
        }
        self.hunger.eat(APPLE_FOOD_VALUE);
        true
    }

    /// Тик потребности голода: списывает сытость по активности и применяет
    /// урон от голода к здоровью. Вызывается на каждом тике симуляции в
    /// режиме игрока (игрок один, O(1) на тик — планировщик не нужен).
    ///
    /// Активность: плавание (тайл воды под ногами) тратит больше ходьбы,
    /// ходьба — больше стояния. `moving` — было ли направление движения
    /// ненулевым на этом тике.
    pub fn tick_hunger(&mut self, moving: bool, world: &World) {
        let swimming = matches!(
            Self::terrain_at(self.pos, world),
            Some(TileType::Water | TileType::DeepWater | TileType::Ocean)
        );
        let activity = if swimming {
            Activity::Swimming
        } else if moving {
            Activity::Walking
        } else {
            Activity::Idle
        };
        let damage = self.hunger.tick(activity);
        if damage > 0.0 {
            // Смерть (hp <= 0) фиксирует app-слой.
            self.hp -= damage;
        }
    }
    /// Мгновенное перемещение (размещение «богом», зажим к границе мира).
    /// Прошлая позиция сбрасывается, чтобы рендер не тянул спрайт через
    /// всю карту от старого места.
    pub fn teleport_to(&mut self, pos: WorldPos) {
        self.pos = pos;
        self.prev_pos = pos;
    }

    /// Шаг движения за один тик. Направление `dir` — ввод игрока (ось X
    /// вправо, ось Y вверх); нормализуется, чтобы диагональ не была быстрее.
    ///
    /// По осям движение проверяется отдельно: упираясь в глубокую воду по
    /// одной оси, игрок скользит вдоль берега по другой. Вброд проходимо
    /// только мелководье: средневодная и океаническая глубина с мелководья
    /// недоступны (иначе вплавь уходил бы через любую глубину). Исключение
    /// — игрок, уже стоящий в глубине (размещённый «богом» на океане):
    /// ему движение разрешено, чтобы он мог выбраться на берег.
    /// Скорость зависит от тайла под ногами: по мелководью — втрое
    /// медленнее, по горе — вдвое.
    pub fn step(&mut self, dir: (f32, f32), world: &World) {
        let len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt();
        if len <= f32::EPSILON {
            // Стоим на месте: сегмент интерполяции схлопывается, иначе спрайт каждый тик «доезжал» бы последний сделанный шаг.
            self.prev_pos = self.pos;
            return;
        }
        // Запоминаем позицию до шага: интерполяция рендера идёт между прошлым и текущим тиком.
        self.prev_pos = self.pos;

        let max = world.size.tiles() as f32;
        let on_deep = matches!(
            Self::terrain_at(self.pos, world),
            Some(TileType::DeepWater | TileType::Ocean)
        );
        let slowdown = match Self::terrain_at(self.pos, world) {
            Some(TileType::Water) => WADE_SLOWDOWN,
            Some(TileType::Mountain) => MOUNTAIN_SLOWDOWN,
            _ => 1.0,
        };
        let speed = WALK_SPEED / slowdown;
        let dx = dir.0 / len * speed;
        let dy = dir.1 / len * speed;

        let try_x = WorldPos {
            x: (self.pos.x + dx).clamp(0.0, max),
            y: self.pos.y,
        };
        if Self::passable(try_x, world, on_deep) {
            self.pos = try_x;
        }
        let try_y = WorldPos {
            x: self.pos.x,
            y: (self.pos.y + dy).clamp(0.0, max),
        };
        if Self::passable(try_y, world, on_deep) {
            self.pos = try_y;
        }
    }

    /// Взаимодействие с тайлом (добыча). `false` — действие невозможно:
    /// тайл вне мира, дальше радиуса, объект руками не добывается или
    /// в инвентаре нет места.
    ///
    /// Порядок: объект на тайле (куст, валун) добывается как раньше;
    /// пустой травяной тайл даёт пучок травы пустой рукой или ножом.
    pub fn interact(&mut self, pos: TilePos, world: &mut World) -> bool {
        if !self.reachable(pos, world) {
            return false;
        }
        if let Some(obj) = world.object_at(pos).copied() {
            return self.harvest_object(pos, obj, world);
        }
        self.gather_grass(pos, world)
    }

    /// Сбор яблок с яблони (ПКМ). За одно действие дерево отдаёт одно
    /// яблоко, как валун — один камень. Последнее яблоко оставляет дерево
    /// пустым навсегда: `charge = 0` и текстура `apple_tree_empty`.
    /// `false` — не яблоня, нет плодов, далеко или в инвентаре нет места.
    pub fn harvest_apple(&mut self, pos: TilePos, world: &mut World) -> bool {
        if !self.reachable(pos, world) {
            return false;
        }
        let Some(obj) = world.object_at(pos).copied() else {
            return false;
        };
        let Some(amount) = obj.apple_yield() else {
            return false;
        };
        if !self.inventory.can_fit(ItemKind::Apple, 1) {
            return false;
        }
        self.inventory.add(ItemKind::Apple, 1);

        // Плодов на дереве на единицу меньше; пустая яблоня остаётся объектом.
        let mut tree = obj;
        tree.charge = amount - 1;
        world.set_object(pos, Some(tree));
        true
    }

    /// Добыча объекта над тайлом (куст, валун). Отдельный метод, чтобы
    /// `interact` оставался коротким и не смешивал правила объектов и травы.
    fn harvest_object(&mut self, pos: TilePos, obj: WorldObject, world: &mut World) -> bool {
        let Some((resource, amount)) = obj.harvest_yield() else {
            return false;
        };
        // Добыча камня требует палку или камень в руке (выбранный слот хотбара).
        if obj.kind == ObjectKind::Rock {
            let selected = self.inventory.items[self.selected_slot.min(MISC_SLOTS - 1)];
            let ok = matches!(selected.map(|s| s.kind), Some(ItemKind::Stick) | Some(ItemKind::Rock));
            if !ok {
                return false;
            }
        }
        let Some(item) = ItemKind::from_resource(resource) else {
            return false;
        };
        if !self.inventory.can_fit(item, u32::from(amount)) {
            return false;
        }
        self.inventory.add(item, u32::from(amount));

        // Применяем добычу к объекту: куст ломается, у валуна падает запас.
        match obj.kind {
            ObjectKind::Bush => world.set_object(pos, None),
            ObjectKind::Rock => {
                let mut rock = obj;
                rock.charge = rock.charge.saturating_sub(amount);
                world.set_object(pos, (rock.charge > 0).then_some(rock));
            }
            // Остальные объекты руками не добываются (harvest_yield = None).
            _ => return false,
        }
        true
    }

    /// Сбор травы пустой рукой или ножом: тайл даёт один пучок и
    /// превращается в землю. Другие предметы в руке траву не берут.
    fn gather_grass(&mut self, pos: TilePos, world: &mut World) -> bool {
        if !matches!(
            world.terrain_kind(pos),
            TileType::Grass | TileType::ForestGrass
        ) {
            return false;
        }
        let selected = self.inventory.items[self.selected_slot.min(MISC_SLOTS - 1)];
        let allowed = matches!(selected.map(|s| s.kind), None | Some(ItemKind::Knife));
        if !allowed {
            return false;
        }
        let amount = GRASS_YIELD_PER_TILE;
        if !self.inventory.can_fit(ItemKind::GrassBunch, amount) {
            return false;
        }
        self.inventory.add(ItemKind::GrassBunch, amount);
        world.set_terrain(pos, TileType::Dirt);
        true
    }

    /// Тайл в пределах мира и радиуса взаимодействия от игрока. Публичный:
    /// правило «радиус как у добычи» применяется и к другим действиям
    /// игрока (например, к выкладыванию предмета на землю).
    pub fn reachable(&self, pos: TilePos, world: &World) -> bool {
        let size = world.size.tiles() as i32;
        if pos.x < 0 || pos.y < 0 || pos.x >= size || pos.y >= size {
            return false;
        }
        let center = WorldPos::of_tile_center(pos);
        let dx = center.x - self.pos.x;
        let dy = center.y - self.pos.y;
        (dx * dx + dy * dy).sqrt() <= INTERACT_REACH
    }

    /// Проходим ли тайл для движения. Глубокая вода и океан не проходимы —
    /// ни с суши, ни вплавь с мелководья. Исключение — игрок, уже стоящий
    /// в глубине: «бог» может разместить его в океане, и движение
    /// позволяет ему выбраться на берег. Вне мира — суша.
    fn passable(pos: WorldPos, world: &World, on_deep_water: bool) -> bool {
        !matches!(
            Self::terrain_at(pos, world),
            Some(TileType::DeepWater | TileType::Ocean) if !on_deep_water
        )
    }

    /// Вид рельефа под позицией. `None` — вне мира (считается сушей:
    /// clamp в `step` не даёт выйти за границы, а для тестов это честный
    /// «не вода»).
    fn terrain_at(pos: WorldPos, world: &World) -> Option<TileType> {
        let size = world.size.tiles() as i32;
        let tx = pos.x.floor() as i32;
        let ty = pos.y.floor() as i32;
        if tx < 0 || ty < 0 || tx >= size || ty >= size {
            return None;
        }
        Some(world.terrain_kind(TilePos { x: tx, y: ty }))
    }
}
