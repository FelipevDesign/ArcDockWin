// Planos de fundo animados (wallpaper.rs). Um triângulo cobre a tela; o pixel shader desenha
// o estilo pedido. Movimento lento (períodos de minutos) e pontilhado contra faixas no degradê.
//
// Como os fundos dinâmicos do macOS, a cor segue o horário: `night` vai de 0 (dia) a 1 (noite)
// e `warm` dá o tom dourado do nascer e do pôr do sol. A cada 4 h o desenho muda (`seed`: formas
// e família de cores); na troca, `blend` faz a passagem do desenho anterior para o novo.

cbuffer Params : register(b0) {
    float2 res;     // tamanho da tela em px
    float time;     // segundos
    float night;    // 0 dia .. 1 noite
    float style;    // 0 ondas, 1 aurora, 2 seda
    float warm;     // 0 .. 1, luz dourada
    float seedA;    // desenho anterior
    float seedB;    // desenho atual
    float blend;    // 0 = só o anterior .. 1 = só o atual
    float3 pad;
};

struct VO { float4 pos : SV_Position; };

VO vs(uint id : SV_VertexID) {
    VO o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

float hash(float2 p) {
    float3 p3 = frac(float3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return frac((p3.x + p3.y) * p3.z);
}

float noise(float2 p) {
    float2 i = floor(p), f = frac(p);
    // Quíntica: derivada contínua, sem vincos na luz da seda.
    float2 u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    return lerp(lerp(hash(i), hash(i + float2(1, 0)), u.x),
                lerp(hash(i + float2(0, 1)), hash(i + float2(1, 1)), u.x), u.y);
}

// Número fixo (0..1) do desenho `seed`, canal `k`.
float rnd(float seed, float k) { return hash(float2(seed * 1.618 + 0.5, k * 7.31 + 1.7)); }

// Gira o matiz (rotação no plano de cor YIQ), mantendo o brilho.
float3 hue(float3 c, float a) {
    float3x3 toYIQ = float3x3(0.299, 0.587, 0.114, 0.596, -0.274, -0.322, 0.211, -0.523, 0.312);
    float3x3 toRGB = float3x3(1.0, 0.956, 0.621, 1.0, -0.272, -0.647, 1.0, -1.106, 1.703);
    float3 y = mul(toYIQ, c);
    float s = sin(a), co = cos(a);
    y.yz = float2(y.y * co - y.z * s, y.y * s + y.z * co);
    return mul(toRGB, y);
}

// Família de cores do desenho: lavanda (original), oceano, pôr do sol. Giros maiores saem do
// estilo do macOS (rosa vira verde).
float familyAngle(float seed) {
    float f = floor(rnd(seed, 9.0) * 3.0);
    return f < 1.0 ? 0.0 : (f < 2.0 ? -0.50 : 0.32);
}

// --- Ondas: camadas de ondas largas, com sombra suave entre elas. ---
float3 waves(float2 uv, float aspect, float t, float n, float seed) {
    float3 top = lerp(float3(0.40, 0.62, 1.00), float3(0.035, 0.04, 0.11), n);
    float3 mid = lerp(float3(0.36, 0.48, 0.98), float3(0.10, 0.09, 0.28), n);
    float3 col = lerp(top, mid, smoothstep(0.0, 0.7, uv.y));
    float3 L[6], D[6];
    L[0] = float3(0.30, 0.40, 0.96); L[1] = float3(0.50, 0.36, 0.95); L[2] = float3(0.20, 0.52, 1.00);
    L[3] = float3(0.70, 0.38, 0.92); L[4] = float3(1.00, 0.44, 0.64); L[5] = float3(1.00, 0.64, 0.38);
    D[0] = float3(0.17, 0.14, 0.46); D[1] = float3(0.30, 0.20, 0.64); D[2] = float3(0.24, 0.36, 0.86);
    D[3] = float3(0.50, 0.32, 0.80); D[4] = float3(0.86, 0.42, 0.62); D[5] = float3(0.98, 0.58, 0.40);
    // Forma do desenho: inclinação, altura e fase das ondas.
    float tilt = lerp(-0.26, 0.10, rnd(seed, 1.0));
    float amp = lerp(0.75, 1.30, rnd(seed, 2.0));
    float freq = lerp(0.75, 1.15, rnd(seed, 3.0));
    float ph = rnd(seed, 4.0) * 6.283;
    float x = uv.x;
    float xn = x / aspect;
    [unroll] for (int i = 0; i < 6; i++) {
        float fi = i;
        float e = 0.22 + fi * 0.122
            + amp * 0.080 * sin(x * 0.85 * freq + t * 0.050 + fi * 1.9 + ph)
            + amp * 0.036 * sin(x * 1.90 * freq - t * 0.037 + fi * 0.7 + ph * 1.7)
            + 0.012 * sin(x * 3.70 + t * 0.065 + fi * 2.3)
            + tilt * (xn - 0.5);
        float d = uv.y - e;
        // Antisserrilhado de um pixel medido na tela (fwidth): nos trechos inclinados a distância
        // vertical engana, e uma largura fixa deixa a borda em degraus. Só isso: borda nítida.
        float aa = fwidth(d) * 0.75;
        // Sombra da camada da frente sobre a de trás, nascendo suave já na borda.
        col *= 1.0 - 0.30 * exp(min(d, 0.0) * 24.0) * smoothstep(aa, -aa, d);
        float3 base = lerp(L[i], D[i], n) * (1.0 + 0.20 * (xn - 0.5));
        float3 lc = lerp(base * 1.12, base * 0.74, smoothstep(0.0, 0.38, d));
        // Brilho na crista: começa no meio da borda e some em ~12 px, sem linha dura.
        lc += lerp(0.05, 0.07, n) * exp(-max(d - aa, 0.0) * 70.0) * smoothstep(-aa, aa * 2.0, d);
        col = lerp(col, lc, smoothstep(-aa, aa, d));
    }
    return col;
}

// --- Aurora: manchas de cor grandes que se misturam e passeiam devagar. ---
float3 aurora(float2 uv, float aspect, float t, float n, float seed) {
    float2 p = float2(uv.x / aspect, uv.y);
    p += 0.035 * float2(sin(p.y * 5.0 + t * 0.11), cos(p.x * 4.0 - t * 0.09));
    float3 L[5], D[5];
    L[0] = float3(0.56, 0.40, 1.00); L[1] = float3(0.22, 0.58, 1.00); L[2] = float3(1.00, 0.42, 0.72);
    L[3] = float3(0.16, 0.80, 0.78); L[4] = float3(1.00, 0.62, 0.30);
    D[0] = float3(0.30, 0.10, 0.62); D[1] = float3(0.08, 0.30, 0.86); D[2] = float3(0.70, 0.16, 0.58);
    D[3] = float3(0.04, 0.48, 0.66); D[4] = float3(0.94, 0.42, 0.24);
    float3 sum = 0;
    float ws = 0;
    [unroll] for (int i = 0; i < 5; i++) {
        float fi = i;
        // Cada desenho espalha as manchas de outro jeito e com outros caminhos.
        float2 c = float2(rnd(seed, 10.0 + fi), rnd(seed, 20.0 + fi)) * 0.76 + 0.12;
        float2 r = float2(0.08, 0.06) + 0.08 * float2(rnd(seed, 30.0 + fi), rnd(seed, 40.0 + fi));
        float sp = 0.028 + 0.025 * rnd(seed, 50.0 + fi);
        float2 P = c + r * float2(sin(t * sp + fi * 2.1), cos(t * sp * 1.13 + fi * 1.3));
        float2 d = (p - P) * float2(aspect, 1.0);
        float w = 1.0 / pow(dot(d, d) * 6.0 + 0.04, 1.6);
        sum += lerp(L[i], D[i], n) * w;
        ws += w;
    }
    float3 col = sum / ws;
    // Véu de luz diagonal bem suave.
    float sheen = sin((p.x + p.y) * 3.0 + t * 0.06) * 0.5 + 0.5;
    col += lerp(0.05, 0.035, n) * sheen;
    return col;
}

// --- Seda: faixas largas de cetim que ondulam devagar, com brilho nas dobras. ---
float silkField(float2 p, float t, float2 dir, float seed) {
    // Deslocamento pequeno por desenho: coordenadas grandes perdem precisão e o ruído quadricula.
    float2 o = float2(rnd(seed, 70.0), rnd(seed, 71.0)) * 40.0;
    float n1 = noise(p * 0.9 + float2(t * 0.010, -t * 0.008) + o);
    float n2 = noise(p * 1.7 - float2(t * 0.006, t * 0.009) + 3.1 + o.yx);
    float w = n1 * 1.7 + n2 * 0.6;
    return sin(dot(p, dir) * 2.2 + w * 3.4 + t * 0.020);
}

float3 silk(float2 uv, float aspect, float t, float n, float seed) {
    float2 p = uv * 1.3;
    // Direção das dobras do desenho.
    float ang = lerp(0.55, 1.35, rnd(seed, 60.0));
    float2 dir = float2(cos(ang), sin(ang)) * 1.66;
    float f = silkField(p, t, dir, seed);
    float e = 0.01;
    float fx = silkField(p + float2(e, 0), t, dir, seed) - f;
    float fy = silkField(p + float2(0, e), t, dir, seed) - f;
    float3 nn = normalize(float3(-fx / e * 0.16, -fy / e * 0.16, 1.0));
    float3 l = normalize(float3(-0.5, -0.7, 0.6));
    float diff = saturate(dot(nn, l));
    float spec = pow(saturate(dot(reflect(-l, nn), float3(0, 0, 1))), 18.0);
    float3 a = lerp(float3(0.30, 0.46, 0.98), float3(0.07, 0.05, 0.20), n);
    float3 b = lerp(float3(0.62, 0.40, 0.98), float3(0.34, 0.16, 0.66), n);
    float3 c = lerp(float3(1.00, 0.50, 0.64), float3(0.88, 0.36, 0.60), n);
    float g = saturate(0.5 + 0.30 * f + 0.45 * (uv.x / aspect - 0.5) - 0.20 * (uv.y - 0.5));
    float3 base = g < 0.5 ? lerp(a, b, g * 2.0) : lerp(b, c, g * 2.0 - 1.0);
    return base * (0.62 + 0.45 * diff) + spec * lerp(0.16, 0.22, n);
}

float3 scene(float2 uv, float aspect, float seed) {
    float3 col;
    if (style < 0.5) col = waves(uv, aspect, time, night, seed);
    else if (style < 1.5) col = aurora(uv, aspect, time, night, seed);
    else col = silk(uv, aspect, time, night, seed);
    return saturate(hue(col, familyAngle(seed)));
}

float4 ps(VO i) : SV_Target {
    float2 uv = i.pos.xy / res.y;          // y de 0 a 1, x de 0 a aspect
    float aspect = res.x / res.y;
    float3 col = scene(uv, aspect, seedB);
    // Troca de desenho: passagem suave do anterior para o novo.
    if (blend < 1.0) col = lerp(scene(uv, aspect, seedA), col, blend);
    // Luz dourada do nascer/pôr do sol: brilho que sobe do horizonte (de baixo), sem tirar a cor.
    float glow = smoothstep(0.15, 1.0, i.pos.y / res.y) * (0.6 + 0.4 * sin(i.pos.x / res.x * 3.14159));
    col += warm * glow * float3(0.30, 0.14, 0.02);
    col = lerp(col, col * float3(1.06, 1.0, 0.94), warm * 0.5);
    // Vinheta leve e pontilhado (evita faixas no degradê).
    float2 c = i.pos.xy / res - 0.5;
    col *= 1.0 - 0.18 * dot(c, c);
    col += (hash(i.pos.xy + frac(time)) - 0.5) / 255.0 * 1.5;
    return float4(saturate(col), 1.0);
}
