// Crops the recorded region out of a captured desktop frame and converts it to sRGB BGRA8.
// FP16 scRGB frames are tone mapped as in DESIGN.md §4 with a fixed source peak; BGRA8 frames are copied.

cbuffer Params : register(b0)
{
    int2 region_origin;
    int2 source_max;
    float2 source_step;
    float2 texel_size;
    uint mode;
    float exposure_scale;
    float sdr_white_nits;
    float eetf_source_pq;
    float eetf_max_lum;
    float eetf_knee;
    uint resample;
    float unused;
};

Texture2D<float4> source : register(t0);
SamplerState linear_clamp : register(s0);

static const uint MODE_SDR = 0;
static const uint MODE_AUTO = 2;

static const float PQ_M1 = 2610.0 / 16384.0;
static const float PQ_M2 = 2523.0 / 4096.0 * 128.0;
static const float PQ_C1 = 3424.0 / 4096.0;
static const float PQ_C2 = 2413.0 / 4096.0 * 32.0;
static const float PQ_C3 = 2392.0 / 4096.0 * 32.0;

void vs_main(uint id : SV_VertexID, out float4 position : SV_Position)
{
    float2 corner = float2((id << 1) & 2, id & 2);
    position = float4(corner * float2(2, -2) + float2(-1, 1), 0, 1);
}

float pq_encode(float nits)
{
    float y = pow(saturate(nits / 10000.0), PQ_M1);
    return pow((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y), PQ_M2);
}

float pq_decode(float encoded)
{
    float p = pow(saturate(encoded), 1.0 / PQ_M2);
    return 10000.0 * pow(max(p - PQ_C1, 0.0) / (PQ_C2 - PQ_C3 * p), 1.0 / PQ_M1);
}

float3 fetch(float2 pixel_center)
{
    if (resample == 0)
    {
        int2 p = clamp(int2(pixel_center) + region_origin, int2(0, 0), source_max);
        return source.Load(int3(p, 0)).rgb;
    }
    float2 position = float2(region_origin) + pixel_center * source_step;
    return source.SampleLevel(linear_clamp, position * texel_size, 0).rgb;
}

float3 into_gamut(float3 x)
{
    float lowest = min(x.r, min(x.g, x.b));
    if (lowest >= 0.0)
        return x;
    float luminance = dot(x, float3(0.2126, 0.7152, 0.0722));
    if (luminance <= 0.0)
        return float3(0, 0, 0);
    return luminance + (x - luminance) * (luminance / (luminance - lowest));
}

float3 compress_highlights(float3 x)
{
    float m = max(x.r, max(x.g, x.b));
    if (mode != MODE_AUTO || m <= 0.0)
        return saturate(x);
    float e1 = pq_encode(m * sdr_white_nits) / eetf_source_pq;
    if (e1 <= eetf_knee)
        return saturate(x);
    float t = saturate((e1 - eetf_knee) / (1.0 - eetf_knee));
    float t2 = t * t;
    float t3 = t2 * t;
    float e2 = (2.0 * t3 - 3.0 * t2 + 1.0) * eetf_knee
             + (t3 - 2.0 * t2 + t) * (1.0 - eetf_knee)
             + (-2.0 * t3 + 3.0 * t2) * eetf_max_lum;
    float mapped = pq_decode(e2 * eetf_source_pq) / sdr_white_nits;
    return saturate(x * (mapped / m));
}

float srgb_oetf(float x)
{
    return x <= 0.0031308 ? 12.92 * x : 1.055 * pow(x, 1.0 / 2.4) - 0.055;
}

float srgb_eotf(float c)
{
    return c <= 0.04045 ? c / 12.92 : pow((c + 0.055) / 1.055, 2.4);
}

// Rounds to the 8-bit code whose linear-space midpoints bracket x, so SDR content round-trips exactly.
float quantize(float x)
{
    float code = clamp(round(srgb_oetf(x) * 255.0), 0.0, 255.0);
    if (code < 255.0 && x >= srgb_eotf((code + 0.5) / 255.0))
        code += 1.0;
    else if (code > 0.0 && x < srgb_eotf((code - 0.5) / 255.0))
        code -= 1.0;
    return code / 255.0;
}

float4 ps_main(float4 position : SV_Position) : SV_Target
{
    float3 color = fetch(position.xy);
    if (mode == MODE_SDR)
        return float4(color, 1);
    float3 x = compress_highlights(into_gamut(color * exposure_scale));
    return float4(quantize(x.r), quantize(x.g), quantize(x.b), 1);
}
