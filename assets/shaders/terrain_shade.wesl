import bevy_pbr::render::{
    forward_io::VertexOutput,
    pbr_fragment::pbr_input_from_standard_material,
};

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
    // Верхняя грань не темнеет; стенки — заметно. side_light горизонтальный,
    // чтобы две видимые стенки (−X/−Y) различались по яркости.
    let wall = 1.0 - abs(n.z);
    let side_light = normalize(vec3<f32>(0.6, 0.4, 0.0));
    let side = abs(dot(n, side_light));
    let shade = 1.0 - 0.42 * wall * (1.0 - 0.5 * side);
    color = color * shade;

    return vec4<f32>(color, 1.0);
}
