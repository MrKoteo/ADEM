//! Автономное поведение людей (ИИ выживания).
//!
//! Объём этой итерации — «выживание» (решение автора): голод → еда
//! (свой инвентарь → яблоня/зрелая культура → охота), ночь → сбор у костра.
//! Полный набор возможностей (добыча, крафт, строительство) доступен
//! человеку при захвате контроллером — через те же функции `actions`,
//! которыми управляется и захваченное существо.
//!
//! Архитектура: движение и воды-паника — общий FSM существ
//! (`entities.rs`), ИИ только решает, куда идти и что делать, выставляя
//! состояние/цель существа. Решения детерминированы: RNG события сеётся
//! от (seed, id, тик) — `decision_rng`, состояние не сохраняется.

use super::combat;
use super::commands;
use super::containers::{self, Container};
use super::craft_planner::{missing_material, plan_craft};
use super::crafts::CrafterKind;
use super::crops::CropKind;
use super::entities::{EntityKind, EntityState, HumanGoal, SocialRole};
use super::hunger::EAT_COOLDOWN_TICKS;
use super::items::{ItemKind, ItemStack, MISC_SLOTS, ToolClass};
use super::knowledge::KnowledgeSet;
use super::objects::{BuildingKind, ObjectKind};
use super::rng::{SplitMix64, mix_seed};
use super::settlement::SETTLEMENT_RADIUS_TILES;
use super::tile::TileType;
use super::time::{self, GAME_SECS_PER_TICK, SECS_PER_HOUR, TICKS_PER_SEC};
use super::world::{World, WorldPos};

/// Час начала ночи: люди возвращаются к костру.
const NIGHT_START_HOUR: u32 = 21;
/// Час конца ночи: люди расходятся по своим делам.
const NIGHT_END_HOUR: u32 = 6;

/// Радиус поиска костра, тайлов. Небольшой квадрат по замыслу «ходить и
/// искать»: найдя костёр, человек запоминает его в `known_campfire` и
/// больше не сканирует (пока костёр на месте).
const CAMPFIRE_SEARCH_TILES: f32 = 12.0;
/// Радиус поиска еды, тайлов (найденное запоминается в `known_food`).
const FOOD_SEARCH_TILES: f32 = 10.0;

/// Порог голода: ниже человек начинает искать еду.
const HUNGRY_THRESHOLD: f32 = 40.0;
/// Порог поедания из инвентаря: ниже человек ест запасы (с запасом выше
/// порога поиска еды, чтобы утолить голод до её добычи).
const EAT_WHEN_BELOW: f32 = 60.0;
/// Порог пополнения запасов: сытый человек днём может идти за едой впрок,
/// пока сытость не близка к максимуму.
const STOCKPILE_THRESHOLD: f32 = 85.0;

/// Дистанция «дошёл до костра», тайлов (встать в кольце вокруг огня).
const CAMPFIRE_ARRIVE_TILES: f32 = 1.4;
/// Дистанция атаки на охоте, тайлов (руки/нож достают).
const HUNT_ATTACK_TILES: f32 = 1.3;

/// Интервал «обдумывания» ИИ, тиков: переоценка цели и скан окрестностей
/// не чаще раза в 5 секунд (при x1). Между обдумываниями ИИ только
/// исполняет текущую цель — сканы объектов не совершаются.
const THINK_INTERVAL_TICKS: u64 = 5 * TICKS_PER_SEC as u64;

/// Раз в игровой час человек может поделиться одним знанием с соседом
/// у костра (или перенять его).
const SHARE_INTERVAL_TICKS: u64 = SECS_PER_HOUR / GAME_SECS_PER_TICK as u64;
/// Целочисленный радиус для поиска построек (2 чанка).
const SETTLEMENT_SEARCH_TILES: i32 = SETTLEMENT_RADIUS_TILES as i32;
/// Обмен знаниями доступен членам одного поселения в его радиусе,
/// независимо от расстояния между людьми.
const SHARE_SETTLEMENT_RADIUS: f32 = SETTLEMENT_RADIUS_TILES;
/// Радиус поиска склада вокруг костра/человека.
const STORAGE_SEARCH_TILES: i32 = 12;
/// Сколько готовой еды брать со склада «с запасом».
const FOOD_TAKE_COUNT: u32 = 3;

/// Ночь ли сейчас по тику (часы считаются так же, как в календаре).
fn is_night(tick: u64) -> bool {
    let hour = time::hour_of_tick(tick);
    // Диапазон ночи «переходит через полночь»: 21:00..06:00.
    !(NIGHT_END_HOUR..NIGHT_START_HOUR).contains(&hour)
}

impl World {
    /// Продвигает автономное поведение людей на один тик. Вызывается из
    /// `tick_entities` после общего FSM-тикания: ИИ только выставляет
    /// состояния и цели, движение делает общий код существ. Голод тикает
    /// в общем FSM (`EntityStore::update`), смерти от голода — там же.
    ///
    /// `controlled` — захваченное существо: его поведение — контроллер,
    /// ИИ его не трогает.
    pub(crate) fn tick_human_ai(&mut self, controlled: Option<u64>, tick: u64) {
        // Снимок id автономных людей: мутации внутри цикла не ломают обход.
        // Буфер переиспользуется между тиками (тысячи людей каждый тик).
        let mut ids = std::mem::take(&mut self.ai_scratch.human_ids);
        ids.clear();
        for entity in self.entities.iter() {
            if entity.kind == EntityKind::Human {
                ids.push(entity.id);
            }
        }
        if ids.is_empty() {
            self.ai_scratch.human_ids = ids;
            return;
        }
        let night = is_night(tick);

        for &id in &ids {
            // В воде ИИ молчит: работает паника/выход на сушу общего FSM.
            let swimming = self.entities.get(id).is_some_and(|e| {
                super::actions::terrain_at(self, e.pos).is_some_and(TileType::is_water)
            });
            if swimming {
                continue;
            }
            // Знания изучаются в фоне, в том числе захваченным человеком.
            // Для управляемого субъекта это единственная часть ИИ.
            self.human_knowledge_tick(id, tick);
            if Some(id) == controlled {
                continue;
            }
            self.human_tick(id, tick, night);
        }
        self.ai_scratch.human_ids = ids;
    }

    /// Завершение изучения, обмен знаниями у костра и постановка
    /// следующей технологии в «фоновую учёбу». Дёшево: вызывается каждый
    /// тик, но обычно только сравнивает два числа.
    fn human_knowledge_tick(&mut self, id: u64, tick: u64) {
        // 1. Завершение текущего изучения. Планировщик тоже умеет
        // завершать, поэтому операция идемпотентна.
        let due = {
            let Some(m) = self.entities.get(id).and_then(|e| e.mind.as_ref()) else {
                return;
            };
            m.studying
                .filter(|_| tick >= m.study_finish_tick)
        };
        if let Some(index) = due
            && let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut())
        {
            m.knowledge.insert(index);
            m.studying = None;
            m.study_finish_tick = 0;
        }

        // 2. Обмен знанием у костра. Только если человек уже знает костёр
        // (иначе не сканируем окрестности каждый тик) и кулдаун истёк.
        let share_due = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .is_some_and(|m| {
                tick >= m.share_ready_tick
                    && m.known_campfire.is_some()
                    && !m.knowledge.is_empty()
            });
        if share_due {
            let in_settlement = self.in_settlement(id);
            if in_settlement {
                if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                    m.share_ready_tick = tick + SHARE_INTERVAL_TICKS;
                }
                self.share_knowledge(id);
            }
        }

        // 3. Выбрать следующее доступное знание. Предпосылки должны быть
        // изучены; время — случайное 0.25..4.0 от расчётного, один раз.
        // Решение принимается не каждый тик, а раз в 5 секунд, со сдвигом
        // по id — так дорогой перебор каталога размазан по тикам.
        if tick % THINK_INTERVAL_TICKS != id % THINK_INTERVAL_TICKS {
            return;
        }
        let (studying, known, is_empty) = {
            let Some(m) = self.entities.get(id).and_then(|e| e.mind.as_ref()) else {
                return;
            };
            (m.studying, m.knowledge, self.knowledge.is_empty())
        };
        if studying.is_some() || is_empty {
            return;
        }
        let Some(index) = self.next_study(&known) else {
            return;
        };
        let base = self.knowledge.base_ticks(index);
        if base == 0 {
            return;
        }
        let mut rng = wander_rng(self.seed, id, tick);
        let factor = 0.25 + rng.next_f32() * 3.75;
        let duration = ((base as f64) * (factor as f64)).round().max(1.0) as u64;
        let finish = tick + duration;
        if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
            m.studying = Some(index);
            m.study_finish_tick = finish;
        }
        self.ai_scratch.study_requests.push((finish, id, index));
    }

    fn next_study(&self, known: &KnowledgeSet) -> Option<u16> {
        let ready = |index: u16| {
            !known.has(index)
                && self
                    .knowledge
                    .get(index)
                    .is_some_and(|info| info.study_hours > 0.0)
                && self.knowledge.requirements_met(index, known)
        };
        // Сначала базовые знания, без которых ИИ не может крафтить и
        // готовить. Дальше — всё остальное по стабильному порядку id.
        for id in [
            "stone_knapping",
            "stone_tools",
            "fire_control",
            "basic_building",
            "cordage",
            "wood_working",
        ] {
            if let Some(index) = self.knowledge.index_of(id) && ready(index) {
                return Some(index);
            }
        }
        (0..self.knowledge.len() as u16).find(|&index| ready(index))
    }

    /// Возвращает поселение человека, создавая/восстанавливая его по
    /// запомненному костру. Поселение — отдельная сущность с жителями и
    /// общим складом; костёр является его центром.
    fn ensure_settlement(&mut self, id: u64) -> Option<u64> {
        let fire = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.known_campfire)?;
        if !self
            .object_at(fire)
            .is_some_and(|o| o.kind == ObjectKind::Building(BuildingKind::Campfire))
        {
            return None;
        }
        let settlement = if let Some(existing) = self.settlements.at_center(fire) {
            existing.id
        } else {
            self.settlements.create(fire)
        };
        let known = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .map(|m| m.knowledge);
        if let Some(settlement) = self.settlements.get_mut(settlement) {
            settlement.add_member(id);
            if let Some(known) = known {
                known.for_each(|index| {
                    settlement.shared_knowledge.insert(index);
                });
            }
        }
        Some(settlement)
    }

    /// Человек находится в своём поселении: помнит костёр и не ушёл
    /// дальше двух чанков от него. Это не близость к другому человеку.
    fn in_settlement(&mut self, id: u64) -> bool {
        let Some(entity) = self.entities.get(id) else {
            return false;
        };
        let Some(fire) = entity.mind.as_ref().and_then(|m| m.known_campfire) else {
            return false;
        };
        let pos = entity.pos;
        self.settlements
            .at_center(fire)
            .is_some_and(|s| s.contains(pos))
            || (self.ensure_settlement(id).is_some()
                && pos.dist(WorldPos::of_tile_center(fire)) <= SHARE_SETTLEMENT_RADIUS)
    }

    /// Передать одно известное знание ближайшему жителю того же поселения.
    fn share_knowledge(&mut self, id: u64) {
        let Some(settlement_id) = self.ensure_settlement(id) else {
            return;
        };
        let Some(fire) = self.settlements.get(settlement_id).map(|s| s.center) else {
            return;
        };
        // Люди, помнящие тот же костёр, — жители поселения, даже если
        // ещё ни разу не обменивались знаниями. Переселяем их при обмене,
        // а не каждый тик.
        let candidates: Vec<u64> = self
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .mind
                    .as_ref()
                    .is_some_and(|m| m.known_campfire == Some(fire))
            })
            .map(|entity| entity.id)
            .collect();
        if let Some(settlement) = self.settlements.get_mut(settlement_id) {
            for member in candidates {
                settlement.add_member(member);
            }
        }
        let (members, fire_pos) = {
            let Some(settlement) = self.settlements.get(settlement_id) else {
                return;
            };
            (settlement.members.clone(), settlement.center_pos())
        };
        let Some(source) = self.entities.get(id) else {
            return;
        };
        let Some(mind) = source.mind.as_ref() else {
            return;
        };
        let origin = source.pos;
        if origin.dist(fire_pos) > SHARE_SETTLEMENT_RADIUS {
            return;
        }

        let mut best: Option<(f32, u64)> = None;
        for &member in &members {
            if member == id {
                continue;
            }
            let Some(entity) = self.entities.get(member) else {
                continue;
            };
            if entity.mind.is_none() || entity.pos.dist(fire_pos) > SHARE_SETTLEMENT_RADIUS {
                continue;
            }
            let dist = origin.dist(entity.pos);
            if best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, member));
            }
        }
        let Some((_, recipient)) = best else {
            return;
        };

        // Ищем первое наше знание, которого нет у соседа. Перебираем по
        // порядку битов — детерминированно.
        let mut gift = None;
        mind.knowledge.for_each(|index| {
            if gift.is_none()
                && !self
                    .entities
                    .get(recipient)
                    .and_then(|e| e.mind.as_ref())
                    .is_some_and(|m| m.knowledge.has(index))
            {
                gift = Some(index);
            }
        });
        if let Some(index) = gift {
            if let Some(other) = self.entities.get_mut(recipient).and_then(|e| e.mind.as_mut()) {
                other.knowledge.insert(index);
            }
            if let Some(settlement) = self.settlements.get_mut(settlement_id) {
                settlement.shared_knowledge.insert(index);
            }
        }
    }

    /// Начинает (или меняет) изучение по команде из интерактивного дерева.
    /// Предпосылки и наличие карточки проверяются здесь же; длительность
    /// выбирается детерминированно от (seed, id, tick).
    pub(crate) fn start_study(&mut self, id: u64, knowledge: u16, tick: u64) -> bool {
        let Some(info) = self.knowledge.get(knowledge) else {
            return false;
        };
        if info.study_hours <= 0.0 {
            return false;
        }
        let Some(known) = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .map(|m| m.knowledge)
        else {
            return false;
        };
        if known.has(knowledge) || !self.knowledge.requirements_met(knowledge, &known) {
            return false;
        }
        let base = self.knowledge.base_ticks(knowledge);
        if base == 0 {
            return false;
        }
        let mut rng = wander_rng(self.seed, id, tick.wrapping_add(knowledge as u64));
        let factor = 0.25 + rng.next_f32() * 3.75;
        let duration = ((base as f64) * (factor as f64)).round().max(1.0) as u64;
        let finish = tick + duration;
        let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) else {
            return false;
        };
        m.studying = Some(knowledge);
        m.study_finish_tick = finish;
        self.ai_scratch
            .study_requests
            .push((finish, id, knowledge));
        true
    }

    /// Завершение изучения из планировщика. Возвращает true, если знание
    /// действительно было добавлено (задача не устарела).
    pub(crate) fn complete_study(&mut self, entity: u64, knowledge: u16) -> bool {
        let Some(m) = self.entities.get_mut(entity).and_then(|e| e.mind.as_mut()) else {
            return false;
        };
        if m.studying != Some(knowledge) {
            return false;
        }
        m.studying = None;
        m.study_finish_tick = 0;
        m.knowledge.insert(knowledge)
    }

    /// Один тик поведения человека: исполнение текущей цели, по
    /// «обдумыванию» — её переоценка (ночь, голод, запасы, сбор, брожение).
    fn human_tick(&mut self, id: u64, tick: u64, night: bool) {
        // Голодную еду из инвентаря человек ест при любой цели — это дёшево
        // и не требует сканов. Кулдаун поедания общий с игроком.
        self.try_eat_from_inventory(id, tick);

        let think_due = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .is_some_and(|m| tick >= m.next_think_tick);
        if think_due {
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.next_think_tick = tick + THINK_INTERVAL_TICKS;
            }
            // Ночь важнее всего: костёр (если известен) перебивает другие
            // цели. Костёр ищется ходьбой и малым сканом: найдя — человек
            // помнит его; если запомненный исчез — забывает.
            if night {
                let have_fire_goal = self
                    .entities
                    .get(id)
                    .and_then(|e| e.mind.as_ref())
                    .and_then(|m| m.goal)
                    .is_some_and(|g| matches!(g, HumanGoal::Campfire(_)));
                if !have_fire_goal && let Some(fire) = self.known_campfire(id) {
                    self.set_goal(id, Some(HumanGoal::Campfire(fire)));
                }
            } else {
                // Утром цель «костёр» гаснет — человек возвращается к делам.
                let was_fire = self
                    .entities
                    .get(id)
                    .and_then(|e| e.mind.as_ref())
                    .and_then(|m| m.goal)
                    .is_some_and(|g| matches!(g, HumanGoal::Campfire(_)));
                if was_fire {
                    self.set_goal(id, None);
                }
            }
            // Дневные занятия — только без цели (не перебиваем дорогу к
            // еде, охоту и костру): запас еды, сбор палок, взгляд по сторонам.
            let has_goal = self
                .entities
                .get(id)
                .and_then(|e| e.mind.as_ref())
                .is_some_and(|m| m.goal.is_some());
            if !night && !has_goal {
                self.pick_day_activity(id, tick);
            }
            // Голод: цель-еда, если уже не идём к костру.
            let going_to_fire = self
                .entities
                .get(id)
                .and_then(|e| e.mind.as_ref())
                .and_then(|m| m.goal)
                .is_some_and(|g| matches!(g, HumanGoal::Campfire(_)));
            if !going_to_fire && !night {
                let hungry = self
                    .entities
                    .get(id)
                    .is_some_and(|e| e.hunger.value() < HUNGRY_THRESHOLD);
                let has_food_goal = self
                    .entities
                    .get(id)
                    .and_then(|e| e.mind.as_ref())
                    .and_then(|m| m.goal)
                    .is_some_and(|g| matches!(g, HumanGoal::Food(_) | HumanGoal::Hunt(_)));
                if hungry && !has_food_goal
                    && let Some(goal) = self.known_food_goal(id)
                {
                    self.set_goal(id, Some(goal));
                }
            }
        }

        let Some(goal) = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.goal)
        else {
            // Без цели — брожение (как у животных, но без лежания).
            self.human_wander(id, tick);
            return;
        };
        match goal {
            HumanGoal::Campfire(fire) => self.tick_campfire_goal(id, fire, tick),
            HumanGoal::Food(tile) => self.tick_food_goal(id, tile),
            HumanGoal::Hunt(victim) => self.tick_hunt_goal(id, victim, tick),
            HumanGoal::Gather(tile) => self.tick_gather_goal(id, tile, tick),
            HumanGoal::Deposit(tile) => self.tick_deposit_goal(id, tile, tick),
            HumanGoal::TakeFood(tile) => self.tick_take_food_goal(id, tile, tick),
            HumanGoal::Cook(tile) => self.tick_cook_goal(id, tile, tick),
            HumanGoal::Build(tile, kind) => self.tick_build_goal(id, tile, kind, tick),
            HumanGoal::Farm(tile) => self.tick_farm_goal(id, tile, tick),
        }
    }

    /// Дневные занятия свободного человека (без цели): пополнить запас еды
    /// впрок, собрать палки с куста или осмотреться. Один бросок RNG на
    /// «обдумывании» — детерминировано от (seed, id, тик). Остальное время
    /// человек стоит или бродит (общее брожение ИИ).
    /// Общие задачи поселения, не зависящие от роли. Возвращает true,
    /// если цель поставлена.
    fn pick_shared_activity(&mut self, id: u64, _tick: u64) -> bool {
        let Some((hunger, role, has_raw, has_cooked, fire)) = self.entities.get(id).and_then(|e| {
            e.mind.as_ref().map(|m| {
                (
                    e.hunger.value(),
                    m.social_role,
                    inventory_has_raw_meat(&m.inventory),
                    m.inventory
                        .items
                        .iter()
                        .flatten()
                        .any(|stack| is_cooked_meat(stack.kind)),
                    m.known_campfire,
                )
            })
        }) else {
            return false;
        };
        // Голодный сначала ищет готовую еду на складе.
        if hunger < EAT_WHEN_BELOW
            && self.storage_has(id, is_cooked_meat)
            && let Some(storage) = self.known_storage(id)
        {
            self.set_goal(id, Some(HumanGoal::TakeFood(storage)));
            return true;
        }

        // Сырое мясо: повар готовит сам, остальные несут на склад, если
        // он есть (иначе — готовят на костре, как умеют).
        if has_raw {
            let storage = self.known_storage(id);
            let cook_here = role == SocialRole::Cook || storage.is_none();
            if cook_here && let Some(fire) = fire {
                self.set_goal(id, Some(HumanGoal::Cook(fire)));
                return true;
            }
            if let Some(storage) = storage {
                self.set_goal(id, Some(HumanGoal::Deposit(storage)));
                return true;
            }
            if let Some(fire) = fire {
                self.set_goal(id, Some(HumanGoal::Cook(fire)));
                return true;
            }
        }

        // Готовую еду повар (и любой сытый человек) относит на склад,
        // чтобы её могли взять другие. Голодный сначала ест сам.
        if has_cooked
            && hunger >= EAT_WHEN_BELOW
            && let Some(storage) = self.known_storage(id)
        {
            self.set_goal(id, Some(HumanGoal::Deposit(storage)));
            return true;
        }

        // Повар без сырья идёт за ним на склад.
        if role == SocialRole::Cook
            && self.storage_has(id, is_raw_meat)
            && let Some(storage) = self.known_storage(id)
        {
            self.set_goal(id, Some(HumanGoal::TakeFood(storage)));
            return true;
        }

        false
    }

    /// Поведение по назначенной социальной роли.
    fn pick_role_activity(&mut self, id: u64, role: SocialRole, _tick: u64) -> bool {
        match role {
            SocialRole::Hunter => {
                if let Some(goal) = self.known_food_goal(id) {
                    self.set_goal(id, Some(goal));
                    return true;
                }
                if let Some(bush) = self.nearest_bush(id) {
                    self.set_goal(id, Some(HumanGoal::Gather(bush)));
                    return true;
                }
            }
            SocialRole::Gatherer => {
                if let Some(tile) = self.nearest_gather_target(id) {
                    self.set_goal(id, Some(HumanGoal::Gather(tile)));
                    return true;
                }
            }
            SocialRole::Builder => {
                let mut fire = self.known_campfire(id);
                // Один костёр на поселение: прежде чем строить новый,
                // осматриваем радиус поселения (2 чанка) и принимаем
                // найденный костёр как общий.
                if fire.is_none() && let Some(entity) = self.entities.get(id) {
                    let tile = super::world::TilePos {
                        x: entity.pos.x.floor() as i32,
                        y: entity.pos.y.floor() as i32,
                    };
                    if let Some(existing) = self.nearest_building(tile, BuildingKind::Campfire, SETTLEMENT_SEARCH_TILES) {
                        if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                            m.known_campfire = Some(existing);
                        }
                        fire = Some(existing);
                    }
                }
                let sticks = self
                    .entities
                    .get(id)
                    .and_then(|e| e.mind.as_ref())
                    .map_or(0, |m| m.inventory.count_of(ItemKind::Stick));

                if fire.is_none() {
                    if sticks >= 16
                        && let Some(entity) = self.entities.get(id)
                        && let Some(spot) = self.find_build_spot(entity.pos, 5, BuildingKind::Campfire)
                    {
                        self.set_goal(id, Some(HumanGoal::Build(spot, BuildingKind::Campfire)));
                        return true;
                    }
                } else if !self.storage_has_any(id)
                    && let Some(fire) = fire
                    && sticks >= 32
                    && let Some(spot) = self.find_build_spot_near(fire, 6, BuildingKind::Storage)
                {
                    self.set_goal(id, Some(HumanGoal::Build(spot, BuildingKind::Storage)));
                    return true;
                }
                if let Some(bush) = self.nearest_bush(id) {
                    self.set_goal(id, Some(HumanGoal::Gather(bush)));
                    return true;
                }
            }
            SocialRole::Farmer => {
                if self.has_tool_class(id, ToolClass::Hoe)
                    && let Some(grass) = self.nearest_farm_tile(id)
                {
                    self.set_goal(id, Some(HumanGoal::Farm(grass)));
                    return true;
                }
                if let Some(goal) = self.known_food_goal(id) {
                    self.set_goal(id, Some(goal));
                    return true;
                }
                if let Some(bush) = self.nearest_bush(id) {
                    self.set_goal(id, Some(HumanGoal::Gather(bush)));
                    return true;
                }
            }
            SocialRole::Cook => {
                let fire = self.known_campfire(id);
                let has_raw = self
                    .entities
                    .get(id)
                    .and_then(|e| e.mind.as_ref())
                    .is_some_and(|m| inventory_has_raw_meat(&m.inventory));
                if has_raw && let Some(fire) = fire {
                    self.set_goal(id, Some(HumanGoal::Cook(fire)));
                    return true;
                }
                if self.storage_has(id, is_raw_meat) && let Some(storage) = self.known_storage(id) {
                    self.set_goal(id, Some(HumanGoal::TakeFood(storage)));
                    return true;
                }
                if let Some(goal) = self.known_food_goal(id) {
                    self.set_goal(id, Some(goal));
                    return true;
                }
            }
            SocialRole::Unassigned => {}
        }
        false
    }

    /// Максимум один инструмент каждого класса: если нужного класса нет,
    /// пытается сделать его из подручных материалов. Если не хватает
    /// сырья — ставит цель на ближайший ресурс.
    /// Возвращает true, если человек уже занят крафтом/добычей для него.
    fn try_craft_needed_tool(&mut self, id: u64, tick: u64) -> bool {
        let Some(role) = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .map(|m| m.social_role)
        else {
            return false;
        };
        let Some(class) = tool_for_role(role) else {
            return false;
        };
        let Some((known, mut inventory)) = self.entities.get(id).and_then(|e| {
            e.mind.as_ref().map(|m| (m.knowledge, m.inventory))
        }) else {
            return false;
        };
        // Максимум один инструмент каждого класса. Если инструмент уже есть,
        // ищем рецепт лучше (камень → медь) и выбрасываем старый.
        let current = inventory
            .items
            .iter()
            .flatten()
            .filter(|stack| stack.kind.tool_class() == Some(class))
            .map(|stack| stack.kind)
            .max_by_key(|kind| *kind as u8);

        let book = &self.craft_book;
        let catalog = &self.knowledge;
        let recipe = book
            .recipes_for(CrafterKind::Manual)
            .filter(|recipe| {
                recipe.result.kind.tool_class() == Some(class)
                    && recipe.tools.is_empty()
                    && recipe.knowledge_satisfied(&known, catalog)
                    && current.is_none_or(|old| recipe.result.kind as u8 > old as u8)
            })
            .max_by_key(|recipe| recipe.result.kind as u8);
        let Some(recipe) = recipe else {
            return false;
        };
        if plan_craft(book, catalog, &known, &mut inventory, recipe, 0) {
            let old_kind = current.filter(|old| *old != recipe.result.kind);
            let mut discarded = None;
            if let Some(old) = old_kind {
                let count = inventory.count_of(old);
                if count > 0 && inventory.take_items(old, count) {
                    discarded = Some(ItemStack { kind: old, count });
                }
            }
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.inventory = inventory;
                select_item_with_class(&mut m.inventory, &mut m.selected_slot, class);
            }
            if let Some(stack) = discarded
                && let Some(entity) = self.entities.get(id)
            {
                // Старый инструмент выбрасываем, но с большим кулдауном
                // подбора: он не должен тут же вернуться в инвентарь.
                self.item_entities
                    .drop_stack_delayed(self.seed, entity.pos, stack, tick, 20000);
            }
            return false;
        }

        // Не хватает базового ресурса — идём добывать ближайший.
        if let Some(missing) = missing_material(book, catalog, &known, &inventory, recipe, 0)
            && let Some(tile) = self.gather_target_for(id, missing)
        {
            self.set_goal(id, Some(HumanGoal::Gather(tile)));
            return true;
        }
        false
    }

    fn nearest_gather_target(&self, id: u64) -> Option<super::world::TilePos> {
        self.nearest_bush(id)
            .or_else(|| self.nearest_object_kind(id, ObjectKind::Rock))
            .or_else(|| self.nearest_grass_tile(id))
    }

    fn gather_target_for(&self, id: u64, item: ItemKind) -> Option<super::world::TilePos> {
        match item {
            ItemKind::Stick => self.nearest_bush(id),
            ItemKind::Rock => self.nearest_object_kind(id, ObjectKind::Rock),
            ItemKind::GrassBunch | ItemKind::PlantFiber => self.nearest_grass_tile(id),
            _ => None,
        }
    }

    fn nearest_object_kind(
        &self,
        id: u64,
        kind: ObjectKind,
    ) -> Option<super::world::TilePos> {
        let origin = self.entities.get(id)?.pos;
        let center = super::world::TilePos {
            x: origin.x.floor() as i32,
            y: origin.y.floor() as i32,
        };
        self.nearest_object(center, FOOD_SEARCH_TILES as i32, kind, |pos| {
            path_water_crossings(self, origin, WorldPos::of_tile_center(pos)) <= MAX_WATER_CROSSINGS
        })
    }

    fn nearest_grass_tile(&self, id: u64) -> Option<super::world::TilePos> {
        let origin = self.entities.get(id)?.pos;
        self.nearest_tile(origin, FOOD_SEARCH_TILES as i32, |world, pos| {
            (world.terrain_kind(pos) == TileType::Grass
            || world.terrain_kind(pos) == TileType::ForestGrass)
            && world.object_at(pos).is_none()
            && world.crop_at(pos).is_none()
        })
    }

    fn nearest_farm_tile(&self, id: u64) -> Option<super::world::TilePos> {
        let origin = self.entities.get(id)?.pos;
        self.nearest_tile(origin, FOOD_SEARCH_TILES as i32, |world, pos| {
            if let Some(crop) = world.crop_at(pos) {
                return crop.is_mature();
            }
            (world.terrain_kind(pos) == TileType::Grass
            || world.terrain_kind(pos) == TileType::Farmland)
            && world.object_at(pos).is_none()
            && world.item_at(pos).is_none()
        })
    }

    fn nearest_tile(
        &self,
        origin: super::world::WorldPos,
        radius: i32,
        mut accept: impl FnMut(&World, super::world::TilePos) -> bool,
    ) -> Option<super::world::TilePos> {
        let size = self.size.tiles() as i32;
        let tx0 = (origin.x.floor() as i32).clamp(0, size - 1);
        let ty0 = (origin.y.floor() as i32).clamp(0, size - 1);
        let x0 = (tx0 - radius).max(0);
        let y0 = (ty0 - radius).max(0);
        let x1 = (tx0 + radius).min(size - 1);
        let y1 = (ty0 + radius).min(size - 1);
        let mut best: Option<(f32, super::world::TilePos)> = None;
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let pos = super::world::TilePos { x: tx, y: ty };
                if !accept(self, pos) {
                    continue;
                }
                if path_water_crossings(self, origin, WorldPos::of_tile_center(pos)) > MAX_WATER_CROSSINGS {
                    continue;
                }
                let dist = origin.dist(WorldPos::of_tile_center(pos));
                if best.is_none_or(|(d, _)| dist < d) {
                    best = Some((dist, pos));
                }
            }
        }
        best.map(|(_, pos)| pos)
    }

    fn has_tool_class(&self, id: u64, class: ToolClass) -> bool {
        self.entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .is_some_and(|m| inventory_has_tool_class(&m.inventory, class))
    }

    fn known_storage(&mut self, id: u64) -> Option<super::world::TilePos> {
        let settlement = self.ensure_settlement(id);
        if let Some(settlement) = settlement
            && let Some(storage) = self.settlements.get(settlement).and_then(|s| s.storage)
            && self
                .object_at(storage)
                .is_some_and(|o| o.kind == ObjectKind::Building(BuildingKind::Storage))
        {
            return Some(storage);
        }
        let remembered = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.known_storage);
        if let Some(storage) = remembered
            && self
                .object_at(storage)
                .is_some_and(|o| o.kind == ObjectKind::Building(BuildingKind::Storage))
        {
            self.remember_storage(id, settlement, storage);
            return Some(storage);
        }

        let center = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.known_campfire)
            .or_else(|| self.entities.get(id).map(|e| {
                super::world::TilePos {
                    x: e.pos.x.floor() as i32,
                    y: e.pos.y.floor() as i32,
                }
            }))?;
        let found = self.nearest_building(center, BuildingKind::Storage, STORAGE_SEARCH_TILES);
        if let Some(storage) = found {
            self.remember_storage(id, settlement, storage);
        }
        found
    }

    fn remember_storage(&mut self, id: u64, settlement: Option<u64>, storage: super::world::TilePos) {
        if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
            m.known_storage = Some(storage);
        }
        if let Some(settlement) = settlement
            && let Some(settlement) = self.settlements.get_mut(settlement)
        {
            settlement.storage = Some(storage);
        }
    }

    fn nearest_building(
        &self,
        center: super::world::TilePos,
        kind: BuildingKind,
        radius: i32,
    ) -> Option<super::world::TilePos> {
        self.nearest_object(center, radius, ObjectKind::Building(kind), |_| true)
    }

    fn storage_has_any(&mut self, id: u64) -> bool {
        let Some(tile) = self.known_storage(id) else {
            return false;
        };
        containers::container_at(self, tile)
            .is_some_and(|container| container.slots.iter().any(Option::is_some))
    }

    fn storage_has(&mut self, id: u64, accept: fn(ItemKind) -> bool) -> bool {
        let Some(tile) = self.known_storage(id) else {
            return false;
        };
        containers::container_at(self, tile).is_some_and(|container| {
            container
                .slots
                .iter()
                .flatten()
                .any(|stack| accept(stack.kind))
        })
    }

    fn find_build_spot(
        &self,
        origin: WorldPos,
        radius: i32,
        kind: BuildingKind,
    ) -> Option<super::world::TilePos> {
        let center = super::world::TilePos {
            x: origin.x.floor() as i32,
            y: origin.y.floor() as i32,
        };
        self.find_build_spot_near(center, radius, kind)
    }

    fn find_build_spot_near(
        &self,
        center: super::world::TilePos,
        radius: i32,
        _kind: BuildingKind,
    ) -> Option<super::world::TilePos> {
        for r in 1..=radius {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let pos = super::world::TilePos {
                        x: center.x + dx,
                        y: center.y + dy,
                    };
                    if commands::buildable_at(self, pos) {
                        return Some(pos);
                    }
                }
            }
        }
        None
    }

    fn tick_deposit_goal(&mut self, id: u64, tile: super::world::TilePos, _tick: u64) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) > super::actions::INTERACT_REACH {
            if entity.state != EntityState::Walking {
                self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
            }
            return;
        }

        let valid = self
            .object_at(tile)
            .is_some_and(|o| o.kind == ObjectKind::Building(BuildingKind::Storage));
        if !valid {
            self.set_goal(id, None);
            return;
        }

        let mut taken: Vec<ItemStack> = Vec::new();
        if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
            for slot in &mut m.inventory.items[..MISC_SLOTS] {
                if let Some(stack) = *slot && should_deposit(stack.kind) {
                    taken.push(stack);
                    *slot = None;
                }
            }
        }

        let mut leftover = Vec::new();
        if let Some(container) = containers::container_at_mut(self, tile) {
            for stack in taken {
                let left = store_stack(container, stack);
                if left > 0 {
                    leftover.push(ItemStack {
                        kind: stack.kind,
                        count: left,
                    });
                }
            }
        }
        if !leftover.is_empty() && let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
            for stack in leftover {
                let _ = m.inventory.add(stack.kind, stack.count);
            }
        }
        self.set_goal(id, None);
    }

    fn tick_take_food_goal(&mut self, id: u64, tile: super::world::TilePos, _tick: u64) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) > super::actions::INTERACT_REACH {
            if entity.state != EntityState::Walking {
                self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
            }
            return;
        }

        let valid = self
            .object_at(tile)
            .is_some_and(|o| o.kind == ObjectKind::Building(BuildingKind::Storage));
        if !valid {
            self.set_goal(id, None);
            return;
        }

        let is_cook = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .is_some_and(|m| m.social_role == SocialRole::Cook);
        let fire = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.known_campfire);

        // Повар берёт сырое мясо, остальные — готовую еду. Если нужного
        // нет, берём то, что есть.
        let accept: fn(ItemKind) -> bool = if is_cook { is_raw_meat } else { is_cooked_meat };
        let need = if is_cook { 2 } else { FOOD_TAKE_COUNT };
        let mut taken: Vec<ItemStack> = Vec::new();
        let mut total = 0;
        if let Some(container) = containers::container_at_mut(self, tile) {
            for slot in 0..container.slots.len() {
                if total >= need {
                    break;
                }
                let Some(stack) = container.get(slot) else {
                    continue;
                };
                if !accept(stack.kind) {
                    continue;
                }
                let Some(stack) = container.take(slot) else {
                    continue;
                };
                total += stack.count;
                taken.push(stack);
            }
            if taken.is_empty() && is_cook {
                // Повар согласен на готовую еду, если сырья нет.
                for slot in 0..container.slots.len() {
                    if total >= need {
                        break;
                    }
                    let Some(stack) = container.get(slot) else {
                        continue;
                    };
                    if !is_cooked_meat(stack.kind) {
                        continue;
                    }
                    let Some(stack) = container.take(slot) else {
                        continue;
                    };
                    total += stack.count;
                    taken.push(stack);
                }
            }
        }

        if taken.is_empty() {
            self.set_goal(id, None);
            return;
        }

        let mut gave_raw = false;
        let mut overflow: Vec<ItemStack> = Vec::new();
        if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
            for stack in taken {
                gave_raw |= is_raw_meat(stack.kind);
                let left = m.inventory.add(stack.kind, stack.count);
                if left > 0 {
                    overflow.push(ItemStack {
                        kind: stack.kind,
                        count: left,
                    });
                }
            }
        }
        // Не теряем еду, если инвентарь полон: возвращаем остаток на склад.
        if !overflow.is_empty() && let Some(container) = containers::container_at_mut(self, tile) {
            for stack in overflow {
                let left = store_stack(container, stack);
                if left > 0 {
                    let _ = left;
                }
            }
        }
        if gave_raw && let Some(fire) = fire {
            self.set_goal(id, Some(HumanGoal::Cook(fire)));
        } else {
            self.set_goal(id, None);
        }
    }

    fn tick_cook_goal(&mut self, id: u64, tile: super::world::TilePos, tick: u64) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) > super::actions::INTERACT_REACH {
            if entity.state != EntityState::Walking {
                self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
            }
            return;
        }
        let fire_exists = self
            .object_at(tile)
            .is_some_and(|o| o.kind == ObjectKind::Building(BuildingKind::Campfire));
        if !fire_exists {
            self.set_goal(id, None);
            return;
        }

        // Уже готовится: ждём и забираем результат.
        let cooking = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.cooking.map(|(kind, count)| (kind, count, m.cook_finish_tick)));
        if let Some((kind, count, finish)) = cooking {
            if tick >= finish  &&  let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.cooking = None;
                m.cook_finish_tick = 0;
                let _ = m.inventory.add(kind, count);
            }
            return;
        }

        let Some(raw) = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| first_item(&m.inventory, is_raw_meat))
        else {
            self.set_goal(id, None);
            return;
        };
        let Some(recipe) = self.craft_book.cooking_recipe(CrafterKind::Campfire, raw) else {
            self.set_goal(id, None);
            return;
        };
        let result = recipe.result;
        let known = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .map_or(KnowledgeSet::new(), |m| m.knowledge);
        if !recipe.knowledge_satisfied(&known, &self.knowledge)
            || !self
                .entities
                .get(id)
                .and_then(|e| e.mind.as_ref())
                .is_some_and(|m| m.inventory.can_fit(result.kind, result.count))
        {
            self.set_goal(id, None);
            return;
        }
        let cooked = self
            .entities
            .get_mut(id)
            .and_then(|e| e.mind.as_mut())
            .is_some_and(|m| {
                if !m.inventory.take_items(raw, 1) {
                    return false;
                }
                m.cooking = Some((result.kind, result.count));
                m.cook_finish_tick = tick + containers::CAMPFIRE_COOK_TICKS;
                true
            });
        if !cooked {
            self.set_goal(id, None);
        }
    }

    fn tick_build_goal(
        &mut self,
        id: u64,
        tile: super::world::TilePos,
        kind: BuildingKind,
        _tick: u64,
    ) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) > super::actions::INTERACT_REACH {
            if entity.state != EntityState::Walking {
                self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
            }
            return;
        }
        if !commands::buildable_at(self, tile) {
            self.set_goal(id, None);
            return;
        }
        if kind == BuildingKind::Campfire
            && let Some(existing) = self.nearest_building(tile, BuildingKind::Campfire, SETTLEMENT_SEARCH_TILES)
        {
            // Пока строитель шёл, кто-то уже поставил костёр. Второй
            // костёр в том же поселении не нужен.
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.known_campfire = Some(existing);
            }
            self.set_goal(id, None);
            return;
        }
        if kind == BuildingKind::Storage
            && let Some(existing) =
                self.nearest_building(tile, BuildingKind::Storage, SETTLEMENT_SEARCH_TILES)
        {
            // Аналогично складу: одного на поселение достаточно, иначе
            // добыча расползётся по нескольким контейнерам.
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.known_storage = Some(existing);
            }
            if let Some(settlement) = self.ensure_settlement(id)
                && let Some(settlement) = self.settlements.get_mut(settlement)
            {
                settlement.storage = Some(existing);
            }
            self.set_goal(id, None);
            return;
        }
        if super::actions::build(self, id, tile, kind) {
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                match kind {
                    BuildingKind::Campfire => m.known_campfire = Some(tile),
                    BuildingKind::Storage => m.known_storage = Some(tile),
                    _ => {}
                }
            }
            match kind {
                BuildingKind::Campfire => {
                    self.ensure_settlement(id);
                }
                BuildingKind::Storage => {
                    let settlement = self.ensure_settlement(id);
                    self.remember_storage(id, settlement, tile);
                }
                _ => {}
            }
        }
        self.set_goal(id, None);
    }

    fn tick_farm_goal(&mut self, id: u64, tile: super::world::TilePos, tick: u64) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) > super::actions::INTERACT_REACH {
            if entity.state != EntityState::Walking {
                self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
            }
            return;
        }

        // Спелый урожай собираем.
        if self.crop_at(tile).is_some_and(|crop| crop.is_mature()) {
            let _ = super::actions::harvest_crop(self, id, tile);
            self.set_goal(id, None);
            return;
        }

        // Грядку засеваем, траву сначала вспахиваем мотыгой.
        let terrain = self.terrain_kind(tile);
        if terrain == TileType::Grass {
            if !select_item_with_class_in_world(self, id, ToolClass::Hoe) {
                self.set_goal(id, None);
                return;
            }
            let _ = super::actions::interact(self, id, tile, tick);
        }
        if self.terrain_kind(tile) == TileType::Farmland
            && self.crop_at(tile).is_none()
            && select_seed_in_world(self, id)
        {
            let _ = super::actions::sow(self, id, tile);
        }
        self.set_goal(id, None);
    }

    /// Выбрать следующую цель днём. Сначала общие задачи поселения, затем
    /// роль, затем прежний случайный fallback.
    fn pick_day_activity(&mut self, id: u64, tick: u64) {
        // Сначала общие потребности поселения: поесть со склада, отнести
        // добычу, приготовить мясо. Это работает и без назначенной роли,
        // поэтому охотник сам приготовит еду, если повара нет.
        if self.pick_shared_activity(id, tick) {
            return;
        }

        let role = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .map_or(SocialRole::Unassigned, |m| m.social_role);
        if role == SocialRole::Unassigned {
            // Если роли в поселении не назначены, люди сами закрывают
            // базовые потребности общины: сначала костёр, затем склад.
            // После этого они возвращаются к свободному поведению.
            let has_fire = self.known_campfire(id).is_some();
            let has_storage = has_fire && self.known_storage(id).is_some();
            if (!has_fire || !has_storage)
                && self.pick_role_activity(id, SocialRole::Builder, tick)
            {
                return;
            }
        } else {
            // Строитель сначала закрывает жилищные задачи (костёр/склад),
            // и только потом думает об инструменте.
            if role == SocialRole::Builder {
                if self.pick_role_activity(id, role, tick) {
                    return;
                }
                self.try_craft_needed_tool(id, tick);
                return;
            }
            // Остальным инструмент нужен для роли: если не хватает
            // материала, ИИ пойдёт добывать его и не станет отвлекаться.
            if self.try_craft_needed_tool(id, tick) {
                return;
            }
            if self.pick_role_activity(id, role, tick) {
                return;
            }
        }

        let mut rng = wander_rng(self.seed, id, tick);
        let roll = rng.next_f32();
        // Запас еды впрок: та же цель-еда, что и при голоде, но по сытости
        // ещё далеко до порога поиска. Не находится еда — цель не ставится.
        if roll < 0.3 {
            let not_full = self
                .entities
                .get(id)
                .is_some_and(|e| e.hunger.value() < STOCKPILE_THRESHOLD);
            if not_full && let Some(goal) = self.known_food_goal(id) {
                self.set_goal(id, Some(goal));
                return;
            }
        }
        // Сбор палок с куста: задел под крафт и строительство.
        if roll < 0.5 && let Some(bush) = self.nearest_bush(id) {
            self.set_goal(id, Some(HumanGoal::Gather(bush)));
            return;
        }
        // Осмотреться: повернуться в случайную сторону (живость покоя).
        if roll < 0.7 && let Some(entity) = self.entities.get_mut(id) {
            entity.facing = if rng.next_f32() < 0.5 {
                super::entities::Facing::Left
            } else {
                super::entities::Facing::Right
            };
        }
    }

    /// Ставит сущности цель (и сбрасывает устаревший путь, если цель
    /// сменилась на другую).
    fn set_goal(&mut self, id: u64, goal: Option<HumanGoal>) {
        let Some(entity) = self.entities.get_mut(id) else {
            return;
        };
        let changed = entity.mind.as_ref().and_then(|m| m.goal) != goal;
        if let Some(m) = entity.mind.as_mut() {
            m.goal = goal;
        }
        if changed {
            // Новая цель — старый путь больше не имеет смысла: сброс в
            // стояние, следующий тик цели отправит в путь.
            entity.state = EntityState::Standing;
            entity.state_ticks = 0;
            entity.stuck_ticks = 0;
        }
    }

    /// Еда из своего инвентаря: находит слот с едой, выбирает его и ест
    /// с общим кулдауном поедания. Сытый (выше порога) еду не тратит —
    /// запас нужен на дорогу и ночь.
    fn try_eat_from_inventory(&mut self, id: u64, tick: u64) {
        // Сытость читается до заёма ума: существо и ум — разные поля.
        let satiated = self
            .entities
            .get(id)
            .is_some_and(|e| e.hunger.value() >= EAT_WHEN_BELOW);
        let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) else {
            return;
        };
        if tick < m.eat_ready_tick || satiated {
            return;
        }
        let Some(slot) = m
            .inventory
            .items
            .iter()
            .position(|s| s.is_some_and(|stack| stack.kind.food_value() > 0.0))
        else {
            return;
        };
        m.selected_slot = slot.min(MISC_SLOTS - 1);
        if super::actions::eat_selected(self, id)
            && let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut())
        {
            m.eat_ready_tick = tick + EAT_COOLDOWN_TICKS;
        }
    }

    /// Цель «костёр»: дойти и стоять рядом до утра; погасший/убранный
    /// костёр гасит цель и забывается.
    fn tick_campfire_goal(&mut self, id: u64, fire: super::world::TilePos, tick: u64) {
        // Костёр исчез (сгорел дотла, убран редактором) — цель недостижима.
        let fire_exists = self
            .object_at(fire)
            .is_some_and(|o| o.kind == ObjectKind::Building(super::objects::BuildingKind::Campfire));
        if !fire_exists {
            self.set_goal(id, None);
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut())
                && m.known_campfire == Some(fire)
            {
                m.known_campfire = None;
            }
            return;
        }
        let center = WorldPos::of_tile_center(fire);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        let near = entity.pos.dist(center) <= CAMPFIRE_ARRIVE_TILES;
        if near {
            let (pos, standing) = (entity.pos, entity.state == EntityState::Standing);
            // Стоит у огня до утра: длинное стояние (ИИ обновит утром).
            if !standing {
                let Some(entity) = self.entities.get_mut(id) else {
                    return;
                };
                entity.state = EntityState::Standing;
                entity.state_ticks = 0;
                entity.state_duration = TICKS_PER_SEC as u64;
                entity.target = entity.pos;
                entity.stuck_ticks = 0;
            }
            // Стоя у костра, смотрит на огонь, а не куда пришлось.
            if let Some(facing) = super::entities::screen_facing(center.x - pos.x, center.y - pos.y)
                && let Some(entity) = self.entities.get_mut(id)
            {
                entity.facing = facing;
            }
            return;
        }
        // Ещё в пути: поддерживаем ходьбу (FSM мог закончить состояние).
        if entity.state != EntityState::Walking {
            let ticks_left = ticks_until_morning(tick);
            self.walk_to(id, center, ticks_left.max(TICKS_PER_SEC as u64));
        }
    }

    /// Цель «еда»: дойти до тайла и собрать (яблоня или зрелая культура).
    fn tick_food_goal(&mut self, id: u64, tile: super::world::TilePos) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) <= super::actions::INTERACT_REACH {
            // На месте: сначала плоды, затем культура (порядок не важен —
            // на тайле есть что-то одно). Успех или пусто — цель исполнена,
            // голод сам решит, нужна ли следующая.
            let _ = super::actions::harvest_fruit(self, id, tile);
            let _ = super::actions::harvest_crop(self, id, tile);
            // Собранная/исчезнувшая еда из памяти тоже уходит.
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut())
                && m.known_food == Some(tile)
            {
                m.known_food = None;
            }
            self.set_goal(id, None);
            return;
        }
        if entity.state != EntityState::Walking {
            self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
        }
    }

    /// Цель «сбор»: дойти до куста и облупить его (палки в инвентарь,
    /// куст исчезает — он отрастает цепочкой BushGrowth). Куст уже не
    /// куст — цель исполнена или отменена.
    fn tick_gather_goal(&mut self, id: u64, tile: super::world::TilePos, tick: u64) {
        let center = WorldPos::of_tile_center(tile);
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(center) <= super::actions::INTERACT_REACH {
            let object = self.object_at(tile).map(|o| o.kind);
            match object {
                Some(kind) if kind.is_bush() => {
                    let _ = super::actions::interact(self, id, tile, tick);
                }
                Some(ObjectKind::Rock) => {
                    // Камень можно добыть палкой/камнем или киркой.
                    if !select_item_kind_in_world(self, id, ItemKind::Stick) {
                        let _ = select_item_kind_in_world(self, id, ItemKind::Rock);
                    }
                    let _ = super::actions::interact(self, id, tile, tick);
                }
                None if matches!(
                    self.terrain_kind(tile),
                    TileType::Grass | TileType::ForestGrass
                ) => {
                    // Для травы нужен пустой слот, нож или уже собранная
                    // трава; после камня в руке может остаться палка.
                    deselect_item_in_world(self, id);
                    let _ = super::actions::interact(self, id, tile, tick);
                }
                _ => {}
            }
            self.set_goal(id, None);
            return;
        }
        if entity.state != EntityState::Walking {
            self.walk_to(id, center, 20 * TICKS_PER_SEC as u64);
        }
    }

    /// Цель «охота»: догнать животное и бить, пока не падёт. Мясо падает
    /// на землю и подбирается автоподбором.
    fn tick_hunt_goal(&mut self, id: u64, victim: u64, tick: u64) {
        let Some(victim_pos) = self.entities.get(victim).map(|e| e.pos) else {
            // Жертва мертва или убрана — охота окончена.
            self.set_goal(id, None);
            return;
        };
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.pos.dist(victim_pos) <= HUNT_ATTACK_TILES {
            combat::attack_entity(self, id, victim, tick);
            return;
        }
        if entity.state != EntityState::Walking {
            self.walk_to(id, victim_pos, 30 * TICKS_PER_SEC as u64);
        } else {
            // Жертва движется: обновляем точку пути, чтобы не бегать по
            // старым координатам.
            let Some(entity) = self.entities.get_mut(id) else {
                return;
            };
            entity.target = victim_pos;
        }
    }

    /// Брожение без цели: из стояния, когда его срок истёк, выбрать
    /// случайную точку рядом (как у животных, детерминированно).
    fn human_wander(&mut self, id: u64, tick: u64) {
        let Some(entity) = self.entities.get(id) else {
            return;
        };
        if entity.state != EntityState::Standing || entity.state_ticks < entity.state_duration {
            return;
        }
        let seed = self.seed;
        let mut rng = wander_rng(seed, id, tick);
        let side = self.size.tiles() as f32;
        for _ in 0..16 {
            let angle = rng.next_f32() * std::f32::consts::TAU;
            let radius = 8.0 * rng.next_f32().sqrt();
            let target = WorldPos {
                x: (entity.pos.x + angle.cos() * radius).clamp(0.0, side),
                y: (entity.pos.y + angle.sin() * radius).clamp(0.0, side),
            };
            // Цель — только сухопутный тайл, и по пути не больше пары
            // проб воды (узкая река). Озеро по прямой — цель отбрасывается:
            // человек не «плывёт» в брожении. Не нашли чистую цель — стоим,
            // на следующем обдумывании попробует снова.
            if !super::actions::terrain_at(self, target).is_some_and(|k| k.is_water())
                && path_water_crossings(self, entity.pos, target) <= MAX_WATER_CROSSINGS
            {
                self.walk_to(id, target, 10 * TICKS_PER_SEC as u64);
                return;
            }
        }
    }

    /// Переводит существо в ходьбу к точке с заданным лимитом тиков.
    fn walk_to(&mut self, id: u64, target: WorldPos, duration_ticks: u64) {
        let Some(entity) = self.entities.get_mut(id) else {
            return;
        };
        entity.state = EntityState::Walking;
        entity.state_ticks = 0;
        entity.state_duration = duration_ticks;
        entity.target = target;
        entity.stuck_ticks = 0;
        if let Some(facing) = super::entities::screen_facing(target.x - entity.pos.x, target.y - entity.pos.y) {
            entity.facing = facing;
        }
    }

    /// Известный костёр: проверяет память человека (`known_campfire`),
    /// при неактуальности (костёр убран) забывает её и делает малый скан
    /// окрестностей — найдя, запоминает. Логика «ходить и искать»:
    /// бродящий человек время от времени сканирует новый квадрат и
    /// пополняет память, вместо дальних сканов каждый тик.
    fn known_campfire(&mut self, id: u64) -> Option<super::world::TilePos> {
        let remembered = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.known_campfire);
        if let Some(fire) = remembered {
            let valid = self.object_at(fire)
                .is_some_and(|o| o.kind == ObjectKind::Building(super::objects::BuildingKind::Campfire));
            if valid {
                return Some(fire);
            }
            // Запомненное устарело — забыть.
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.known_campfire = None;
            }
        }
        let found = self.nearest_campfire(id);
        if let Some(fire) = found
            && let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut())
        {
            m.known_campfire = Some(fire);
        }
        found
    }

    /// Цель-еда по памяти (`known_food`): неактуальная (собрана, съедена
    /// кем-то) забывается; малый скан окрестностей пополняет память.
    /// В remembered-еде нет — пробует охоту: ближайший волк в радиусе.
    fn known_food_goal(&mut self, id: u64) -> Option<HumanGoal> {
        let remembered = self
            .entities
            .get(id)
            .and_then(|e| e.mind.as_ref())
            .and_then(|m| m.known_food);
        if let Some(tile) = remembered {
            if self.food_still_there(tile) {
                return Some(HumanGoal::Food(tile));
            }
            // Запомненное устарело — забыть.
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.known_food = None;
            }
        }
        if let Some(tile) = self.nearest_food_tile(id) {
            if let Some(m) = self.entities.get_mut(id).and_then(|e| e.mind.as_mut()) {
                m.known_food = Some(tile);
            }
            return Some(HumanGoal::Food(tile));
        }
        self.find_hunt_goal(id)
    }

    /// Еда ли ещё на тайле: плоды (яблоня, вишнёвый куст) или зрелая культура.
    fn food_still_there(&self, tile: super::world::TilePos) -> bool {
        match self.object_at(tile) {
            Some(obj) => obj.fruit_yield().is_some(),
            None => self.crop_at(tile).is_some_and(|crop| crop.is_mature()),
        }
    }

    /// Охота: ближайший волк в радиусе поиска еды.
    fn find_hunt_goal(&mut self, id: u64) -> Option<HumanGoal> {
        let origin = self.entities.get(id)?.pos;
        let mut best: Option<(f32, u64)> = None;
        for entity in self.entities.iter() {
            if entity.kind != EntityKind::Wolf {
                continue;
            }
            let dist = origin.dist(entity.pos);
            if dist <= FOOD_SEARCH_TILES && best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, entity.id));
            }
        }
        best.map(|(_, victim)| HumanGoal::Hunt(victim))
    }

    /// Ближайший костёр в радиусе `CAMPFIRE_SEARCH_TILES` от человека.
    /// Скан идёт по чанкам в квадрате радиуса — только на «обдумывании».
    fn nearest_campfire(&self, id: u64) -> Option<super::world::TilePos> {
        let origin = self.entities.get(id)?.pos;
        let center = super::world::TilePos {
            x: origin.x.floor() as i32,
            y: origin.y.floor() as i32,
        };
        self.nearest_object(
            center,
            CAMPFIRE_SEARCH_TILES as i32,
            ObjectKind::Building(BuildingKind::Campfire),
            |_| true,
        )
    }

    /// Цель-еда по памяти: см. `known_food_goal`. Старый скан-выбор
    /// заменён памятью с малым сканом окрестностей.
    /// Ближайший тайл с готовой едой: плоды (яблоня, вишнёвый куст) или
    /// зрелая культура. Скан по чанкам квадрата радиуса `FOOD_SEARCH_TILES`.
    fn nearest_food_tile(&self, id: u64) -> Option<super::world::TilePos> {
        let origin = self.entities.get(id)?.pos;
        let radius = FOOD_SEARCH_TILES as i32;
        let size = self.size.tiles() as i32;
        let tx0 = (origin.x.floor() as i32).clamp(0, size - 1);
        let ty0 = (origin.y.floor() as i32).clamp(0, size - 1);
        let x0 = (tx0 - radius).max(0);
        let y0 = (ty0 - radius).max(0);
        let x1 = (tx0 + radius).min(size - 1);
        let y1 = (ty0 + radius).min(size - 1);

        let mut best: Option<(f32, super::world::TilePos)> = None;
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                let pos = super::world::TilePos { x: tx, y: ty };
                let food = match self.object_at(pos) {
                    Some(obj) => obj.fruit_yield().is_some(),
                    None => self.crop_at(pos).is_some_and(|crop| crop.is_mature()),
                };
                if !food {
                    continue;
                }
                // Еда за озером по прямой не рассматривается: человек в
                // брожении туда не идёт (узкая река — можно).
                if path_water_crossings(self, origin, WorldPos::of_tile_center(pos)) > MAX_WATER_CROSSINGS {
                    continue;
                }
                let dist = origin.dist(WorldPos::of_tile_center(pos));
                if best.is_none_or(|(d, _)| dist < d) {
                    best = Some((dist, pos));
                }
            }
        }
        best.map(|(_, pos)| pos)
    }

    /// Ближайший куст (палки) в радиусе поиска еды. Куст и вишнёвый куст —
    /// разные виды, поэтому берём ближайший из двух индексов.
    fn nearest_bush(&self, id: u64) -> Option<super::world::TilePos> {
        let origin = self.entities.get(id)?.pos;
        let center = super::world::TilePos {
            x: origin.x.floor() as i32,
            y: origin.y.floor() as i32,
        };
        let radius = FOOD_SEARCH_TILES as i32;
        let mut best: Option<(f32, super::world::TilePos)> = None;
        for kind in [ObjectKind::Bush, ObjectKind::CherryBush] {
            let found = self.nearest_object(center, radius, kind, |pos| {
                path_water_crossings(self, origin, WorldPos::of_tile_center(pos)) <= MAX_WATER_CROSSINGS
            });
            if let Some(pos) = found {
                let dist = origin.dist(WorldPos::of_tile_center(pos));
                if best.is_none_or(|(d, _)| dist < d) {
                    best = Some((dist, pos));
                }
            }
        }
        best.map(|(_, pos)| pos)
    }
}

/// Тиков до конца ночи (час `NIGHT_END_HOUR`) — лимит ходьбы к костру.
fn ticks_until_morning(tick: u64) -> u64 {
    let hour = time::hour_of_tick(tick);
    let hours_left = if hour >= NIGHT_START_HOUR {
        24 - hour + NIGHT_END_HOUR
    } else {
        NIGHT_END_HOUR - hour
    };
    hours_left as u64 * time::SECS_PER_HOUR / time::GAME_SECS_PER_TICK as u64
}

/// Сколько проб воды допустимо на прямом пути человека: 1–2 — узкая река,
/// переходится вброд; больше — озеро, человек в брожении туда не идёт.
const MAX_WATER_CROSSINGS: usize = 2;

/// Число проб воды на прямой «от—до» (8 проб по отрезку). Для выбора
/// целей брожения и добычи: озеро на пути — повод искать цель с другой
/// стороны или подождать.
fn path_water_crossings(world: &World, from: WorldPos, to: WorldPos) -> usize {
    const SAMPLES: usize = 8;
    let mut crossings = 0;
    for step in 1..=SAMPLES {
        let t = step as f32 / SAMPLES as f32;
        let point = WorldPos {
            x: from.x + (to.x - from.x) * t,
            y: from.y + (to.y - from.y) * t,
        };
        if super::actions::terrain_at(world, point).is_some_and(|kind| kind.is_water()) {
            crossings += 1;
        }
    }
    crossings
}

fn is_raw_meat(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::MeatBirdRaw | ItemKind::MeatHerbivoreRaw | ItemKind::MeatPredatorRaw
    )
}

fn is_cooked_meat(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::MeatBirdCooked | ItemKind::MeatHerbivoreCooked | ItemKind::MeatPredatorCooked
    )
}

fn inventory_has_raw_meat(inventory: &super::items::Inventory) -> bool {
    inventory
        .items
        .iter()
        .flatten()
        .any(|stack| is_raw_meat(stack.kind))
}

fn inventory_has_tool_class(inventory: &super::items::Inventory, class: ToolClass) -> bool {
    inventory
        .items
        .iter()
        .flatten()
        .any(|stack| stack.kind.tool_class() == Some(class))
}

fn first_item(
    inventory: &super::items::Inventory,
    accept: fn(ItemKind) -> bool,
) -> Option<ItemKind> {
    inventory
        .items
        .iter()
        .flatten()
        .map(|stack| stack.kind)
        .find(|&kind| accept(kind))
}

fn tool_for_role(role: SocialRole) -> Option<ToolClass> {
    match role {
        SocialRole::Hunter => Some(ToolClass::Spear),
        SocialRole::Farmer => Some(ToolClass::Hoe),
        SocialRole::Builder => Some(ToolClass::Axe),
        SocialRole::Unassigned | SocialRole::Gatherer | SocialRole::Cook => None,
    }
}

fn should_deposit(kind: ItemKind) -> bool {
    is_raw_meat(kind) || is_cooked_meat(kind)
}

/// Кладёт стопку в контейнер, сливая одинаковые предметы. Возвращает
/// остаток, который не поместился.
fn store_stack(container: &mut Container, stack: ItemStack) -> u32 {
    let mut left = stack.count;
    if left == 0 {
        return 0;
    }
    let cap = stack.kind.max_stack();
    for slot in 0..container.slots.len() {
        if left == 0 {
            break;
        }
        let Some(existing) = container.get(slot) else {
            continue;
        };
        if existing.kind != stack.kind || existing.count >= cap {
            continue;
        }
        let moved = (cap - existing.count).min(left);
        if moved > 0 {
            container.replace(
                slot,
                ItemStack {
                    kind: existing.kind,
                    count: existing.count + moved,
                },
            );
            left -= moved;
        }
    }
    for slot in 0..container.slots.len() {
        if left == 0 {
            break;
        }
        if container.get(slot).is_some() {
            continue;
        }
        let moved = cap.min(left);
        container.put(
            slot,
            ItemStack {
                kind: stack.kind,
                count: moved,
            },
        );
        left -= moved;
    }
    left
}

fn select_item_with_class(
    inventory: &mut super::items::Inventory,
    selected_slot: &mut usize,
    class: ToolClass,
) -> bool {
    let Some(slot) = inventory
        .items
        .iter()
        .position(|stack| stack.is_some_and(|stack| stack.kind.tool_class() == Some(class)))
    else {
        return false;
    };
    *selected_slot = slot.min(MISC_SLOTS - 1);
    true
}

fn select_item_with_class_in_world(world: &mut World, id: u64, class: ToolClass) -> bool {
    let Some(m) = world.entities.get_mut(id).and_then(|e| e.mind.as_mut()) else {
        return false;
    };
    let (mut inventory, mut selected) = (m.inventory, m.selected_slot);
    let ok = select_item_with_class(&mut inventory, &mut selected, class);
    if ok {
        m.inventory = inventory;
        m.selected_slot = selected;
    }
    ok
}

fn select_item_kind_in_world(world: &mut World, id: u64, kind: ItemKind) -> bool {
    let Some(m) = world.entities.get_mut(id).and_then(|e| e.mind.as_mut()) else {
        return false;
    };
    let Some(slot) = m
        .inventory
        .items
        .iter()
        .position(|stack| stack.is_some_and(|stack| stack.kind == kind))
    else {
        return false;
    };
    m.selected_slot = slot.min(MISC_SLOTS - 1);
    true
}

fn deselect_item_in_world(world: &mut World, id: u64) -> bool {
    let Some(m) = world.entities.get_mut(id).and_then(|e| e.mind.as_mut()) else {
        return false;
    };
    let Some(slot) = m.inventory.items.iter().position(Option::is_none) else {
        return false;
    };
    m.selected_slot = slot.min(MISC_SLOTS - 1);
    true
}

fn select_seed_in_world(world: &mut World, id: u64) -> bool {
    let Some(m) = world.entities.get_mut(id).and_then(|e| e.mind.as_mut()) else {
        return false;
    };
    let Some(slot) = m.inventory.items.iter().position(|stack| {
        stack.is_some_and(|stack| CropKind::from_seed_item(stack.kind).is_some())
    }) else {
        return false;
    };
    m.selected_slot = slot.min(MISC_SLOTS - 1);
    true
}

fn wander_rng(seed: u64, id: u64, tick: u64) -> SplitMix64 {
    let x = seed
        ^ id.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ tick.wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ 0x0D1E_5EED;
    SplitMix64::new(mix_seed(x, 0x0A1D_0000_0000_0001))
}
