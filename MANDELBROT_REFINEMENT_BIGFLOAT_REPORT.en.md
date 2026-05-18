# Mandelbrot Deep Refinement and Zust BigFloat GPU Notes

This document records a Mandelbrot deep-zoom experiment built with Zust, Metal GPU execution, and GPU-side BigFloat arithmetic. The goal was not to render a familiar shallow Mandelbrot image, but to start from high-precision coordinates and observe where `f32`, roughly `f64`-level precision, and higher-limb BigFloat arithmetic break down or begin to reveal new structure.

## 1. Starting Coordinates

The main exploration point was:

```text
x = -0.744047327885618596691631635069778759229
y =  0.1098916491492589420042574690894010907545
step = 0.0000000000000000000009947598299507502772273195852327151970156
```

The step is approximately `9.9475982995e-22`. At this scale, rendering is no longer just a matter of running the Mandelbrot recurrence. The system must preserve enough center-coordinate precision, add pixel offsets without losing them, run enough iterations for slow-escaping points, and display coordinate values honestly according to the selected GPU precision.

## 2. Precision Findings

### f32

In `f32` mode, the displayed values are quantized to what `f32` can actually represent:

```text
x    ~= -0.744047343
y    ~=  0.109891645
step ~=  0.000000000000000000000497379905
```

At `x ~= -0.744`, `f32` has only around 7 to 9 decimal digits of useful precision. The pixel step is far smaller than the coordinate resolution, so many pixels map to nearly identical complex values. `f32` is useful only as a shallow preview mode here.

### BigFloat<2>

`BigFloat<2>` has about a 64-bit mantissa:

```text
x    ~= -0.744047327885618596781
y    ~=  0.10989164914925894195
step ~=  0.000000000000000000000497379914975375136967
```

It behaves like a double-precision-class renderer, though it is not IEEE `f64`. It preserves much more than `f32`, but still runs out of useful resolution at this zoom depth.

### BigFloat<4>

`BigFloat<4>` has about a 128-bit mantissa:

```text
x    ~= -0.7440473278856185967907907034287632824382
y    ~=  0.1098916491492589419847261374429344422415
step ~=  0.0000000000000000000004973799149753751386136597926163575984281
```

This is where the region begins to show real deep-zoom structure. Pixel offsets remain meaningful, and the rendered image develops visible filaments and spiral textures.

## 3. Iteration Count

The experiment also showed that missing detail was not only a precision problem. With `1000` iterations, many slow-escaping points were still misclassified as interior or displayed as flat regions. Increasing the iteration count made the boundary structure appear.

The current renderer exposes four iteration presets:

```text
500    Fast scouting mode
1000   Normal preview
5000   Default deep inspection
10000  Final-quality output
```

The practical workflow is to use `500` for quick navigation, then raise the count to `5000` or `10000` before saving an important image.

## 4. Sampling Strategy

The renderer initially used supersampling:

```text
2049 x 2049 sample buffer
3 x 3 sample average per final pixel
```

This produced smoother images, but it also blurred the very boundary details we were trying to inspect. The renderer was changed to:

```text
1024 x 1024 sample buffer
one sample per output pixel
```

This makes edges sharper and reduces the GPU buffer size by more than 4x, leaving more budget for higher iteration counts.

## 5. Server/Client Split

The system is split into a rendering server and output clients:

- The server stores view state, updates coordinates, runs Zust GPU kernels, and writes PNG images.
- The clients only display images and send user interactions.
- Android, H5, DApp, and mini-program clients can all be output endpoints.
- The client never computes Mandelbrot coordinates, avoiding JavaScript `Number`, `f32`, or platform-specific numeric truncation.

Each client has a unique id. The server stores that client's current view in Redis using msgpack rather than JSON strings.

The stored state includes:

```text
center_x
center_y
step
max_iter
precision
history
```

The server stores coordinates and step at the system maximum precision, currently `BigFloat<16>`. Rendering with `f32` or `BigFloat<2>` does not degrade the stored state.

## 6. Interaction Semantics

The current browser client follows these rules:

- Single click: move the center to the clicked point.
- Double click: move the center and halve `step`.
- Right click: return to the previous server-side view state.
- While a render request is pending, the image is locked so the user cannot queue inconsistent clicks.
- Refreshing the page sends only the client id, so the server restores Redis state.
- Manual coordinate entry only overrides state when the user presses the render button.

All position updates happen on the server using BigFloat arithmetic. `step >> 1` is implemented server-side, not with JavaScript number math.

## 7. GPU Precision Modes

The page currently supports:

```text
f32
BigFloat<2>
BigFloat<4>
BigFloat<6>
BigFloat<8>
BigFloat<10>
BigFloat<12>
BigFloat<14>
BigFloat<16>
```

`f32` uses a dedicated `mandelbrot_f32.zs` kernel. `BigFloat<N>` uses a generic `mandelbrot.zs` kernel:

```zust
pub struct Params<N> {
    x: bigfloat::BigFloat<N>,
    y: bigfloat::BigFloat<N>,
    step: bigfloat::BigFloat<N>,
    max_iter: u32,
}

pub fn main<N>(params: Params<N>, buf: Vec<f32>) {
    ...
}
```

The Rust server asks the VM for `gpu_struct_layout("mandelbrot::Params", [ConstInt(N)])`, packs the parameters, and runs the kernel through `gpu::metal_run(options)`.

## 8. Display Precision vs Stored Precision

The UI displays `x`, `y`, and `step` according to the selected GPU precision:

- `f32` shows about 9 significant decimal digits.
- `BigFloat<2>` shows a 64-bit-mantissa-level value.
- `BigFloat<4>` shows a 128-bit-mantissa-level value.
- `BigFloat<16>` shows the high-precision server state.

This is important because showing too many decimal digits while rendering with `f32` would be misleading. The UI should communicate the precision actually used by the current GPU kernel.

## 9. Zust BigFloat Capability

`zusts/bigfloat.zs` defines:

```zust
pub struct BigFloat<N> {
    sign: bool,
    exp: i32,
    data: [u32; N],
}
```

The mantissa has approximately `32 * N` bits:

```text
BigFloat<2>   64-bit mantissa
BigFloat<4>   128-bit mantissa
BigFloat<8>   256-bit mantissa
BigFloat<16>  512-bit mantissa
```

The Mandelbrot kernel already uses GPU-side BigFloat construction, addition, subtraction, multiplication, comparison, and conversion back to `f32` for smooth coloring.

## 10. Normalization Bug Fixed

During the experiment, `BigFloat<8>` initially produced all-black images. The root cause was not Metal or BigFloat itself. It was a server-side normalization bug: old Redis values stored with fewer limbs were expanded to more limbs without shifting the mantissa and adjusting the exponent.

The fix canonicalizes every nonzero mantissa:

- If the mantissa is too large, shift right and increase `exp`.
- If the mantissa is too small, shift left and decrease `exp`.
- Normalize Redis-loaded state to `BigFloat<16>`.
- Convert to the selected `BigFloat<N>` only when packing GPU parameters.

After this fix, `BigFloat<8>`, `BigFloat<10>`, and `BigFloat<16>` rendered correctly.

## 11. Conclusion

The deep Mandelbrot workflow depends on several parameters working together:

- `f32` and double-class precision are not enough at this coordinate scale.
- `BigFloat<4>` begins to reveal the hidden structure.
- Higher `BigFloat<N>` values preserve room for further zooming.
- `1000` iterations can hide slow-escaping detail.
- `5000` and `10000` iterations are better for final output.
- Single-sample rendering is better for scouting fine structure than heavy averaging.
- The client must remain an output endpoint; the server must own the high-precision state.

The key result is that Zust can compile and run generic BigFloat Mandelbrot kernels on Metal, and the resulting system can explore Mandelbrot regions beyond ordinary floating-point precision.

## 12. Visual Evidence: Controls and Intermediate Images
These images are kept once, as evidence for the precision and iteration claims above.

### 12.1 f32 Collapse
![f32 precision collapse](docs/assets/mandelbrot-f32-collapse.png)

### 12.2 BigFloat<2> Collapse
![BigFloat2 precision collapse](docs/assets/mandelbrot-bf2-collapse.png)

### 12.3 Low-Iteration Boundary
![low iteration boundary](docs/assets/mandelbrot-low-iteration-boundary.png)

### 12.4 BigFloat<4> Detail Recovery
![BigFloat4 detailed field](docs/assets/mandelbrot-bf4-detail.png)

### 12.5 High-Precision Spiral Field
![BigFloat spiral field](docs/assets/mandelbrot-bf4-spiral-field.png)

## 13. Step-by-Step NFT Exploration Results
The following images were not generated from external preset coordinates. They were produced from the current high-precision browser view by repeatedly running a 500-iteration preview, analyzing image complexity, selecting a dense boundary point, double-click zooming, increasing BigFloat precision when needed, and saving the selected state at 10000 iterations. Each image is appended here immediately before the next search step starts.

Important: when precision is insufficient, the displayed `x/y/step` values are already quantized to that low-precision mode, so the original high-precision coordinates are missing from that failed view. The successful exploration chain is preserved by the server-side `BigFloat<16>` state.

### 13.1 Deep Zoom Trace 01
![Deep Zoom Trace 01](docs/assets/nft-explore-01-bf4-10000.png)
Starting from the current bf4 deep view, the 500-iteration preview collapsed as all-interior, so the step was judged at 5000 iterations before saving this filament field. Selected pixel (168, 472), preview_iter=5000, score=1509.9, step=0.0000000000000000000002486899574876875693068298963081787990547.

```text
asset = docs/assets/nft-explore-01-bf4-10000.png
parent_preview = docs/assets/nft-explore-101-bf4-5000.png
click = (168, 472)
center_x = -0.7440473278856185969618893941802923301194
center_y = 0.1098916491492589420046213340419494477849
step = 0.0000000000000000000002486899574876875693068298963081787990547
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 1509.85
render_time_ms = 6004.7
```

### 13.2 Deep Zoom Trace 02
![Deep Zoom Trace 02](docs/assets/nft-explore-02-bf4-10000.png)
The second step moves along the high-complexity boundary toward the left, revealing broader slow-escape green regions and several small spiral arms. Selected pixel (72, 456), preview_iter=5000, score=1386.6, step=0.0000000000000000000001243449787438437846534149481540893994477.

```text
asset = docs/assets/nft-explore-02-bf4-10000.png
parent_preview = docs/assets/nft-explore-102-bf4-5000.png
click = (72, 456)
center_x = -0.7440473278856185970713129754748748606138
center_y = 0.109891649149258942018547971661259951667
step = 0.0000000000000000000001243449787438437846534149481540893994477
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 1386.56
render_time_ms = 5291.9
```

### 13.3 Deep Zoom Trace 03
![Deep Zoom Trace 03](docs/assets/nft-explore-03-bf4-10000.png)
This jump selects an upper dense boundary, where fan-like radial texture begins to form and confirms that halving the step is still worthwhile. Selected pixel (504, 200), preview_iter=5000, score=1562.8, step=0.00000000000000000000006217248937192189232670747407704469964421.

```text
asset = docs/assets/nft-explore-03-bf4-10000.png
parent_preview = docs/assets/nft-explore-103-bf4-5000.png
click = (504, 200)
center_x = -0.7440473278856185970723077353048256108907
center_y = 0.1098916491492589420573436050293392124769
step = 0.00000000000000000000006217248937192189232670747407704469964421
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 1562.81
render_time_ms = 5496.3
```

### 13.4 Deep Zoom Trace 04
![Deep Zoom Trace 04](docs/assets/nft-explore-04-bf4-10000.png)
The fourth image enters a more even field of broken coastline detail, with the local density continuing to rise. Selected pixel (280, 168), preview_iter=5000, score=1628.1, step=0.0000000000000000000000310862446859609461633537370385223498221.

```text
asset = docs/assets/nft-explore-04-bf4-10000.png
parent_preview = docs/assets/nft-explore-104-bf4-5000.png
click = (280, 168)
center_x = -0.7440473278856185970867317528391114899127
center_y = 0.109891649149258942078730941373280343437
step = 0.0000000000000000000000310862446859609461633537370385223498221
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 1628.12
render_time_ms = 5757.2
```

### 13.5 Deep Zoom Trace 05
![Deep Zoom Trace 05](docs/assets/nft-explore-05-bf4-10000.png)
This frame preserves a useful contrast between smooth slow-escape ground and dense boundary grain, with repeated bright centers beginning to appear. Selected pixel (456, 520), preview_iter=5000, score=1634.9, step=0.00000000000000000000001554312234298047308167686851926117491105.

```text
asset = docs/assets/nft-explore-05-bf4-10000.png
parent_preview = docs/assets/nft-explore-105-bf4-5000.png
click = (456, 520)
center_x = -0.744047327885618597088472582541525302898
center_y = 0.1098916491492589420784822514157926558685
step = 0.00000000000000000000001554312234298047308167686851926117491105
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 1634.93
render_time_ms = 5620.2
```

### 13.6 Deep Zoom Trace 06
![Deep Zoom Trace 06](docs/assets/nft-explore-06-bf4-10000.png)
The sixth step has a lower score, but its spiral arms and radial nuclei remain clear enough to keep the path alive. Selected pixel (568, 520), preview_iter=5000, score=1393.0, step=0.000000000000000000000007771561171490236540838434259630587375872.

```text
asset = docs/assets/nft-explore-06-bf4-10000.png
parent_preview = docs/assets/nft-explore-106-bf4-5000.png
click = (568, 520)
center_x = -0.7440473278856185970876021676903183964039
center_y = 0.1098916491492589420783579064370488120828
step = 0.000000000000000000000007771561171490236540838434259630587375872
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 1393.05
render_time_ms = 5705.9
```

### 13.7 Deep Zoom Trace 07
![Deep Zoom Trace 07](docs/assets/nft-explore-07-bf4-10000.png)
The seventh image re-enters a dense boundary cluster; the preview score jumps and the frame gains tighter spirals and granular branches. Selected pixel (520, 488), preview_iter=5000, score=2032.8, step=0.000000000000000000000003885780585745118270419217129815293687936.

```text
asset = docs/assets/nft-explore-07-bf4-10000.png
parent_preview = docs/assets/nft-explore-107-bf4-5000.png
click = (520, 488)
center_x = -0.7440473278856185970875399952009464745125
center_y = 0.1098916491492589420785444239051645777599
step = 0.000000000000000000000003885780585745118270419217129815293687936
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 2032.76
render_time_ms = 6082.2
```

### 13.8 Deep Zoom Trace 08
![Deep Zoom Trace 08](docs/assets/nft-explore-08-bf4-10000.png)
The eighth image zooms along the lower boundary, where a larger spiral arm cuts through the frame and a stronger radial center appears near the corner. Selected pixel (40, 840), preview_iter=5000, score=2186.8, step=0.000000000000000000000001942890292872559135209608564907646843968.

```text
asset = docs/assets/nft-explore-08-bf4-10000.png
parent_preview = docs/assets/nft-explore-108-bf4-5000.png
click = (40, 840)
center_x = -0.7440473278856185970893740836374181703349
center_y = 0.1098916491492589420772698878730401789689
step = 0.000000000000000000000001942890292872559135209608564907646843968
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 2186.84
render_time_ms = 6038.0
```

### 13.9 Deep Zoom Trace 09
![Deep Zoom Trace 09](docs/assets/nft-explore-09-bf4-10000.png)
The ninth image has the highest complexity score in this run, with several boundary clusters unfolding at once, making it the strongest NFT candidate. Selected pixel (536, 344), preview_iter=5000, score=2228.5, step=0.000000000000000000000000971445146436279567604804282453823421984.

```text
asset = docs/assets/nft-explore-09-bf4-10000.png
parent_preview = docs/assets/nft-explore-109-bf4-5000.png
click = (536, 344)
center_x = -0.7440473278856185970893274542703892289163
center_y = 0.1098916491492589420775962934422427689016
step = 0.000000000000000000000000971445146436279567604804282453823421984
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 2228.51
render_time_ms = 5932.9
```

### 13.10 Deep Zoom Trace 10
![Deep Zoom Trace 10](docs/assets/nft-explore-10-bf4-10000.png)
The final image follows that high-density region inward, keeping the tension between broad slow-escape fields and surrounding filaments. Selected pixel (488, 536), preview_iter=5000, score=2080.3, step=0.000000000000000000000000485722573218139783802402141226911710992.

```text
asset = docs/assets/nft-explore-10-bf4-10000.png
parent_preview = docs/assets/nft-explore-110-bf4-5000.png
click = (488, 536)
center_x = -0.7440473278856185970893507689539036996256
center_y = 0.1098916491492589420775729787587282981923
step = 0.000000000000000000000000485722573218139783802402141226911710992
precision = bf4
max_iter = 10000
preview_iter = 5000
preview_score = 2080.33
render_time_ms = 5917.2
```
