//! Линзы карты: окраска terrain-мешей целевых чанков без отдельных
//! рендер-сущностей, поэтому линза не может перекрывать декор или сущности.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;

use crate::app::state::GameState;
use crate::simulation::objects::ObjectKind;
use crate::simulation::{CHUNK_SIZE, ChunkPos, TilePos, World as SimWorld, split_tile};
use super::terrain::{
    BlockPos, BuiltBlock, TerrainRender, block_tiles_for_lod, render_world,
    set_block_lens_colors,
};

/// Период обновления целевых чанков, секунд.
const SCAN_INTERVAL_SECS: f32 = 1.5;

/// Цвета линз: тёплый оранжевый для разумных, холодный синий для построек.
const SENTIENT_TINT: [f32; 3] = [1.0, 0.8, 0.2];
const BUILDING_TINT: [f32; 3] = [0.2, 0.8, 1.0];
const TINT_STRENGTH: f32 = 1.0;

#[derive(Resource, Default)]
struct LensRender {
    scan_cooldown: f32,
    last_flags: Option<(bool, bool)>,
}

pub struct LensPlugin;

impl Plugin for LensPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LensRender>()
            .add_systems(Update, update_map_lenses.after(render_world));
    }
}

impl SimWorld {
    fn lens_sentient_chunks(&self) -> HashSet<ChunkPos> {
        self.entities
            .iter()
            .filter(|entity| entity.mind.is_some())
            .map(|entity| {
                split_tile(TilePos {
                    x: entity.pos.x.floor() as i32,
                    y: entity.pos.y.floor() as i32,
                })
                .0
            })
            .collect()
    }

    fn chunk_has_building(&self, chunk: ChunkPos) -> bool {
        if chunk.x >= self.chunks_per_side || chunk.y >= self.chunks_per_side {
            return false;
        }
        self.chunk(chunk)
            .objects
            .values()
            .any(|object| matches!(object.kind, ObjectKind::Building(_)))
    }
}

fn update_map_lenses(
    time: Res<Time>,
    state: Res<GameState>,
    mut terrain: ResMut<TerrainRender>,
    mut lenses: ResMut<LensRender>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let flags = (state.lenses.sentients, state.lenses.buildings);
    lenses.scan_cooldown -= time.delta_secs();
    let scan_due = lenses.scan_cooldown <= 0.0 || lenses.last_flags != Some(flags);
    if !scan_due {
        return;
    }
    lenses.scan_cooldown = SCAN_INTERVAL_SECS;
    lenses.last_flags = Some(flags);
    // Без включённых линз terrain-меши не несут vertex color: перекрашивать
    // нечего, а сбрасывать накопленные оттенки нужно при выключении.
    if !flags.0 && !flags.1 {
        if !terrain.lens_tints.is_empty() {
            terrain.lens_tints = Arc::new(HashMap::new());
        }
        return;
    }
    if terrain.lens_mode != Some(true) {
        return;
    }

    let desired = state
        .world
        .as_ref()
        .map(|world| {
            let mut visible_chunks = HashSet::new();
            let active_step = block_tiles_for_lod(terrain.lod);
            for block in terrain.active.keys() {
                push_block_chunks(world, *block, active_step, &mut visible_chunks);
            }

            let sentients = if flags.0 {
                world.lens_sentient_chunks()
            } else {
                HashSet::new()
            };
            visible_chunks
                .into_iter()
                .filter_map(|chunk| {
                    let has_sentient = sentients.contains(&chunk);
                    let has_building = flags.1 && world.chunk_has_building(chunk);
                    let color = lens_color(has_sentient, has_building);
                    (has_sentient || has_building).then_some((chunk, color))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    update_terrain_tints(&mut terrain, &mut meshes, desired);
}

fn lens_color(has_sentient: bool, has_building: bool) -> [f32; 4] {
    let tint = match (has_sentient, has_building) {
        (true, true) => std::array::from_fn(|channel| {
            (SENTIENT_TINT[channel] + BUILDING_TINT[channel]) * 0.5
        }),
        (true, false) => SENTIENT_TINT,
        (false, true) => BUILDING_TINT,
        (false, false) => [1.0; 3],
    };
    [
        1.0 - TINT_STRENGTH + tint[0] * TINT_STRENGTH,
        1.0 - TINT_STRENGTH + tint[1] * TINT_STRENGTH,
        1.0 - TINT_STRENGTH + tint[2] * TINT_STRENGTH,
        1.0,
    ]
}

fn update_terrain_tints(
    terrain: &mut TerrainRender,
    meshes: &mut Assets<Mesh>,
    desired: HashMap<ChunkPos, [f32; 4]>,
) {
    let previous = &terrain.lens_tints;
    let changed_chunks: HashSet<ChunkPos> = previous
        .keys()
        .chain(desired.keys())
        .copied()
        .filter(|chunk| previous.get(chunk) != desired.get(chunk))
        .collect();
    if changed_chunks.is_empty() {
        return;
    }

    terrain.lens_tints = Arc::new(desired);
    recolor_scene(
        &terrain.active,
        terrain.lod,
        &changed_chunks,
        &terrain.lens_tints,
        meshes,
    );
    if let Some(incoming) = terrain.incoming.as_ref() {
        recolor_scene(
            &incoming.blocks,
            incoming.lod,
            &changed_chunks,
            &terrain.lens_tints,
            meshes,
        );
    }
}

/// Перекрашивает блоки одной сцены: её сетка блоков задаёт шаг в тайлах,
/// поэтому «затронутые чанки» проецируются на блоки именно этого LOD.
fn recolor_scene(
    scene: &HashMap<BlockPos, BuiltBlock>,
    scene_lod: u32,
    changed_chunks: &HashSet<ChunkPos>,
    tints: &HashMap<ChunkPos, [f32; 4]>,
    meshes: &mut Assets<Mesh>,
) {
    let step = block_tiles_for_lod(scene_lod);
    let chunks_per_block = step / CHUNK_SIZE as i32;
    for (&position, block) in scene {
        let chunk_x0 = (position.x * step).div_euclid(CHUNK_SIZE as i32);
        let chunk_y0 = (position.y * step).div_euclid(CHUNK_SIZE as i32);
        let touched = (0..chunks_per_block).any(|y| {
            (0..chunks_per_block).any(|x| {
                changed_chunks.contains(&ChunkPos {
                    x: (chunk_x0 + x) as u32,
                    y: (chunk_y0 + y) as u32,
                })
            })
        });
        if touched {
            recolor_block(block, position, step, tints, meshes);
        }
    }
}

fn recolor_block(
    block: &BuiltBlock,
    position: BlockPos,
    step: i32,
    tints: &HashMap<ChunkPos, [f32; 4]>,
    meshes: &mut Assets<Mesh>,
) {
    if block.fallback || block.lod > 4 {
        return;
    }
    if let Some(handle) = &block.mesh && let Some(mut mesh) = meshes.get_mut(handle) {
        set_block_lens_colors(&mut mesh, position, step, tints);
    }
}

fn push_block_chunks(
    world: &SimWorld,
    block: BlockPos,
    step: i32,
    out: &mut HashSet<ChunkPos>,
) {
    let chunks_per_block = step / CHUNK_SIZE as i32;
    let chunk_x0 = (block.x * step).div_euclid(CHUNK_SIZE as i32);
    let chunk_y0 = (block.y * step).div_euclid(CHUNK_SIZE as i32);
    for y in 0..chunks_per_block {
        for x in 0..chunks_per_block {
            let chunk_x = chunk_x0 + x;
            let chunk_y = chunk_y0 + y;
            if chunk_x >= 0
                && chunk_y >= 0
                && chunk_x < world.chunks_per_side as i32
                && chunk_y < world.chunks_per_side as i32
            {
                out.insert(ChunkPos {
                    x: chunk_x as u32,
                    y: chunk_y as u32,
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/lenses.rs"]
mod tests;
