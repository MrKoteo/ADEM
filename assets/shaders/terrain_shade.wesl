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

// Обычный материал террейна без vertex color. Наклон поверхности считается
// из производных мировой позиции: нормали и лишние атрибуты вершин не нужны.
@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> @location(0) vec4<f32> {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    var color = pbr_input.material.base_color.rgb;

    let p = in.world_position.xyz;
    let n = normalize(cross(dpdx(p), dpdy(p)));

    // Тинт по абсолютной высоте: высокогорье уходит в холодный тон,
    // вершины светлеют. Нижние уровни не трогаем, чтобы не красить воду.
    let high = smoothstep(TERRAIN_ROCK_HEIGHT, TERRAIN_SNOW_HEIGHT, p.z);
    color = mix(color, color * vec3<f32>(0.92, 0.96, 1.06), high * 0.55);
    let peak = smoothstep(TERRAIN_SNOW_HEIGHT, TERRAIN_MAX_HEIGHT, p.z);
    color = mix(color, color * vec3<f32>(1.04, 1.04, 1.07), peak * 0.35);

    // Верхняя грань не темнеет; стенки — заметно. Две видимые стенки
    // (−X/−Y) различаются по яркости: одна чуть светлее, чтобы читался
    // наклон. abs(n.x) − abs(n.y) даёт устойчивый знак для обеих стенок.
    let wall = 1.0 - abs(n.z);
    let orientation = abs(n.x) - abs(n.y);
    let wall_shade = mix(0.30, 0.14, orientation * 0.5 + 0.5);
    let shade = 1.0 - wall * wall_shade;

    // Контактная тень у основания стенки. Для верхних граней `wall_t` = 1,
    // поэтому они не затемняются. Атрибут UV_1 есть только у terrain-мешей.
    var wall_t = 1.0;
    @if(VERTEX_UVS_B) {
        wall_t = in.uv_b.y;
    }
    let contact = 1.0 - 0.28 * (1.0 - smoothstep(0.0, 0.20, wall_t));

    color = color * shade * contact;

    return vec4<f32>(color, 1.0);
}
