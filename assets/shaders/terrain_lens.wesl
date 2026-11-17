import bevy_pbr::render::{
    forward_io::VertexOutput,
    pbr_fragment::pbr_input_from_standard_material,
};

// Пороговые высоты в мировых единицах. Дублируют константы симуляции:
// STONE_HEIGHT=24, SNOW_HEIGHT=142 при HEIGHT_WORLD_UNIT=1.
// При тюнинге высот обновлять вместе с `simulation/generation/height.rs`.
const TERRAIN_ROCK_HEIGHT: f32 = 24.0;
const TERRAIN_SNOW_HEIGHT: f32 = 142.0;
const TERRAIN_MAX_HEIGHT: f32 = 255.0;

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> @location(0) vec4<f32> {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    var color = pbr_input.material.base_color.rgb;

    let p = in.world_position.xyz;
    let n = normalize(cross(dpdx(p), dpdy(p)));

    // Тинт по абсолютной высоте: высокогорье холоднее, вершины светлее.
    let high = smoothstep(TERRAIN_ROCK_HEIGHT, TERRAIN_SNOW_HEIGHT, p.z);
    color = mix(color, color * vec3<f32>(0.92, 0.96, 1.06), high * 0.55);
    let peak = smoothstep(TERRAIN_SNOW_HEIGHT, TERRAIN_MAX_HEIGHT, p.z);
    color = mix(color, color * vec3<f32>(1.04, 1.04, 1.07), peak * 0.35);

    // Затенение по наклону: верхние грани не темнеют, стенки — заметно.
    // Две видимые стенки различаются по яркости для читаемости склона.
    let wall = 1.0 - abs(n.z);
    let orientation = abs(n.x) - abs(n.y);
    let wall_shade = mix(0.30, 0.14, orientation * 0.5 + 0.5);
    let shade = 1.0 - wall * wall_shade;

    // Контактная тень у основания стенки; верхние грани не затемняются.
    var wall_t = 1.0;
    @if(VERTEX_UVS_B) {
        wall_t = in.uv_b.y;
    }
    let contact = 1.0 - 0.28 * (1.0 - smoothstep(0.0, 0.20, wall_t));

    color = color * shade * contact;

    // Линза: приглушение контраста на подсвеченных чанках (vertex color).
    if in.color.r < 0.999 || in.color.g < 0.999 || in.color.b < 0.999 {
        color = (color - vec3<f32>(0.5)) * 0.75 + vec3<f32>(0.5);
    }

    return vec4<f32>(color, 1.0);
}
