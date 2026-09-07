//_DEFINES_

precision highp float;

uniform float alpha;
// Element size in physical pixels (window size inflated by `thickness` on
// every side), thickness of the ring, the window's own corner radius (so
// the border follows rounded corners), and the two gradient endpoint
// colors plus its direction, all supplied by `BorderRenderElement::draw`
// (see src/border.rs).
uniform vec2 size;
uniform float thickness;
uniform float innerRadius;
uniform vec4 color1;
uniform vec4 color2;
uniform float angle;

varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Signed distance from `pos` to a `size`-sized rounded rect centered on the
// origin (Inigo Quilez's `sdRoundedBox`): negative inside, positive outside.
float roundedBoxDist(vec2 pos, vec2 size, float radius) {
    vec2 q = abs(pos) - (size - radius);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
    vec2 pos = v_coords * size - size * 0.5;

    // Outer edge of the ring: the padded element bounds themselves. Inner
    // edge: the window's own bounds, i.e. the outer bounds inset by
    // `thickness` on every side.
    float outerDist = roundedBoxDist(pos, size * 0.5, innerRadius + thickness);
    float innerDist = roundedBoxDist(pos, size * 0.5 - vec2(thickness), innerRadius);

    float outerMask = 1.0 - smoothstep(-1.0, 1.0, outerDist);
    float innerMask = smoothstep(-1.0, 1.0, innerDist);
    float mask = outerMask * innerMask;

    vec2 dir = vec2(cos(angle), sin(angle));
    float t = clamp(dot(v_coords - 0.5, dir) + 0.5, 0.0, 1.0);
    vec4 color = mix(color1, color2, t);

    color = vec4(color.rgb * color.a, color.a) * alpha * mask;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
