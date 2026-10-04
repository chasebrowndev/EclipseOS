// SPDX-License-Identifier: AGPL-3.0-only
void main() {
    float s = clamp(shown(), 0.0, 1.0);
    vec2 centre = content.xy + content.zw * 0.5;
    vec2 uv = centre + (v_coords - centre) / (0.94 + 0.06 * s);
    vec2 spread = content.zw / size * (1.0 - s) * 9.0;
    vec4 acc = vec4(0.0);
    for (int i = -2; i <= 2; i++) {
        for (int j = -2; j <= 2; j++) {
            acc += texture2D(tex, uv + vec2(float(i), float(j)) * spread);
        }
    }
    gl_FragColor = acc / 25.0 * s * alpha;
}
