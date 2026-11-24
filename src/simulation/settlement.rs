//! Поселения — отдельные сущности мира.
//!
//! У поселения есть id, центр (костёр), радиус, список жителей,
//! общий склад и накопленные общие знания. Это позволяет строить общую
//! логику (обмен знаниями, строительство, логистику), не сверяя попарно
//! всех людей.
//!
//! Поселения сохраняются: id стабилен, жители переживают загрузку.

use super::knowledge::KnowledgeSet;
use super::world::{TilePos, WorldPos};

/// Радиус поселения по умолчанию — 2 чанка (32 тайла).
pub const SETTLEMENT_RADIUS_TILES: f32 = 32.0;

/// Одно поселение вокруг костра.
#[derive(Clone, Debug)]
pub struct Settlement {
    pub id: u64,
    /// Центр — тайл костра.
    pub center: TilePos,
    /// Общий склад, если построен.
    pub storage: Option<TilePos>,
    /// Жители по возрастанию id.
    pub members: Vec<u64>,
    /// Знания, накопленные поселением (пока пополняется при вступлении).
    pub shared_knowledge: KnowledgeSet,
}

impl Settlement {
    pub fn new(id: u64, center: TilePos) -> Self {
        Self {
            id,
            center,
            storage: None,
            members: Vec::new(),
            shared_knowledge: KnowledgeSet::new(),
        }
    }

    pub fn center_pos(&self) -> WorldPos {
        WorldPos::of_tile_center(self.center)
    }

    pub fn contains(&self, pos: WorldPos) -> bool {
        pos.dist(self.center_pos()) <= SETTLEMENT_RADIUS_TILES
    }

    /// Добавляет жителя; повторное добавление ничего не меняет.
    pub fn add_member(&mut self, id: u64) -> bool {
        match self.members.binary_search(&id) {
            Ok(_) => false,
            Err(index) => {
                self.members.insert(index, id);
                true
            }
        }
    }

    pub fn remove_member(&mut self, id: u64) -> bool {
        match self.members.binary_search(&id) {
            Ok(index) => {
                self.members.remove(index);
                true
            }
            Err(_) => false,
        }
    }
}

/// Хранилище поселений. Порядок списка — порядок создания; id стабилен.
#[derive(Clone, Debug, Default)]
pub struct Settlements {
    list: Vec<Settlement>,
    next_id: u64,
}

impl Settlements {
    pub fn len(&self) -> usize {
        self.list.len()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Settlement> {
        self.list.iter()
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Settlement> {
        self.list.iter_mut()
    }
    pub fn get(&self, id: u64) -> Option<&Settlement> {
        self.list.iter().find(|s| s.id == id)
    }
    pub fn get_mut(&mut self, id: u64) -> Option<&mut Settlement> {
        self.list.iter_mut().find(|s| s.id == id)
    }
    pub fn at_center(&self, center: TilePos) -> Option<&Settlement> {
        self.list.iter().find(|s| s.center == center)
    }
    /// Создаёт поселение и возвращает его id.
    pub fn create(&mut self, center: TilePos) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.list.push(Settlement::new(id, center));
        id
    }
    /// Загружает поселение из сейва, сохраняя его id.
    pub fn insert_loaded(&mut self, settlement: Settlement) {
        self.next_id = self.next_id.max(settlement.id + 1);
        self.list.push(settlement);
    }
    pub fn remove(&mut self, id: u64) -> Option<Settlement> {
        let index = self.list.iter().position(|s| s.id == id)?;
        Some(self.list.remove(index))
    }
    /// Убирает человека из всех поселений (смерть, переселение).
    pub fn remove_member_everywhere(&mut self, member: u64) {
        for settlement in &mut self.list {
            settlement.remove_member(member);
        }
    }
    pub fn memory_bytes(&self) -> u64 {
        let mut bytes = (self.list.capacity() as u64) * std::mem::size_of::<Settlement>() as u64;
        for settlement in &self.list {
            bytes += (settlement.members.capacity() as u64) * std::mem::size_of::<u64>() as u64;
        }
        bytes
    }
}
