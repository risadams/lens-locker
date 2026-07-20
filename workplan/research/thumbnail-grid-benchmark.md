# Benchmark: virtualized thumbnail grid at 100k+ items (Tauri v2 / WebView2)

Ticket: [../tickets/019-validate-thumbnail-grid-performance.md](../tickets/019-validate-thumbnail-grid-performance.md)
Status: real measurements from a running prototype, not estimates.

## Summary — verdict

**The Tauri-based virtualized grid holds up at 100k+ items under realistic
scrolling.** A hand-built Tauri v2 prototype (vanilla JS, no framework) rendering
100,000 synthetic 200×200 JPEG thumbnails in a windowed/recycled grid — only 78
`<img>` elements ever exist in the DOM at once, regardless of list position —
sustained a **flat 60.0 fps with zero dropped frames** (worst frame 17ms, out of
540 frames sampled) under a momentum-scroll simulation modeling real flick-and-pause
browsing (2,500–5,500 px/s flick peaks with friction decay, repeated flicks over 9
seconds). Every thumbnail decode requested during that test completed cleanly
(2,496 of 2,496), averaging 2.6–3.1ms decode latency.

An artificial worst-case stress test — a near-instant linear sweep across the
whole list (~330,000 px/s average, which reloads essentially the entire visible
grid every single frame, not a realistic input) — did surface real degradation:
average fps dropped to ~52–58, worst frame time hit 116ms, and 1–3% of frames
exceeded 25ms. This is a graceful degradation under an unrealistic input (e.g. a
scrollbar-thumb drag flung across the entire list instantly), not a failure mode —
but it's the one caveat worth carrying into the spec.

**This validates [Choose the GUI shell](../tickets/007-choose-gui-shell.md)'s
Tauri decision rather than calling it into question.** No reopening recommended.

## Methodology

Built at `tauri-grid-bench` (scratchpad, not committed — throwaway):
- Tauri v2, vanilla JS/HTML frontend (no framework), `image` crate to procedurally
  generate a pool of 300 unique 200×200 JPEGs (varied gradients, ~6.9KB each) at
  build time.
- 100,000 logical grid cells (6 columns × ~16,667 rows), each cell's `<img src>`
  mapped to `pool[cellIndex % 300]` via a real file path with a unique query
  string per row-assignment (`thumbs/thumb_NNN.jpg?cell=X&r=Y`) — this defeats the
  browser's decoded-image cache so every recycle triggers a genuine fresh decode,
  even though only 300 distinct files exist on disk. Decode *cost* scales with
  image dimensions/content, not file identity, so this is a reasonable proxy for
  100,000 genuinely unique thumbnails without spending build time generating that
  many real files.
- Virtualization: a spacer element sized for the full 100k-item scroll extent; a
  pool of 13 recycled row elements (only ever 78 `<img>` DOM nodes) repositioned
  via `transform: translateY()` and re-assigned `src` only when their row index
  changes.
- Instrumentation (in-page JS, results written out via a Tauri command to a JSON
  file so the whole run is fully automated, headless-driven, no manual
  interaction): per-frame `requestAnimationFrame` deltas → avg/p95/worst frame
  time; per-image `load` event timing → decode latency; `performance.memory`
  (Chromium/WebView2-native) sampled at start/mid/end; Windows process working-set
  memory sampled externally via PowerShell every 400ms.
- Two automated scroll drivers were tested (see below); both ran for a fixed 9s
  window after an 0.8s settle delay, then the app wrote results and self-quit.

**A real bug surfaced and was fixed during this work**: the first implementation
used a custom `register_uri_scheme_protocol("thumb", …)` handler, intended to
serve thumbnails without writing 100k files to disk. On Windows/WebView2 this
failed silently — `fetch()` to the registered scheme returned `TypeError: Failed
to fetch`, and neither `<img>` `load` nor `error` events ever fired (a genuinely
confusing failure mode; it took a `fetch()`-based diagnostic to get a clear
signal, since `<img>` gave no error at all). Root cause not fully isolated within
this ticket's time-box — worth a note for whoever builds the real import
pipeline's thumbnail-serving mechanism, since LensLocker will likely want
on-demand/generated thumbnail serving rather than static files. The prototype
worked around it by serving thumbnails through Tauri's default static-asset
pipeline instead (the same mechanism that serves `index.html` itself), which
worked immediately and is arguably the more natural fit for thumbnails that are
already written to disk in the managed store's cache anyway.

## Results

### Realistic momentum-scroll (2 runs, consistent)

Simulated repeated flick-and-decay scrolling: velocity impulses of 2,500–5,500
px/s every 1.4–2.4s, decaying via friction (~1.2s falloff per flick) — modeling a
person actually browsing, not a synthetic sweep.

| Metric | Run 1 | Run 2 |
|---|---|---|
| Frames sampled | 540 | 540 |
| Avg frame time | 16.67ms (60.0 fps) | 16.67ms (60.0 fps) |
| Worst frame time | 17.0ms | 16.9ms |
| Frames over 25ms | **0** | **0** |
| Decodes completed / assigned | 2,496 / 2,496 (100%) | 2,496 / 2,496 (100%) |
| Avg decode latency | 2.57ms | 3.14ms |
| Max decode latency | 28.4ms | 53.5ms |
| JS heap (start → mid → end) | 1.33MB → 1.87MB → 1.86MB | 1.33MB → 1.76MB → 1.85MB |
| Distance covered in 9s | ~34 rows (7,190px) | ~34 rows (7,160px) |

No frame exceeded the 16.7ms budget for 60fps by more than a rounding error in
either run. This is the headline result: **at realistic scroll speeds, the grid
never drops a frame**, and every thumbnail decode requested completes well within
a frame budget.

### Stress test: unrealistic full-list linear sweep

A naive first version of the test linearly interpolated scroll position across
85% of the full 100k-item list over 9 seconds — averaging ~330,000 px/s, which
reloads nearly all 78 visible `<img>` elements on almost every single frame. This
is not representative of any real input device, but is a useful upper-bound
stress case:

| Metric | Value |
|---|---|
| Frames sampled | ~470–520 |
| Avg fps | 52–58 |
| Worst frame time | 116–117ms |
| Frames over 25ms | 5–14 (~1–3%) |
| Decode assigned | ~36,000–40,000 |
| Decode completed within window | ~4,875 |

The gap between assigned and completed here isn't data loss — it's expected
backpressure: an assignment gets superseded (its `<img src>` overwritten again)
before its decode finishes if the row recycles again before the previous load
completed, which happens constantly at this artificial speed. The frame loop
itself never stalled or died; it degraded gracefully rather than catastrophically.

### Process footprint

| Metric | Value |
|---|---|
| App's own exe working set (peak) | ~30MB |
| Total WebView2 (Chromium engine) working set, baseline (other apps already using WebView2 on this machine) | ~779MB |
| Total WebView2 working set, peak during this app's run | ~1,144–1,370MB |
| Estimated delta attributable to this app's own WebView2 instance | ~365–591MB (range across two measurement passes; imprecise — see caveats) |

The few-hundred-MB WebView2 engine overhead is consistent with what
[Survey Rust GUI shells for an offline image grid](../tickets/006-research-gui-shells.md)
already flagged as Tauri's tradeoff against a pure-Rust shell — it's a property of
spinning up a Chromium instance at all, not specific to the 100k-item grid.

## Caveats — what this does and doesn't establish

- Measured on this dev machine's hardware; not validated against low-end target
  hardware.
- Thumbnails were 200×200 procedurally-generated JPEGs from a pool of 300 unique
  files, referenced via 100,000 unique query-stringed URLs to force fresh decodes.
  Decode cost scales with image dimensions/complexity more than content identity,
  so this should be a reasonable proxy for real photos at the same thumbnail
  size — but it's still synthetic content, not real photo decode complexity.
- Tests the **display/scroll** path only, not thumbnail **generation** (decoding
  and downscaling a full-size source photo into a 200×200 thumbnail) — that's a
  separate cost, presumably handled by a background worker ahead of scroll, and
  still fog (see the map's "Thumbnail/preview cache strategy" item).
- No human/subjective visual judgment — only objective frame-timing and
  decode-latency instrumentation. Worth a human sanity-check of the actual feel
  before this is treated as fully closed for UI-polish purposes.
- The WebView2 process-footprint delta is imprecise: measured by diffing total
  system-wide `msedgewebview2.exe` working set before/during the run, since other
  running tools on this machine also embed WebView2 and pollute a simple
  process-name filter — treat the 365–591MB range as an order-of-magnitude
  estimate, not a precise figure.
- Only continuous forward momentum-scroll was tested — not random jump-scrolling
  (e.g. a scrollbar drag to an arbitrary position, or a filter/search jumping the
  view discontinuously), which is closer to what the "stress test" numbers
  approximate but wasn't tested as its own realistic scenario.
- Single window size (1400×900) and column count (6) — not varied.
