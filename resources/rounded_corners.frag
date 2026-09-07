//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
// Element size in physical pixels and corner radius in the same unit,
// supplied by `RoundedWindowRenderElement::draw` (see src/rounded_corners.rs).
uniform vec2 size;
uniform float radius;

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
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    vec2 pos = v_coords * size;
    float dist = roundedBoxDist(pos - size * 0.5, size * 0.5, radius);
    float mask = 1.0 - smoothstep(-1.0, 1.0, dist);

    color = color * alpha * mask;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
