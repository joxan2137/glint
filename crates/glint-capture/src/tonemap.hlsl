// DESIGN.md section 4 on the GPU, mirroring glint-core::tonemap operation for operation in fp32 (same table lookups,
// same evaluation order) so SDR content comes out bit-exact and HDR content within one code of the CPU result.
// `precise` keeps the compiler from fusing multiply-adds or reassociating.

cbuffer Params : register(b0)
{
    uint width;
    uint height;
    uint percentile_rank; // ceil(pixels * 0.999)
    uint auto_mode;       // 1 = BT.2390 highlight compression, 0 = clip
    float scale;          // 80 / sdr_white_nits * 2^exposure
    float normalization;  // 80 / sdr_white_nits, for the statistics
    float hdr_threshold;  // 1 + 0.5 / 255
    float exposure;       // 2^exposure_stops
    float sdr_white_nits;
    float lut_step;       // 24 / 4095
    float2 unused;
};

Texture2D<float4> source : register(t0);
StructuredBuffer<float> tables : register(t1);  // [0, 255) sRGB midpoints, [255, 2304) mantissa log2 table
StructuredBuffer<uint> aux_in : register(t2);   // [0] curve enabled, [1, 4097) m -> m' table over log2(m) in [-16, 8] as float bits
RWStructuredBuffer<uint> stats : register(u0);  // [0, 2048) histogram, 2048 hdr count, 2049 out of gamut count, 2050 max bits
RWStructuredBuffer<uint> output : register(u1); // packed BGRA8
RWStructuredBuffer<uint> aux : register(u2);    // same layout as aux_in, written by curve_main

static const uint MANTISSA_LOG2 = 255;
static const uint HISTOGRAM_BINS = 2048;
static const uint STAT_HDR_COUNT = 2048;
static const uint STAT_GAMUT_COUNT = 2049;
static const uint STAT_MAX_BITS = 2050;
static const uint CURVE_LUT_SIZE = 4096;
static const uint CURVE_LUT_LAST = 4095;
static const float LOG_MIN = -16.0;
static const float MIN_CURVE_INPUT = 1.52587890625e-05; // 2^-16

float sanitize(float v)
{
    return (v != v) ? 0.0 : clamp(v, -65504.0, 65504.0);
}

float3 load_pixel(uint2 p)
{
    float4 raw = source.Load(int3(p, 0));
    return float3(sanitize(raw.r), sanitize(raw.g), sanitize(raw.b));
}

float fast_log2(float value)
{
    uint bits = asuint(value);
    int exponent = int((bits >> 23) & 0xff) - 127;
    uint mantissa = bits & 0x7fffff;
    uint index = (mantissa >> 12) & 0x7ff;
    float fraction = float(mantissa & 0xfff) / 4096.0;
    precise float low = tables[MANTISSA_LOG2 + index];
    precise float high = tables[MANTISSA_LOG2 + index + 1];
    precise float result = float(exponent) + low + (high - low) * fraction;
    return result;
}

uint histogram_bin(float maximum)
{
    if (maximum <= 0.0)
        return 0;
    precise float position = ((fast_log2(maximum) - LOG_MIN) * 2048.0) / 24.0;
    return (uint)clamp(position, 0.0, 2047.0);
}

groupshared uint local_histogram[2048];
groupshared uint local_hdr;
groupshared uint local_gamut;
groupshared uint local_max;

[numthreads(16, 16, 1)]
void stats_main(uint3 id : SV_DispatchThreadID, uint index : SV_GroupIndex)
{
    for (uint i = index; i < HISTOGRAM_BINS; i += 256)
        local_histogram[i] = 0;
    if (index == 0)
    {
        local_hdr = 0;
        local_gamut = 0;
        local_max = 0;
    }
    GroupMemoryBarrierWithGroupSync();

    if (id.x < width && id.y < height)
    {
        float3 c = load_pixel(id.xy);
        precise float maximum = max(max(max(c.r, c.g), c.b), 0.0) * normalization;
        InterlockedAdd(local_histogram[histogram_bin(maximum)], 1);
        if (maximum > hdr_threshold)
            InterlockedAdd(local_hdr, 1);
        if (c.r < 0.0 || c.g < 0.0 || c.b < 0.0)
            InterlockedAdd(local_gamut, 1);
        InterlockedMax(local_max, asuint(maximum));
    }
    GroupMemoryBarrierWithGroupSync();

    for (uint j = index; j < HISTOGRAM_BINS; j += 256)
    {
        uint count = local_histogram[j];
        if (count != 0)
            InterlockedAdd(stats[j], count);
    }
    if (index == 0)
    {
        if (local_hdr != 0)
            InterlockedAdd(stats[STAT_HDR_COUNT], local_hdr);
        if (local_gamut != 0)
            InterlockedAdd(stats[STAT_GAMUT_COUNT], local_gamut);
        InterlockedMax(stats[STAT_MAX_BITS], local_max);
    }
}

static const float PQ_M1 = 0.1593017578125;   // 2610 / 16384
static const float PQ_M2 = 78.84375;          // 2523 / 32
static const float PQ_C1 = 0.8359375;         // 3424 / 4096
static const float PQ_C2 = 18.8515625;        // 2413 / 128
static const float PQ_C3 = 18.6875;           // 2392 / 128

float pq_encode(float nits)
{
    float luminance = pow(max(nits, 0.0) / 10000.0, PQ_M1);
    return pow((PQ_C1 + PQ_C2 * luminance) / (1.0 + PQ_C3 * luminance), PQ_M2);
}

float pq_decode(float signal)
{
    float power = pow(max(signal, 0.0), 1.0 / PQ_M2);
    float numerator = max(power - PQ_C1, 0.0);
    float denominator = max(PQ_C2 - PQ_C3 * power, 1.17549435e-38);
    return 10000.0 * pow(numerator / denominator, 1.0 / PQ_M1);
}

groupshared uint curve_enabled;
groupshared float source_white;
groupshared float target_white;

float bt2390(float maximum)
{
    if (source_white <= 0.0)
        return min(maximum, 1.0);
    float normalized = clamp(pq_encode(maximum * sdr_white_nits) / source_white, 0.0, 1.0);
    float maximum_luminance = clamp(target_white / source_white, 0.0, 1.0);
    float knee = max(1.5 * maximum_luminance - 0.5, 0.0);
    if (normalized <= knee || knee >= 1.0)
        return min(maximum, 1.0);
    float t = (normalized - knee) / (1.0 - knee);
    float t2 = t * t;
    float t3 = t2 * t;
    float mapped = (2.0 * t3 - 3.0 * t2 + 1.0) * knee
                 + (t3 - 2.0 * t2 + t) * (1.0 - knee)
                 + (-2.0 * t3 + 3.0 * t2) * maximum_luminance;
    return clamp(pq_decode(mapped * source_white) / sdr_white_nits, 0.0, 1.0);
}

// Reduces the histogram to the 99.9th percentile peak and, in Auto mode with HDR content, builds the m -> m' table.
[numthreads(256, 1, 1)]
void curve_main(uint index : SV_GroupIndex)
{
    if (index == 0)
    {
        uint cumulative = 0;
        uint bin = 0;
        [loop] for (uint i = 0; i < HISTOGRAM_BINS; ++i)
        {
            cumulative += stats[i];
            if (cumulative >= percentile_rank)
            {
                bin = i;
                break;
            }
        }
        float maximum = asfloat(stats[STAT_MAX_BITS]);
        float peak = (maximum <= 0.0) ? 0.0 : exp2(LOG_MIN + (float(bin) + 0.5) * (24.0 / 2048.0));
        float source_peak = peak * exposure;
        curve_enabled = (auto_mode != 0 && source_peak > hdr_threshold) ? 1 : 0;
        source_white = pq_encode(source_peak * sdr_white_nits);
        target_white = pq_encode(sdr_white_nits);
        aux[0] = curve_enabled;
    }
    GroupMemoryBarrierWithGroupSync();
    if (curve_enabled != 0)
    {
        for (uint j = index; j < CURVE_LUT_SIZE; j += 256)
            aux[1 + j] = asuint(bt2390(exp2(LOG_MIN + float(j) * lut_step)));
    }
}

float3 into_gamut(float3 x)
{
    float lowest = min(min(x.r, x.g), x.b);
    if (lowest >= 0.0)
        return x;
    precise float luminance = 0.2126 * x.r + 0.7152 * x.g + 0.0722 * x.b;
    if (luminance <= 0.0)
        return float3(0.0, 0.0, 0.0);
    precise float saturation = luminance / (luminance - lowest);
    precise float3 mapped;
    mapped.r = luminance + (x.r - luminance) * saturation;
    mapped.g = luminance + (x.g - luminance) * saturation;
    mapped.b = luminance + (x.b - luminance) * saturation;
    return mapped;
}

float curve_map(float maximum)
{
    if (maximum <= MIN_CURVE_INPUT)
        return maximum;
    precise float position = clamp(((fast_log2(maximum) - LOG_MIN) * 4095.0) / 24.0, 0.0, 4095.0);
    uint lower = (uint)position;
    uint upper = min(lower + 1, CURVE_LUT_LAST);
    precise float fraction = position - float(lower);
    precise float low = asfloat(aux_in[1 + lower]);
    precise float high = asfloat(aux_in[1 + upper]);
    precise float mapped = low + (high - low) * fraction;
    return mapped;
}

// Number of sRGB midpoints at or below `linear`, which is the 8-bit code that round-trips SDR content.
uint quantize(float linear_value)
{
    float v = clamp(linear_value, 0.0, 1.0);
    if (v >= 1.0)
        return 255;
    uint low = 0;
    uint high = 255;
    [loop] while (low < high)
    {
        uint mid = (low + high) >> 1;
        if (v >= tables[mid])
            low = mid + 1;
        else
            high = mid;
    }
    return low;
}

[numthreads(16, 16, 1)]
void tonemap_main(uint3 id : SV_DispatchThreadID)
{
    if (id.x >= width || id.y >= height)
        return;

    precise float3 x = load_pixel(id.xy) * scale;
    x = into_gamut(x);
    if (aux_in[0] != 0)
    {
        float maximum = max(max(x.r, x.g), x.b);
        if (maximum > 0.0)
        {
            precise float ratio = curve_map(maximum) / maximum;
            x.r = x.r * ratio;
            x.g = x.g * ratio;
            x.b = x.b * ratio;
        }
    }
    output[id.y * width + id.x] = quantize(x.b) | (quantize(x.g) << 8) | (quantize(x.r) << 16) | 0xff000000u;
}
