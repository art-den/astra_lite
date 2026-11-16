# This is the "astra_lite" project review result, formatted as a TODO list

Don't forget to mark the corrected items on this list!

## src/image/cam_db.rs

### [ ] Warning. Camera-DB lookup silently depends on an undocumented "all keys are lowercase" invariant
Location: src/image/cam_db.rs:41-45 (keys at :10, :16, :22, :28, :34)
Only the input is normalized (`let cam_name_lc = cam_name.to_lowercase();`), then `cam_name_lc.contains(name)` compares it against the table key taken verbatim. All 8 current keys are lowercase, so behavior is correct today, but nothing states, asserts or tests the invariant: adding an entry as `names: &["ASI174MC"]` (the natural way to write a model number) makes the lookup return `None` forever, with no log line. Degradation is graceful but invisible: neutral WB `[1,1,1]` (`preview.rs:456`) and a blank sensor label (`ui_preview.rs:961`).
Resolution: keep keys lowercase and enforce it with a `#[cfg(test)]` test asserting every key equals its own lowercase (zero hot-path cost); alternatively compare case-insensitively on both sides with an ASCII-only helper (note: `to_ascii_lowercase` per key would allocate per key per call in the repaint path).

### [ ] Warning. `get_cam_info()` input contract is undocumented and the matching rule is too fragile (dead table key, likely no match on ASCOM)
Location: src/image/cam_db.rs:40-49 (dead key at :16)
Nothing documents which string is expected. Verified call sites: INDI passes FITS `INSTRUME` via `RawImageInfo.camera` (`raw.rs:140`) — usually model-bearing, works; ASCOM Classic passes `self.device_name` (`hal_ascom/mod.rs:1137`), which is the registry chooser *driver description* (`:442-447`), and the code comment there itself admits it "can differ" from the runtime `Name`; ASCOM Alpaca passes `static_name()` (`hal_ascom_alpaca/mod.rs:379`) — typically model-less. So on the ASCOM backends the table plausibly never matches. Matching also requires the token to be contiguous and unpunctuated: the key `"touptek atr2600c"` (:16) is effectively dead (`"ToupTek ATR-2600C"` normalizes to `"touptek atr-2600c"`), and a shorter input can never match a longer key.
Reproduction: any camera whose name is not the exact lowercase model token → silent `None`, no diagnostic.
Resolution: document the expected string; normalize the table keys as well as the input (lowercase + drop non-alphanumerics, match on the model token, e.g. `atr2600c`); add `log::debug!` on a miss. Note the deeper fix: if an ASCOM string carries no model token at all, normalization recovers nothing — feed a model-bearing name (runtime `Name`) into the DB.

### [ ] Warning. `src/image/cam_db.rs` has no unit tests for a pure-logic lookup with a subtle matching rule
Location: src/image/cam_db.rs:40
The file has no `#[cfg(test)] mod tests`, while the project runs hardware-free lib tests (`cargo test --lib -p astra_lite`) and other pure-logic modules (`utils/math.rs`, `hal/mod.rs`) do have them. The fragile behavior (case folding, key invariant, multi-key entries, mono names must not match colour keys) is exactly what a few assertions would pin down.
Resolution: add tests for `"ZWO ASI294MC Pro"` → IMX294, `"SVBONY SV405CC"` → IMX294, `"ZWO ASI294MM Pro"` → `None` (verified correct — `"asi294mm"` does not contain `"asi294mc"`), plus the all-keys-lowercase invariant assertion (see the item above). Do NOT assert `"ToupTek ATR-2600C"` matches — it currently returns `None`; that is the separate lookup-gap issue above.

### [ ] Nit. `CamInfo.wb` has no doc comment for channel order / data provenance
Location: src/image/cam_db.rs:5
`pub wb: [f32; 3]` is uncommented and the numbers have no stated source. Channel order is only implied by `wb[0]→r / wb[1]→g / wb[2]→b` in `preview.rs:345-347`.
Verified correction to the original report: index 1 is NOT a reference channel — `preview.rs:467-471` divides by `min_wb = min(r,g,b)`, so the reference is the smallest channel; green being 1.000 in every row is data coincidence. The `asi294mc`/`sv405cc` shared coefficients are already explained by the adjacent `sensor: "Sony IMX294"` field.
Resolution: one `///` line stating "RGB white-balance gains, applied as `value * gain` after the level stretch" + where the numbers came from. Do not document "index 1 = reference" (false) and do not restate the `preview.rs` pipeline here (it would drift).

### [ ] Nit. No stated normalization convention for the `wb` table (latent footgun for the next entry)
Location: src/image/cam_db.rs:12,18,24,30,36 + src/image/preview.rs:467-471
All 5 entries store green = 1.000 with r,b > 1, so `min_wb` is always 1.0 and the consumer's min-normalization is a genuine no-op for table values (it still matters for the manual `wb_red/green/blue` path). Verified consequence, independent of table shape: WB is applied AFTER the level stretch (`preview.rs:361-368`), so for blue = 1.607 any stretched value >= 1/1.607 (62 %) saturates to 255. The real latent problem is that nothing states the convention, so a future entry with green != 1 silently re-scales all channels.
Resolution: one-line comment "green = 1.0 is the normalization reference, values are >= 1.0 multipliers, do not pre-normalize". Do NOT refactor the field to `[r_gain, b_gain]`: it breaks symmetry with the `[f64; 3]` manual-WB path, still needs green hardcoded, and fixes none of the clipping.

### [ ] Nit. `wb` is `[f32; 3]` while every consumer uses `f64`, forcing casts
Location: src/image/cam_db.rs:5 → src/image/preview.rs:464-466
Widening is lossless and the coefficients are far within f32 precision, so this is type-consistency only. Consumers: `PreviewParams::wb: Option<[f64; 3]>` (`preview.rs:21`), `GammaTables::wb: [f64; 3]` (`preview.rs:315`), `PreviewOptions::wb_red/green/blue: f64` (`options/preview.rs:46-48`).
Resolution: change to `[f64; 3]` and drop the three `as f64` casts (safe — `preview.rs` is the only reader of `.wb`).

### [ ] Nit. `CamInfo` API shape: internal key exposed, only `Clone` derived, no `Debug`, no struct doc
Location: src/image/cam_db.rs:1-6, :48
`pub mod image` (`lib.rs:24`) → `pub mod cam_db` (`image/mod.rs:11`) → `pub struct CamInfo` with all fields `pub`; `names` is read only internally (`:44`) — consumers use only `sensor`/`wb` (`preview.rs:458-463`, `ui_preview.rs:962-963`). All fields are `Copy` yet only `Clone` is derived, so `.cloned()` (`:48`) copies ~44 bytes of static data; no `Debug`, no `///` on the struct.
Resolution (cheap, zero-risk): derive `Copy, Debug` and add a `///` line. Returning `Option<&'static CamInfo>` is valid (verified `const CAM_INFO: &[CamInfo]` inlines to `&'static [CamInfo]`) but only marginal; making `names` `pub(crate)` is optional since the struct is built solely inside the `CAM_INFO` const.

## src/image/histogram.rs

### [x] Critical. `HistogramChan::take_from_freq` overflows `usize` in `Σ value·count` on 32-bit targets, silently corrupting mean/std_dev
Location: src/image/histogram.rs:26-28 (consumed at :30 `mean`, :37 `std_dev`)
`total_sum` is accumulated in `usize` (`i * *v as usize`). `usize == u32` on `armv7-unknown-linux-gnueabihf`, which `build_cross.sh` ships in `--release`, and `[profile.release]` (`Cargo.toml:68-72`) does not enable `overflow-checks`, so the sum wraps silently on the Pi/Orange Pi.
Reproduction (verified numbers): overflow when `samples × mean > 2^32`. Full-res `from_raw_image` (no subsampling): a 12 MP frame at mean > ~358 ADU. Subsampled `from_image`: `step = (len/3_000_000) | 1` gives up to ~6e6 samples, so mean > ~716 ADU. Both are ordinary light frames → `mean`/`std_dev` become garbage; 64-bit desktop builds are unaffected, so it is invisible in development. (No crash/NaN in release: the variance sum is `Σ cnt·diff²` in f64, always >= 0.)
Resolution: accumulate in a fixed-width type — `self.freq.iter().enumerate().map(|(i, v)| i as u64 * *v as u64).sum::<u64>()` — and convert to `f64` once (max ≈ 6.5e12, far below `u64::MAX`). Keep line 26 (`total_cnt`) as is (it fits).
Done. No unit test was added (maintainer decision), so the `:27` comment is the only guard against re-narrowing the accumulator. Note: the sibling items below still carry pre-fix line numbers for this file (+1 from line 27, and a further +5 past `:29` from the `total_cnt == 0` guard).

### [x] Severe. No `total_cnt == 0` guard: NaN moments plus a `u16::MAX` sentinel that leaks into levels/stacking and can abort the UI
Location: src/image/histogram.rs:26-36, :39-47 (sentinel at :46), :50, :54
With an empty histogram: `mean = 0.0/0.0` → NaN (:29), `std_dev` → NaN (:36), `count == 0` while the channel stays `Some(..)`; `get_nth_element` falls through and returns `u16::MAX` for any `n`, so `median()`/`get_percentile()` return 65535 — a value greater than `Histogram.max` that all consumers treat as a legal ADU.
Verified consequences: `src/image/preview.rs:399-440` (`calc_levels`) produces levels of 65535 → black/blown-out preview; `src/image/image_stacker.rs:318-325` `.unwrap_or(0)` misses `Some(count = 0)` → 65535 background fill in live stacking; `src/core/mode_camera.rs:999-1078` flat-exposure adjustment becomes a no-op (it bails with "Minimum exposure reached!" before the 20-try message). Most importantly `src/ui/utils.rs:170` (`draw_histogram`, GTK main thread) does integer `... / cnt as u64`, so a mixed `Some(count == 0)` + `Some(count > 0)` state panics → `panic = "abort"` kills the app; the `max_count == 0` early return at :164 only covers the all-empty case.
Reachability (verified): not ordinary capture — needs `max_value` truncation of a whole channel (see the `Vec::resize` item), a 1-row CFA frame, or a malformed FITS header with `BITDEPTH >= 32` (`raw.rs:148-152`).
Resolution: (a) in `take_from_freq`, handle `total_cnt == 0` explicitly (`mean = std_dev = 0.0`); (b) filter `count > 0` channels at `src/ui/utils.rs:168-170` — this is what actually removes the abort; (c) optionally return `Option<u16>` from `get_nth_element`/`median`/`get_percentile`, but note it touches ~26 call sites (9 in `preview.rs`, 6 in `mode_camera.rs`, 4 in `image_stacker.rs`, 2 each in `info.rs`/`frame_processing.rs`, 1 in `ui_preview.rs`) and each needs a behaviour decision; clamping the fallback to `self.max` is the cheaper 80% fix. Also sanity-clamp `max_value` in `raw.rs:148` — done, see the Critical `raw.rs` item.
**Partial — (a) and (b) done, the item stays open.**
- Done: (a) `total_cnt == 0` early return in `take_from_freq` (`mean = std_dev = 0`, not NaN) plus `HistogramChan::is_empty()` as the single name for the predicate; (b) `draw_histogram` skips `is_empty()` channels both before the `total_max_v` divisor and in `paint_channel` (the latter also removes the `inf`/`NaN` Cairo coords from `max_count / chan.count`). This removes the abort.
- Also fixed because (a) made it live: `src/core/mode_camera.rs:1086` no longer feeds an empty channel into the camera-offset min-diff search. Before (a) the pair was inert by accident (`NaN < x` is always false); with `std_dev = 0.0` it can *win* the selection and store `camera_offset = Some(65535)`.
- Still open: the 65535 sentinel reaches `calc_levels` (black preview), the stacker background fill and the flat-exposure loop — each needs an `is_empty()` guard or a `HistogramChan::max` clamp (`mode_camera`'s flat loop needs it regardless, since `unwrap_or_default()` yields 0, a legal median); `u16::MAX` deserves a named constant.
- Order matters: land the `freq[max_value + 1..]` fold next — it makes `count == 0` nearly unreachable. The `raw.rs:148` clamp has since landed, which closes the whole header-driven wrong-`max_value` route (the old formula yielded `1 … 32767`, not only 0). The remaining way for `draw_histogram` to hand NaN coordinates to Cairo is `hist.max == 0`, an error dialog, not an abort.
- Rejected: (c) `Option<u16>` (duplicates the `count == 0` predicate, does not touch the divisor, ~26 call sites) and collapsing an empty channel into `None` (`fill_table` leaves an empty LUT that `to_grb_bytes_no_reduct_mono` indexes unguarded → OOB panic).

### [x] Severe. `Vec::resize` truncates the histogram, silently discarding every pixel above `max_value`
Location: src/image/histogram.rs:25
`self.freq.resize(max_value as usize + 1, 0);` — `freq` always arrives with 65536 bins (`TmpFreqValues::new`/`new_from_slice`, :219-227), and `Vec::resize` truncates when `new_len < len`, so all counts above `max_value` are dropped before `count`/`mean`/`std_dev`/percentiles are computed.
Verified producer path: `subtract_dark_or_bias` clamps to `u16::MAX` (`raw.rs:724`) and `apply_flat` scales **up** by `65535/flat_value` with a `u16::MAX` clamp (`raw.rs:741-744`; master flats are normalized so the 99th percentile maps to 65535, `raw.rs:797`), while `copy_into_monochrome`/`demosaic_linear` keep `Image.max_value = info.max_value` (`raw.rs:905-921`). `from_image` passes `img.max_value()` (:198), i.e. the camera's value, and feeds `preview.img_hist` (`frame_processing.rs:497`, cloned into `raw_hist` for `ReadyImage`) and live stacking (`:713`).
Verified impact (Monte-Carlo, 12-bit, sky 1200 ADU, dark+flat): ~2 % of pixels exceed `max_value` → `count` short by 2 %, reported 95th percentile 3765 → 3647 (mildly compressed highlight stretch, slightly biased mean/std); 14-bit cameras are unaffected; an extreme mono frame collapses to `count == 0` (the NaN/65535 case above). No loss of saved data — raw FITS are unaltered.
Resolution: sum `freq[max_value + 1..]` into the last bin before truncating so `count` stays the true sample count. Do NOT switch to keeping all 65536 bins: `src/ui/utils.rs:183-195` scales the x-axis by `hist.max` and would draw ~16x past the plot area, and percentiles could exceed `light_max`.

### [x] Warning. `get_percentile` returns the `u16::MAX` sentinel for `n >= 100` and silently saturates negative/NaN input
Location: src/image/histogram.rs:54
`((n * self.count as f64) as usize + 50) / 100` is `round(n·count/100)`, so `n = 100` yields exactly `count` → `get_nth_element(count)` exhausts every bin and returns `u16::MAX` (:46) instead of the largest present value; any `n > 100` does the same. `n < 0`/NaN saturate through the `as usize` cast to index 0 (no panic). Latent: the only callers pass 1.0/45.0/60.0/95.0 (`preview.rs:381-384`).
Resolution: clamp (`let n = f64::clamp(n, 0.0, 100.0);`) and cap the index with `usize::min(idx, count.saturating_sub(1))` — verified not to change results for existing callers. Add a doc line stating `n` is a percentage.
**Done differently: the accessors now return `Option<u16>` and `get_nth_element` caps the index with `usize::min(n, count.saturating_sub(1))`, so a non-empty channel always yields `Some` (the largest present value for `n >= 100`); covered by `non_empty_accessors_always_yield_a_value`.**

### [ ] Warning. `from_raw_image`/`from_image` take `&mut self`, forcing callers to hold an `RwLock` write guard across the whole rayon computation
Location: src/image/histogram.rs:72-76, :188-206 (rayon scopes at :116, :172)
The `Histogram` lives in `RwLock<Histogram>`, so the write guard is held for the full parallel computation at `src/core/frame_processing.rs:328-358`, `:495-502`, `:714-718`. The GTK main thread takes read guards on the same locks in `handler_draw_histogram` (`src/ui/ui_preview.rs:1731-1737`) and `show_histogram_stat` (`:1654-1659`), and `std::sync::RwLock` blocks readers. Verified: no deadlock (the rayon closures never touch the `RwLock`, and the lock order forms no cycle), and the widget repaint is normally triggered after the guard is dropped (`frame_processing.rs:358` → `:360`), so the effect is an occasional hitch of a few ms to tens of ms on large Pi sensors rather than a per-frame freeze.
Resolution: compute outside the lock — either `Histogram::from_raw_image(&RawImage, bool) -> Histogram` or a `fill(&mut self)` that computes into locals and swaps at the end. To preserve the existing per-frame `freq` buffer reuse, move the old `Histogram` out under a short guard, compute, then swap back (a naive fresh-return adds ~768 KiB of allocations per frame).

### [ ] Warning. `take().unwrap_or(HistogramChan::new())` looks like buffer reuse but reuses nothing
Location: src/image/histogram.rs:81, :134-136, :194 (with :24 `self.freq = freq;`)
Every field is unconditionally replaced by `take_from_freq`, so the previous `Vec` is simply dropped; the pattern only communicates an optimisation that does not happen. (`take()` itself is still needed to move out of the `Option`.)
Resolution: write `self.l.take().unwrap_or_default()` and add `impl Default for HistogramChan`. Do NOT "fix" this with `self.freq.clear(); extend_from_slice(&freq)` — `self.freq = freq` is currently an O(1) move of the 256 KiB accumulator `Vec`, so real reuse would ADD a 256 KiB memcpy per channel per frame (and :25 truncates it anyway). Unrelated but real: `TmpFreqValues::new_from_slice` (:219-223) should just be `freq_data.to_vec()`.

### [ ] Warning. Hand-rolled map/reduce (`Mutex<Vec<TmpFreqValues>>` + `tasks_cnt = 2 × threads`) instead of rayon `fold`/`reduce`, with 768 KiB of stack arrays per task
Location: src/image/histogram.rs:88-132 (CFA path, three `[u32; 65536]` at :91-93), :149-186 (mono path, one array at :153), :219-231; duplicated task-count arithmetic at :114-115 and :170-171
Verified: not a stack-overflow risk (default rayon worker stack is 2 MiB, the code is non-recursive, so 768 KiB is ~37 %), and the cost is real but secondary — roughly 15-25 MiB of extra memory traffic per frame (~few-15 ms) on a 4-core Pi, dwarfed by demosaic/calibration. `rayon` is already a dependency and the idiom is already used in this repo (`raw.rs:531` `par_chunks_exact`, `solver.rs` fold/reduce), and `cfa_row(y)` is a pure function of `y` (`raw.rs:317`), so `par_chunks().enumerate()` supplies the row phase with no blocker.
Resolution: express both paths as `par_chunks().fold(..).reduce(..)`, dropping the `Mutex` and the duplicated task math. Keep the fold accumulator on the **heap** (`TmpFreqValues`), otherwise the big stack frames stay. Do this as idiom cleanup, not urgently.

### [ ] Nit. `Histogram::clear()` leaves a stale `max`; no `Default` for `Histogram`/`HistogramChan`
Location: src/image/histogram.rs:208-213 (`clear`), :14, :68 (`new`)
`clear()` nulls `l/r/g/b` but keeps the previous frame's `max`, so a cleared histogram reports a non-zero ADU range. Verified there is no user-visible consequence today: the only caller is `LiveStacking::prepare_for_work` (`live_stacking.rs:33`), the widget early-returns on `max_count == 0` (`src/ui/utils.rs:164`) before using `hist.max`, and `info.rs:115` would panic on `.unwrap()` of a cleared channel before reaching `max`. Setting `max = 0` is safe (the `hist.max` division is gated behind `total_max_v != 0 && max_count != 0`, :174).
Resolution: `self.max = 0` in `clear()` for consistency with `new()`. `#[derive(Default)]` is optional — `clippy::new_without_default` is deliberately `#![allow]`ed crate-wide (`src/lib.rs:17`) and `cargo clippy --lib` reports none.

### [ ] Nit. Undocumented non-obvious contracts and no unit tests for the pure histogram math
Location: src/image/histogram.rs (whole file, 0 doc comments; no `mod tests`)
The contracts that are impossible to see from the signatures: `get_nth_element`/`median`/`get_percentile` return `u16::MAX` as an out-of-data sentinel (:46); `get_percentile(n)` takes a **percent** 0..=100 (:53); `count` is the sum of `freq` and may come from a subsampled layer (:26-28, :196); `freq` is indexed by raw ADU with `freq.len() == max + 1` (:25). Tests: `histogram.rs` has none, and `tests/full_frame_processing.rs` only counts histogram *events* (`:343`, `:348`), never values — `get_nth_element`/`get_percentile`/`freq` are never exercised, which is exactly why the empty-histogram and 100 %-percentile edges above are unguarded.
Resolution: a few `///` lines for the four contracts + `#[cfg(test)] mod tests` covering: empty freq, all-one-value, all values above `max_value`, `get_percentile(0/95/100)`, and a `Σ value·count > 2^32` case. If `from_raw_image`/`from_image` are renamed, prefer `set_from_*`/`update_from_freq` — the proposed `set_freq` is wrong (the method also computes `count`/`mean`/`std_dev`). (The report's `use itertools::*;` complaint is invalid: 5 files in the project use the same glob.)

## src/image/image.rs

### [x] Severe. `ImageLayer` constructors multiply `width * height` unchecked; a corrupt FITS yields an undersized buffer and out-of-bounds reads on 32-bit builds
Location: src/image/image.rs:27, :32, :59, :71 (invariant consumed by `get()`'s `get_unchecked` at :115, `row()` at :91-92, `RectIterator::init_iter` at :347-350)
All length computations are plain `width * height`. `[profile.release]` has `overflow-checks` off (only `[profile.dev.package."*"]` sets it) and `panic = "abort"`, so the wrap is silent. Verified that `src/image/simple_fits.rs:200` parses `NAXIS` as a raw `usize` with no min/max/negative validation, returns it unvalidated from `dims()` (:112), and `src/image/io.rs:233-239` passes it straight to `make_monochrome`/`make_color` → `resize` (:71); the TIF path (`io.rs:140-147`) is the same.
Reproduction (verified): a FITS header with `NAXIS1 = NAXIS2 = 65536` on an armhf build wraps `data_len` to 0 → `bytes_len` 0 (:208) so the header-walk seek is a no-op and `read_data` (:231) iterates the zero-length slice and returns `Ok` → the `Image` ends up with `width = height = 65536` and `data.len() == 0`; every later `get_unchecked`/`row` is an out-of-bounds read (UB). On 64-bit the same file attempts an 8 GiB allocation (OOM abort, DoS only).
Resolution: `width::checked_mul(height)` (use `usize::checked_mul`) in all four constructors **including `resize`** (the actual loader path), and validate `dims()` in the FITS/TIF loaders so a bad file fails with an `eyre` error before any allocation.

### [x] Severe. `DivNLIterator`/`DivNRGBIterator` form out-of-bounds raw pointers on the final block (UB); the comments claiming otherwise are untrue
Location: src/image/image.rs:494 + :499 (comment :472) and :603-605 + :611-614 (comment :601)
After yielding the last element, `next()` still performs the row-to-row jump unconditionally (`advance_rows(&mut rows, self.advance)`), because the `y += 1` is not followed by a `y < height` guard. Recomputed for a 4x4 layer with N=2 (`advance = 2*(4-2) = 4`): `[0,4]→[2,6]→[4,8]→+adv→[8,12]→[10,14]→[12,16]→+adv→[16,20]` with `data.len() == 16` — index 20 is 4 elements past one-past-the-end. Generalized: the trailing index is `(y1 + height·N + i)·src_width + x1`, in bounds only when `src_h mod N == N-1`, so it is OOB for the divisible (common binning) case too. Confirmed with Miri on the same arithmetic: "in-bounds pointer arithmetic failed … at or beyond the end of the allocation" — flagged **without** any dereference, i.e. real UB that a Miri-clean build would reject. No dereference path exists (the next `next()` returns `None` on the `self.y >= self.height` check), so this is latent, not an observable crash.
Resolution: guard the row jump with `if self.y < self.height { advance_rows(&mut rows, self.advance) }` in both iterators (the horizontal `add(N)` stays legal), and fix the false comments at :472 and :601.

### [x] Severe. `calc_noise()` panics (program abort) when the layer holds fewer than 11 pixels
Location: src/image/image.rs:187 (11-element tuple pattern), :200-201, :207
`.tuples()` destructures 11 elements, so `data.len() < 11` leaves `diffs` empty; then `max_pos = 80 * 0 / 100 == 0` and `diffs.select_nth_unstable(0)` panics (verified with `rustc`: "partition_at_index index 0 greater than length of slice 0") → abort under `panic = "abort"`. Secondary defect: with `diffs.len() == 1` (also for ~11-87 px images) `max_pos == 0`, so `&diffs[..0]` sums to 0 and :207 computes `0.0/0.0` → NaN, which propagates into `LightFrameInfo.noise` and frame-quality comparisons.
Reproduction: `ImageLayer::<u16>::new_with_size(3, 3).calc_noise()`. Reachable via `LightFrameInfo::from_image()` (`src/image/info.rs:49`), which runs for light frames including `ReadyImage` loaded from file (`src/core/frame_processing.rs:476-479` → `:572`), and `load_image_from_fits_reader` sets dimensions straight from `hdu.dims()` with no minimum-size check — so a corrupt/tiny FITS/TIF/PNG loaded as a light source aborts the app. Normal camera frames are large (this is not hit on ordinary capture).
Resolution: `if diffs.is_empty() { return 0.0; }` plus a `max_pos == 0` guard before the RMS division. Note `if self.is_empty()` is NOT sufficient — see the `is_empty()` item below.

### [x] Severe. `Image::clear()` leaves `raw_info` stale, leaking the previous source's camera name and RA/Dec
Location: src/image/image.rs:686-693 (`make_color`/`make_monochrome` at :657-684 also skip it)
`clear()` resets the four layers, `levels_locked` and `max_value` but keeps `raw_info`. The only writers are `new_empty` (:651, `None`), `RawImage::demosaic_into` (`raw.rs:900`) and `ImageStacker::copy_to_image` (`image_stacker.rs:563`); no file loader sets it. The preview image is long-lived (`Preview.image: Arc<RwLock<Image>>`, `src/core/preview.rs:23`) and the file path does exactly `image.clear()` then load (`FromFileCameraShot::get_image`, `src/image/io.rs:353-363`), so switching camera/live-stacking → "image from file" keeps the old `RawImageInfo`.
Verified consequences: `mode_goto.rs:369-376` reads `image.raw_info.ra/dec` as the plate-solve search hint for `start_goto_image` (`engine.rs:787`; a `ReadyImage` keeps `frame_type = Lights`/`is_light_frame = true`, so the gate at `:643` passes) → the solver is steered with the previous frame's coordinates; `stars.rs` `calc_angular_fwhm`/`find_stars` likewise reuse stale `focal_len`/`pixel_size`; `get_preview_rgb_data` (`preview.rs:152-159`) shows the stale sensor label and may apply the stale camera's WB.
Resolution: `self.raw_info = None;` in `clear()`. Verified safe: the only `Image::clear()` callers are `io.rs:354` and `live_stacking.rs:33`, and every writer assigns `raw_info` after the clear, so nothing expects the old value.

### [x] Warning. `get_crd_i64()` interpolates wrong values for sub-pixel negative coordinates instead of going out of bounds
Location: src/image/image.rs:220-225 (bounds test :226), same for `y`
`x_i = x / CRD_DIV` truncates toward zero while `x_p1 = x as usize % CRD_DIV as usize` yields a positive remainder, so for every `x` in `[-255, -1]` we get `x_i == 0`, which passes the `x_i >= 0 && x_i < width_1` test and takes the interior branch (:227-234) instead of the out-of-bounds branch (:235+). Verified by running the real arithmetic: x=-1 → weight of pixel 1 is 0.9961; x=-100 → 0.6094; x=-255 → 0.0039. So a coordinate of `-0.004 px` (outside the frame) returns essentially pixel 1. No UB (`width_1` keeps the reads in range) and no crash — wrong samples confined to the real-coordinate band (-1, 0), i.e. a sub-pixel left/top border.
Reproduction: `ImageLayer::new_mono(vec![0, 1000, 0, 2000, …], w, h).get_crd_i64(-100, 0)` returns ≈ `0.39·v(0) + 0.61·v(1)`. Real callers: `image_stacker.rs:270` and `:289` pass `rot_x`/`rot_y`, which go negative near the left/top edges once `sin_a ≠ 0`, and the result is accumulated as valid data (`*dst_v += v; *cnt_v += 1`). UPDATE from the `stars.rs` review: the previously listed `stars.rs:631`/`:723` (`get_f64_crd`) caller is NOT affected — the detection border guarantees `s.x, s.y >= 16` and `|x_f|, |y_f| <= result_width2 / k <= 16`, so those coordinates never go negative (they can read replicated edge values for stars sitting exactly on the detection border, which is a different, benign matter). Additionally `get_f64_crd` (:264-269) turns NaN into `0` → a valid-looking pixel (0,0).
Resolution: `let x_i = x.div_euclid(CRD_DIV); let x_p1 = x.rem_euclid(CRD_DIV) as usize;` (identical to `/` and `%` for x >= 0, so the interior and right/bottom clamping arms are unaffected). Note that negative coordinates then return the clamped edge value, not `None` — add an explicit `x_i < 0 → None` only if "outside = no data" is the intended contract, and guard NaN separately. Add unit tests for x/y in `(-CRD_DIV..0)` and at the exact right/bottom border.

### [x] Warning. `remove_gradient()`'s Bresenham branch takes one step too many per row, and draws a 1-DN vertical seam when a row has zero horizontal gradient
Location: src/image/image.rs:290-307 (`height = z_diff as usize + 1`, `dz` from `if z1 < z2 {1} else {-1}`)
Over `self.width` pixels the loop performs `z_diff + 1` rises instead of `z_diff`. Simulated with the exact loop: w=100, 500→500 ends at 499; 500→510 ends at 511 (reference 509.9); 510→500 ends at 499 (reference 500.1). Error against the f64 branch grows monotonically from x=0 and peaks at **1.4 DN** at the right end (worst 1.49 for w=120/z_diff=59). Because the only guard is `z_diff < self.width`, `z1 == z2` reaches the branch and picks `dz = -1`, producing a single step at `x = width/2` — a real 1-DN vertical seam at mid-width that is coherent across frames (it does not average out in the stack), which happens for every row when the gradient is nearly vertical. Only display paths are affected (`frame_processing.rs:485-491` preview, `:694-705` live stacking); raw FITS are saved unaltered.
Resolution: `height = z_diff` with the step count verified against a reference ramp, and a three-way `z1.cmp(&z2)` for `dz` (verified: step count becomes exactly `z_diff`, monotone, first pixel `== z1`, last `== z2`, max error 0.5 DN, and the `z1 == z2` case degenerates to no steps → seam gone). Separate pre-existing nit found while verifying: `z2 = line.get(self.width)` samples one pixel past the last pixel while the f64 branch uses `line.get(width - 1)`, so the ramp endpoint carries a ~1-pixel × slope bias.
DONE: `rise = i32::abs_diff(z1, z2)` used as the accumulator step (the `+ 1` is gone) and `dz` from a three-way `i32::cmp` (was `if z1 < z2 {1} else {-1}`), kept inline in `remove_gradient()`. Verified against `line.get(x)`: max error 1.50 → 0.50 DN (the quantization floor), monotone, first pixel `== z1`, `z1 == z2` → zero steps. No unit tests added (ramp left inline). NOTE: the seam needs BOTH defects, so fixing only one of them is not enough. The "pre-existing nit" about `z2` is WRONG, do not "fix" it: `z2 = line.get(self.width)` is exactly what the ramp needs, because the Bresenham accumulator steps `rise` times over `width` pixels, i.e. slope `rise/width` = `line.get(x)`'s slope. Measured against the true line: keeping the endpoint at `width` gives max error 0.50 DN, moving it to `width - 1` gives 1.50 DN. The residual "last pixel != z2" is correct, since `z2` is the value one pixel past the row. Also found while verifying: `calc_gradient()` subtracts the average of its sampled cells before fitting, so `remove_gradient()` leaves a constant pedestal equal to that average instead of the original background level (pre-existing, by design, unrelated to this item).

### [x] Warning. `i32::abs(z1 - z2)` can overflow, and `v -= z` can overflow again right after it
Location: src/image/image.rs:287-289 (second site :297)
`z1`/`z2` are `line.get(..).round() as i32` (saturating casts: verified `1e30 → i32::MAX`, `-1e30 → i32::MIN`, `NaN → 0`), and `z1 - z2` is a plain `i32 - i32` with no widening → debug panic / release wrap. Verified `i32::abs(i32::MIN.wrapping_sub(i32::MAX)) == 1`, and with that wrapped `z_diff = 1` the Bresenham branch runs with `z = i32::MIN`, so :297 `v -= z` overflows a second time and the row is clamped to 0 (the 65535 variant only comes from the `else` branch's `v as u16`). Reachability is low: `calc_gradient` uses `calc_fitting_plane_z_dist`, which hard-codes `c: 1.0` (`utils/math.rs:548`), and the z inputs are u16 cell means minus their average, so coefficients stay well-conditioned — this needs a pathological solve.
Resolution: `let z_diff = i32::abs_diff(z1, z2);` with `if z_diff < self.width as u64` (all downstream uses are unsigned — `z_diff as usize + 1` at :291 — and the signed `z1 < z2` comparison at :294 is untouched); a huge spread then routes to the deterministic f64 branch.
DONE: fixed together with the Bresenham item above (same two lines) — `let rise = i32::abs_diff(z1, z2); if u64::from(rise) < self.width as u64`, and the signed comparison survives as the three-way `i32::cmp` in the `dz` computation. `i32::abs_diff` returns `u32`, not `u64`. Residual, accepted: the guard bounds the endpoints' *difference*, not their *magnitude*, so `v -= z` at :340 would still overflow if both endpoints sat near `i32::MIN`/`MAX` with a tiny spread — unreachable, since `calc_fitting_plane_z_dist` pins `c: 1.0` over u16 cell means minus their average. Strictly smaller hazard than before.

### [ ] Warning. `ImageLayer::is_empty()` uses `&&`, so a degenerate zero-width/zero-height layer reports "not empty"
Location: src/image/image.rs:78-80
`width == 0 && height == 0` mis-classifies layers built legally by `resize(0, 100)` (:70-76) or `fill_from_iter(_, 0, h)` (:53-68). Verified the invariant `data.len() == width * height` holds (:32, :63, :71), so `data.is_empty()` is equivalent to `width == 0 || height == 0`. The claimed `par_chunks_exact_mut(0)` panic (:283) is NOT reachable today, because `calc_gradient` returns `None` when `usize::min(w, h) / 30 <= 16` (:785-792) and `remove_gradient` exits at :273 first.
Resolution: `self.width == 0 || self.height == 0` for `ImageLayer::is_empty()` ONLY. Do **not** change `Image::is_empty()` (:695-700) to `||` — `is_color()` requires `l.is_empty()` and `is_monochrome()` requires r/g/b empty, so `||` would make every real image report empty and break `engine.rs:789`, `image_stacker.rs:170`, `preview.rs:138`, `plate_solve/mod.rs:106`.

### [ ] Warning. `DivNRGBIterator` duplicates `DivNLIterator` (including the identical tail-pointer UB)
Location: src/image/image.rs:411-512 vs :516-628
Rect clamping is already shared via `clamp_rect` (:399-405), but the `advance` derivation (:448 vs :565), x/y bookkeeping, `size_hint` and the trailing-pointer bug (:494-499 vs :603-615) are duplicated, so any bounds fix has to be applied twice. Verified there is NO numeric divergence: mono rounds at :492-493 and RGB at :583-584 with the identical `(sum + k/2) / k`, so there is no colour shift — the "k in `next` vs in `avg`" difference is stylistic only.
Resolution: keep the two `Iterator` wrappers (their `Item` types differ: `u16` vs a tuple) over a shared `BlockWalker` core holding `[[*const u16; P]; N]`; the fields are raw pointers (Copy, no borrow of the layers), so borrow-checker risk is low. ~60-80 shared LOC, worth doing once the pointer bug above is fixed.

### [ ] Warning. `calc_background()` divides by zero on an empty layer (latent)
Location: src/image/image.rs:216
`(sum / self.data.len() as u64) as u16` is an integer division by zero → panic → abort, and there is no `is_empty()` guard (unlike `remove_gradient` at :272). Verified it is not triggerable today: the only caller is `LightFrameInfo::from_image` (`info.rs:55`), which runs `calc_noise` on the same layer at `info.rs:49` first, so any empty layer panics there before reaching this line. The report's partial-RGB trigger (`build_part_image`) is wrong: those part images never reach `LightFrameInfo::from_image`.
Resolution: add `if self.data.is_empty() { return 0; }` for symmetry/defence; fix `calc_noise` first, since that is the reachable one.

### [ ] Nit. `remove_gradient()`'s two branches round differently (up to 1 DN apart)
Location: src/image/image.rs:294-299 vs :309-313
The fast branch rounds the plane value first (`z1 = …round() as i32`) and subtracts an integer with an explicit `[0, u16::MAX]` clamp; the slow branch subtracts the fractional `line.get(x)` and relies on `v as u16` saturation. The branch is chosen per row by `z_diff < self.width`, so the same gradient is corrected up to 1 DN differently above and below the threshold.
Resolution: use one rounding rule in both branches. Do NOT add a `max_value` clamp here: at the live-stacking call site (`frame_processing.rs:703`) stacked data legitimately exceeds the camera maximum, so the `u16::MAX` clamp is correct; the histogram truncation above `max_value` is `histogram.rs`'s concern (see that entry).

### [ ] Nit. `calc_noise()` comment is wrong and its capacity is mis-sized (in the opposite direction)
Location: src/image/image.rs:185-188
The comment says "every 7th tuple of 10 pixels" while the pattern at :187 has 11 elements (10 neighbours + the centre). The capacity claim in the original report was wrong: `diffs` receives one element per sampled tuple (~N/77) against a capacity of N/10, i.e. ~7.7x over-provisioned with zero reallocations. Row-straddling windows are real but rare and harmless (≈10/width, exactly 0 when `gcd(width, 77) > 1`; the routine keeps only the lowest 80 % of `|diffs|`, so they are discarded anyway).
Resolution: fix the comment to "11 pixels (10 neighbours + centre)" and, if tidying, `Vec::with_capacity(self.data.len() / 77)`. Row-wise iteration is not worth the complexity.

### [ ] Nit. `calc_gradient()` corner-cell arms are shorter along y than along x
Location: src/image/image.rs:827-836
`add_corner_cell` mirrors every cell to all four corners, and loop 1 already emits the corner cell, so skipping `i = 0` in loop 2 is correct — but skipping `i = corner_cells_cnt - 1` drops a real cell, making the inward arm `y_inward_max` one cell shorter than `x_inward_max` (measured 682 px vs 799 px for 4656x3520). Verified there is NO top/bottom weighting (the point set is mirror-symmetric: 24 above / 24 below the midline) and no measurable bias (level bias -0.009 DN over 3000 realisations, i.e. far under 1 DN). `corner_cells_cnt <= 2` is unreachable (`cell_size > 16` forces it to >= 7).
Resolution: `for i in 1..corner_cells_cnt` (48 → 52 points, equal arms, ~4 % variance win, no duplicates). Rationale should read "balance the corner arm lengths in x/y", not "fix top-vs-bottom bias".

### [ ] Nit. Unreachable `u16::MAX` clamp in `get_crd_i64()`; the 32-bit headroom deserves a comment
Location: src/image/image.rs:259-261
Brute-forced over all weight combinations: `x_p0 + x_p1 == 256` and `y_p0 + y_p1 == 256`, so the maximum intermediate is `65535 * 256 * 256 = 4 294 901 760` and the maximum `result` is 65535 — the `result > u16::MAX` branch is dead and `result as u16` is always lossless. Worth documenting: that maximum fits `usize` on armhf (32-bit) by a margin of only **65 535** below `u32::MAX`, so any change to `CRD_DIV` or to the accumulation order silently breaks there.
Resolution: replace the dead clamp with a `debug_assert!` and add a one-line comment about the 32-bit margin. (Other parts of the original report were checked and rejected: `#![allow(dead_code)]` at line 1 is an established idiom used by 13 of 108 `src` files; `ImageLayer::set()`'s panic-on-out-of-bounds is deliberate crate convention and has a production caller at `raw.rs:1039`; the `T: Default` bounds on the iterator impls at :346/:353 are REQUIRED because `init_iter` calls `ImageLayer::row()`, whose impl block demands `Default` — only :378 could drop it.)

## src/image/image_stacker.rs

### [x] Severe. `add_median()` treats "no data in any of the 5 slots" as a valid sample: the `i32::MIN` skip guard is dead code
Location: src/image/image_stacker.rs:437-468 (dead guard :463, bug arm :455, accumulation :464-467)
`result` is always in `[-65535, +65535]` (every arm is a `u16 - u16`, a median/mean thereof, or `0`), so `if result != i32::MIN` can never be false and skips nothing. The `0 => 0` arm fires exactly when all five slots hold the "outside the frame" sentinel (`*r1..*r5 == 0`), and it then does `result = 0 + common_background; *d += result; *c += 1;`. A destination pixel with NO source data in any of the 5 frames therefore ends up with `sum = N·background`, `cnt = N` → it renders as flat sky background instead of "no data".
Reproduction (verified end-to-end): mode `AverageOfSlidingMedians` (the "remove tracks" toggle), a small non-zero `angle` or any sub-pixel `transl_*`; the corner wedges outside every source frame come out as background through `get()` (:78) into the preview (`frame_processing.rs:698`), into `save_to_tiff` (:493, :518-522) and into the live-stacking histogram (`frame_processing.rs:716`), biasing auto-levels. The `Average` path is inconsistent with this: `add_layer` (:271-274) increments sum and count only `if let Some(v)`, so the same pixel yields `cnt == 0` → black.
Resolution: make the no-data case explicit — set `result = i32::MIN` in the `0 =>` arm (the guard is checked *before* `+= background`, so the sentinel is safe) or return `Option<i32>` — so neither `*d +=` nor `*c += 1` happens, restoring parity with `Average`. Do NOT additionally weight partial coverage by slot count: each `add_median` call is one independent window estimate, so equal weight is correct for a median. Add unit tests for the all-sentinel and partially-covered pixels in both modes.

### [ ] Severe. Accumulator widths cannot hold the frame counts the UI allows: `i32` sums overflow at 32 769 saturated frames, `cnt: u16` wraps at 65 536
Location: src/image/image_stacker.rs:19 (`data: Vec<i32>`), :98 (`cnt: Vec<u16>`), accumulation at :272-273, :291, :466-467; `frames_cnt: u32` at :104
All accumulation is plain `+=`, with no checked or saturating arithmetic. A pixel at full well overflows `i32` at frame **32 769**; for typical sky pixels (1000-3000 ADU) `data` survives but **`cnt` wraps at ~65 536**, which is the binding limit. There is no cap: `LiveStackingOptions::use_cnt` defaults to false (`src/options/live_stacking.rs:26`) so no `Progress` limit is built, the periodic save (`src/core/frame_processing.rs:782-807`) writes a TIFF but never resets the accumulator, and `src/ui/ui_camera.rs:567` offers a frame-count range up to **100 000** — a user-selected 100k is guaranteed to break both counters.
Consequences (verified): release build → a negative sum divided by a valid count clamps to 0 (permanently black pixel) and a small wrapped `cnt` with a positive sum clamps to 65535 (permanently white); the corruption is irreversible because `data`/`cnt` are running totals, and it is written to disk by `save_to_tiff`. Dev build → overflow-checks are on, so `+=` panics → abort. At 1 s sub-exposures 32 769 frames is ~9 h of continuous live stacking (45 h at 5 s).
Resolution: prefer accumulator halving over widening — when `cnt` reaches a power-of-two threshold (e.g. 32768) divide `data` and `cnt` by 2 for **all** channels at once (the `cnt` buffer is shared, so halving must be global): unbounded session length, ~0.1 % precision cost, zero extra memory. Widening to `i64`/`u32` would add ~280 MB on a 20 MP colour sensor on top of the ~600 MB the sliding-median mode already needs. `saturating_add` alone is insufficient (it freezes the sum at `i32::MAX`, producing wrong-bright pixels).

### [ ] Severe. `AverageOfSlidingMedians` allocates 5 full-resolution slots per channel with no size budget → OOM abort on Pi-class targets
Location: src/image/image_stacker.rs:20, :98, :356-363 (`rotate_image` grows `dst_chan.tmp`), :417-419
Footprint is `(14·channels + 2)` bytes/pixel — 44 B/px colour (3 channels), 16 B/px mono — all 5 slots fully populated by frame 5, never downscaled or subsampled. Recomputed and confirmed: 5472×3648 colour ≈ **878 MB**, 6224×4160 colour ≈ **1139 MB** (mono 319/414 MB). Adding what else is resident during `add()` (source `preview.image` ≈120 MB + result `live_stacking.image` ≈120 MB at 20 MP), the peak is ≈**1.12 GB @20 MP / 1.45 GB @26 MP**, versus ≈519 MB total for the default `Average` mode — so this mode is the marginal cause of OOM on 1-2 GB Raspberry Pi/Orange Pi targets. There is no `try_reserve` anywhere, so a failing `Vec::resize` aborts the process (`panic = "abort"`), and `add()`/`prepare_for_work()` return `()` so they cannot refuse the job.
Reproduction: enable "remove tracks" with a 20+ MP colour camera on a 1-2 GB board; the first frame aborts.
Resolution: compute the `(14·channels + 2) · pixels` budget in `add()`/`prepare_for_work()` and either return `eyre::Result` or transparently downgrade to `Average` with a warning. The downgrade is cheap and safe: `Average` uses only `data`/`cnt` (already allocated before the mode `match`) and `add_median` only *reads* `tmp[0..4]`, so simply not routing to `add_no_tracks` is enough.

### [ ] Warning. `save_to_tiff()` encodes synchronously on the GTK main thread while holding the stacker's read guard
Location: src/image/image_stacker.rs:473-536; caller `src/ui/ui_preview.rs:1542`
The function is pure blocking IO (`File::create` → `BufWriter` → per-strip `get()` → `write_strip` → `finish`), and `get()` (:35-84) does a full-frame resample with a per-pixel integer divide for >=5 frames. The `save_image_linear` action handler (connected at `ui_preview.rs:509`) calls `stacker.read().unwrap().save_to_tiff(&file_name)?;`, so the temporary `RwLockReadGuard` is held for the entire encode and a processing-thread `add()` (which needs `write()`) blocks behind it. The encoder is created without `.with_compression()`, so the output is uncompressed: for 20 MP RGB16 expect ~1-2 s of resampling plus a ~120 MB write, i.e. **4-8 s of frozen UI on a Pi SD card** (~2-3 s on SSD).
Resolution: add `eyre::ensure!(!self.is_empty(), ...)` at the top of `save_to_tiff` (the call-site guard at `:1536` releases its lock before the file dialog, so a `prepare_for_work()` racing in that window would otherwise reach the RGB16 branch with `width == height == 0` and write a degenerate 0x0 TIFF), document the blocking contract, and if offloading is wanted follow the idiom already used in `src/ui/ui_skymap.rs:797-821` (`std::thread::spawn` + `async_channel` + `glib::spawn_future_local`) rather than a "snapshot" clone (a 120 MB copy is itself slow and doubles memory).

### [ ] Warning. Per-frame background taken from `median()` without a `count > 0` guard, so a zero-count channel yields 65535 as the background
Location: src/image/image_stacker.rs:318-320, :325 (`hist.{r,g,b,l}.as_ref().map(|chan| chan.median()).unwrap_or(0)`)
`median()` is `get_nth_element(count / 2)` and falls through to `u16::MAX` when `count == 0` (`src/image/histogram.rs:39-47`), so `Some(chan)` with `count == 0` gives `background = 65535`, not 0 — `.unwrap_or(0)` only covers `None`. The values are stored per slot at :360 and consumed at :440-444 / :449-453 with `common_background = median5(bg1..bg5)` at :421-427.
Verified damage is subtler than "everything clamps": the full-coverage branch self-cancels (the median picks the good slots and `common_background` follows them), but the partial-coverage branch (:446-462) produces ≈ -64500, which the `result < 0 → 0` clamp (:465) turns into a **0 with `*c += 1`** → dark specks / a persistent dark rim on rotated frame edges while the bad frames sit in the 5-slot window. Reachability is latent: `count == 0` requires every sampled pixel to sit above `max_value`, i.e. it is a concrete instance of the `Vec::resize` truncation bug tracked under `src/image/histogram.rs`.
Resolution: `hist.r.as_ref().filter(|c| c.count > 0).map(|c| c.median()).unwrap_or(0)` for all four channels (behaviour-preserving for real frames, and it errs bright rather than crushing to 0). Clamping to `hist.max` is NOT a good alternative — `hist.max` is the very quantity that can be 0 here.

### [ ] Warning. The stacker relies on `get_crd_i64() == None` to mean "outside the source", which is wrong for sub-pixel negative coordinates
Location: src/image/image_stacker.rs:270-274, :289-292, :393-399
All three resampling loops treat `Option` as the validity signal (add to sum/count, or store the 0 sentinel). With `x_i = x / CRD_DIV` truncating toward zero and `x_p1 = x as usize % CRD_DIV` positive (`src/image/image.rs:220-226`), every coordinate in `[-255, -1]` returns `Some` with wrong weights, so those pixels are accumulated and counted as valid data and stored as valid slots instead of the sentinel.
Quantified by replaying the exact mapping: the affected region is the fixed source band `(-1, 0)` px, so 0.05° and 0.5° rotation give the *same* count — 1495 px (0.072 %) for 1920×1080, 3486 px (0.029 %) for 4000×3000, landing on roughly half of the destination's left column and top row. Correction to the original report: `sin_a != 0` is NOT required — since `transl_x/y` are quantized to 1/256 px, pure sub-pixel translation triggers it too (`angle = 0, transl = (1.4, 2.2)` → 2996 px). Impact is a 1-px edge whose sample is displaced at most 1 px inward; invisible in the downscaled preview.
Resolution: fix once at the root in `image.rs` with `div_euclid`/`rem_euclid` **plus** the explicit `x_i < 0 → None` arm (`div_euclid` alone still returns clamped `Some` edge values, which would keep this assumption wrong). Here, only add a `debug_assert!` documenting the "None ⇒ outside the source frame" contract; do not duplicate bounds checks in both loops.

### [ ] Warning. The `AverageOfSlidingMedians` output formula is undocumented and the module has no unit tests at all
Location: src/image/image_stacker.rs:87-90 and all `pub` items; no `#[cfg(test)]` in the file
Nothing states that this mode subtracts each frame's background median (:439-445) and adds back the median of the 5 backgrounds (:421-427, :464) — i.e. `copy_to_image()`/`save_to_tiff()` do **not** return the plain mean of the input frames, which is exactly what a consumer would assume — nor that each frame feeds up to 5 consecutive medians. Verified there is no `#[cfg(test)]` module and no test/bench/example in the project references `image_stacker` (only the median helpers in `utils/math.rs` are tested), so none of the window-fill, sentinel, `cnt == 0` or strip-sizing edge cases above are covered — despite this being pure arithmetic that needs no hardware.
Resolution: a short type-level doc for `ImageStackingMode` stating each mode's exact output formula, plus unit tests for: first frame, frames 2-4 in sliding-median mode, all-sentinel pixel, partially covered pixel, `cnt == 0` pixel.

### [ ] Warning. Sliding-window size 5 is a magic number in five places; `tmp[0..4]` is indexed after only an `is_empty()` check
Location: src/image/image_stacker.rs:315 (`% 5`), :333 (`>= 5`), :356 (`while dst_chan.tmp.len() <= idx`), :411-415 (`&chan.tmp[0]`…`&chan.tmp[4]`), plus the `median5`/`median4_i32`/`tmp[0;4]` blocks
Correct today (exactly 5 slots exist once `tmp_idx >= 5`), but the `is_empty()` guard does not prove `len == 5`, so changing the window to 3 or 7 — or calling `add_median()` from elsewhere — turns :411-415 into an out-of-bounds panic.
Resolution: `const WINDOW: usize = 5;` used at all sites plus `debug_assert_eq!(chan.tmp.len(), WINDOW)` before the indexing. Do NOT "simplify" `get()`'s `else` branch into a loop over `self.tmp`: that branch reads the accumulated `data`/`cnt`, not the slots, so it is a different algorithm.

### [ ] Nit. `rotate_image()` reserves pixel value 0 as the "no data" sentinel, silently remapping real 0 pixels to 1
Location: src/image/image_stacker.rs:394-399 (consumed at :43-44, :54-56, :67-70, :438, :449-453)
`*dst_v = if let Some(mut v) = src_v { if v == 0 { v = 1; } v } else { 0 };` — validity is encoded in the pixel value, and nothing documents that 0 is reserved. Verified the bias is a single DN and mostly self-correcting: in `median5` the bumped slot is the minimum so it is not the median unless >=3 slots are ~0, and then `median + common_background < 0` is clamped to 0 (:465), erasing the +1; only the 2-4 slot `get()` average path leaks <=1 DN. Real 0 pixels are rare here anyway (calibration re-adds the dark/bias offset, `raw.rs:717-722`, keeping the background near sky level).
Resolution: document the convention at the producer and centralise the test in one helper (`fn is_valid(v: u16) -> bool { v != 0 }`) used at all six consumer sites. A per-slot validity bitmask is not worth an extra branch in the hot rayon loop for a <=1 DN error.

### [ ] Nit. `prepare_for_work()` does not reset `raw_info`; `copy_to_image()` sets `max_value`/`raw_info` even when it copies nothing
Location: src/image/image_stacker.rs:145-159, :542-563 (:561-562)
`prepare_for_work()` resets mode, the four channels, `cnt`, `tmp_idx`, `width`, `height`, `max_value`, `total_exp`, `frames_cnt` but not `raw_info` (only writer: `add()`'s `if self.is_empty()` branch at :188; only reader: `copy_to_image` at :562). And `copy_layer` early-returns for an empty channel while :561-562 run unconditionally, so a copy from a freshly reset stacker leaves the destination `Image` with `max_value == 0`. Verified unreachable through current callers (the only `copy_to_image` call, `frame_processing.rs:698`, follows a successful `add()`; in the `prepare_for_work` interleaving every consumer bails on `is_empty()` first), and `calc_levels` with `light_max == 0` does not divide by zero (`linear_interpolate`'s denominator is the constant -1) — it merely collapses the white point to ~the 95th percentile.
Resolution: add `self.raw_info = None;` to `prepare_for_work()` and an `if self.is_empty() { return; }` at the top of `copy_to_image()` (the latter also covers the `raw_info` half). Do NOT set `mode = None` in `prepare_for_work()` — `add()` panics on `None` at :204.

### [ ] Nit. `add_layer()`/`rotate_image()` duplicate the transform setup; `get()` triplicates its averaging loop
Location: src/image/image_stacker.rs:240-256 vs :365-381; :38-73
The scalar setup (`const K`, `f64_to_i64`, `crd_to_i64`, `cos_a`/`sin_a`, centres, translation) is verbatim duplicated and recomputed per channel, and the 2/3/4-slot averaging chain appears three times. The "3x waste" framing is overstated: the per-pixel `rot_x`/`rot_y` loop must run once per channel because each channel owns its source, so only the negligible scalar setup is repeated.
Resolution: extract the transform constants into a small helper built once per `add()` call for readability, and collapse the `get()` branches into one loop over the non-zero slots.

### [ ] Nit. `add_no_tracks()` is `pub` though only `add()` sizes and validates the buffers
Location: src/image/image_stacker.rs:300 (sole caller `add()` at :203)
It is the only public mutator that does not establish the frame geometry. The specific failure in the original report is wrong — calling it on a fresh stacker hits `unreachable!()` at :328 (both predicates false), i.e. a panic, not the described silent `zip` truncation.
Resolution: make it private, like `rotate_image`/`add_median`.

### [ ] Nit. `frames_cnt` is a dead private field
Location: src/image/image_stacker.rs:104, :122, :158, :207
Initialized, zeroed in `prepare_for_work()`, incremented in `add()`, and never read (no accessor; all other `frames_cnt` hits in the repo are unrelated GTK widget IDs / `options.live.frame_cnt`).
Resolution: delete it, or expose `pub fn frames_count(&self) -> u32` and use it (e.g. for the accumulator-halving guard or the UI).

### [ ] Nit. `ImageStacker::is_empty()` and `ImageStackerChan::is_empty()` use different definitions of empty
Location: src/image/image_stacker.rs:127-133 vs :31-33
The channel predicate is `data.is_empty() && tmp.is_empty()`; the public stacker one checks the four `data` vectors plus `cnt`, ignoring `tmp` — and `save_to_tiff` branches on data-only (`!self.l.data.is_empty()`, :477) while `copy_to_image` branches on the channel predicate. No live divergence because `data` is always non-empty after `add()`.
Resolution: define `ImageStacker::is_empty()` in terms of the channel predicates so there is one notion of emptiness.

### [ ] Nit. Comments understate the `is_color()`/`is_monochrome()` contract, and two same-named predicates mean different things
Location: src/image/image_stacker.rs:171-173 (and the stacker's own predicates at :135-143)
The comments say "image.r, image.g, image.b are not empty" / "image.l is not empty", but `Image::is_color()` (`image.rs:702-707`) also requires `l` to be empty and `is_monochrome()` (`:709-714`) also requires r/g/b empty — and the XOR assert at :173 holds only because of those omitted conjuncts. Meanwhile the stacker's private predicates use the opposite, weaker convention (non-emptiness only), and both conventions meet in one expression at :195-196. Verified the both-populated state is unreachable from production writers (`demosaic_into`/`make_color`/`make_monochrome`/the file loaders all enforce exclusivity; only `benches/image_div_iter.rs` builds such an image and never calls `add`).
Resolution: reword :171-172 to the real contract, note on :173 that it asserts layer exclusivity, and rename the private predicates to `has_rgb_data()`/`has_l_data()` (8 internal call sites). Do NOT weaken `Image::is_color()`/`is_monochrome()`.

### [ ] Nit. `_ => panic!("self.mode is not initialized")` swallows future enum variants; plus style cleanups
Location: src/image/image_stacker.rs:204; also :197-198, :313-314, :296 (stray blank lines), :78-81, :42-45/:53-56/:66-70
`_ =>` should be `None =>` so a third `ImageStackingMode` variant is caught by the compiler rather than routed to the panic. Additionally: `ImageStacker::new()` has no `Default`; `ImageStackingMode` derives nothing (not even `Debug`/`Copy`, so it cannot be logged in `--debug` mode); :78-81 hand-roll what `i32::clamp(value, 0, u16::MAX as i32)` expresses; the `let mut div = 0; if *s1 != 0 { div += 1; }` chain is the same code three times.
Note: the original report's proposal to convert `add()`'s `assert_eq!`s into `eyre::Result` was reviewed and **rejected** — asserts for caller invariants are the established idiom here (126 `assert!` + 152 `assert_eq!` vs 12 `debug_assert` in `src/`), `continue_work` re-pins `options.cam.frame` so geometry cannot drift, and the invariant is currently unreachable. Likewise `clear()`'s `shrink_to_fit()` is **correct as-is** (session-boundary teardown; keeping ~1 GB resident on a 1-2 GB Pi would be worse, and `ImageLayer::clear()` follows the same keep-hot/shrink-on-teardown convention).

## src/image/info.rs

### [ ] Warning. `RawImageStat::calc()` recomputes the 95th-percentile index with unchecked `usize` multiply — wrong stat on 32-bit builds
Location: src/image/info.rs:93 (`max: c.get_nth_element(95 * c.count / 100)`)
On the shipped `armv7-unknown-linux-gnueabihf` target (`Cross.toml:15`) `usize == u32`, so `95 * c.count` overflows once `count >= 45 210 183` — reachable for a mono flat from a 61 MP sensor (9576x6388, e.g. IMX455/ASI6200MM/QHY600M); colour needs >=90 MP because G is ~n/2. This path uses the full-resolution `Histogram::from_raw_image` count, not the ~3M-subsampled `from_image`. Debug builds panic (overflow checks) → abort; release wraps and, verified, returns the **24.8th** percentile instead of the 95th while still printing it as the flat's "max". (The original report's "yields the `u16::MAX` sentinel" is false: `wrapped/100 <= 95c/100 < count` always.)
Resolution: `c.get_nth_element(((c.count as u64 * 95) / 100) as usize)` — the quotient is `< count` so it fits `u32`. Do NOT "just call `HistogramChan::get_percentile(95.0)`" as originally proposed: that method (`histogram.rs:54`) is 32-bit-broken in the same way (f64→usize cast saturates to `u32::MAX`, then `+ 50` overflows), so delegate only after fixing it.

### [ ] Warning. No empty-image guard before the layer statistics: `calc_noise()`/`calc_background()` abort on a short or empty layer
Location: src/image/info.rs:42-56 (`:44` picks the layer, `:49`/`:55` call the stats)
`is_color()` (`image.rs:702-707`) requires `l.is_empty()`, so a partially filled image falls through to the empty `l`; `calc_noise()` then hits `select_nth_unstable(0)` on a 0-length Vec and `calc_background()` divides by `self.data.len()`. Verified it also panics for *any* layer under ~11 px (the `tuples().step_by(7)` loop yields nothing). Both call sites (`frame_processing.rs:571`, `:753`) pass freshly demosaiced/stacked images and there is no `is_empty` check in that file, so this is latent rather than live.
Resolution: guard at the top of `from_image` and make `calc_noise`/`calc_background` return early for short data (see the matching `src/image/image.rs` entries — `calc_noise` is the reachable one).

### [ ] Warning. `bg_percent` divides by `image.max_value()` without a zero check (and re-reads a value already bound locally)
Location: src/image/info.rs:65 (local `max_value` already bound at :43)
The claimed empty-image route is refuted (an empty layer panics in `calc_background` at :55 before line 65 is reached, and no loader or driver can produce `max_value == 0`: ASCOM falls back to `u16::MAX`, Alpaca hardcodes it, all file loaders pass `u16::MAX`). The genuine zero route is `src/image/raw.rs:148-152` — `((1 << bitdepth) - 1) as u16` with a `BITDEPTH` of 32 yields `max_value == 0` on a *non-empty* image, producing `inf`/`NaN` in the light-history table (`src/ui/ui_preview.rs:1626`, and `:1627` divides noise by `max_value` as well).
Resolution: reuse the local and guard — `bg_percent: if max_value == 0 { 0.0 } else { (100.0 * background as f64 / max_value as f64) as f32 }`; fix the root cause at `raw.rs:148` (landed, see the Critical `raw.rs` item). Do NOT use the `i32::clamp(bitdepth, 1, 16)` form suggested here originally — it was measured to be worse than the bug it replaces: `clamp` maps `bitdepth <= 0` (the legal `BITPIX = -32` fallback) to `max_value == 1`, i.e. straight into the 8-bit truncation route, and it is still defeated by `as i32` aliasing (`BITDEPTH = 2^32 + 8` → 8 → 255).
Note: no other producer route to `max_value == 0` was found (ASCOM falls back to `u16::MAX`, Alpaca hardcodes it, loaders pass `u16::MAX`), so the guard above is now purely defensive. The redundant local re-read at `:43`/`:65` is still worth fixing.

### [ ] Nit. `hist.l.as_ref().unwrap()` depends on a cross-module invariant; the value is already computed safely elsewhere
Location: src/image/info.rs:115
`Histogram::from_raw_image(img, false)` leaves `l == None` (`histogram.rs:142`), so the `unwrap` would abort for Bayer frames. Verified unreachable today: the sole caller sits in the `FrameType::Darks | Biases` arm (`frame_processing.rs:438-442`), the same `frame_type` forces `is_monochrome_img` (`:322-324`) which takes the `l`-populating branch (`histogram.rs:79-83`), and `FrameProcessing` runs a single worker thread so no writer can interpose. Note the sibling code at `:346-352` does not degrade gracefully either — it reaches `unreachable!()`.
Resolution: don't add a second defensive fallback — delete the redundant recomputation. `raw_mean`/`raw_median`/`raw_std_dev` are already computed at `:354-356` with safe l→g selection and carried in `RawFrameResult` (`:391-397`); build `RawImageStat` from those locals plus `hist.max`/`max_value`, which removes the cross-module coupling entirely.

### [ ] Nit. `FlatInfoChan.max` holds the 95th percentile, not a maximum
Location: src/image/info.rs:75 (value produced at :93)
`get_nth_element` walks the cumulative histogram, so the field is a percentile; the true maximum is carried separately as `FlatImageInfo.max_value` (:80). The only consumer is display-only (`ui_preview.rs:917`, `percent_max`).
Resolution: rename to `p95` (2 sites) or add a one-line doc.

### [ ] Nit. `LightFrameInfo::from_image()` returns placeholders that callers must patch afterwards
Location: src/image/info.rs:61-67 (`exposure: 0.0`, `raw_noise: None`, `calibr_methods: empty()`)
Callers patch them by direct field assignment (`frame_processing.rs:576-579`), while the live-stacking caller sets only `exposure` (`:754`). Verified no wrong value reaches the UI today: `raw_noise`/`calibr_methods` are read only by the light-history table (`ui_preview.rs:1627`, `:1632`), which is fed exclusively by the first caller.
Resolution: take `exposure`, `raw_noise` and `calibr_methods` as parameters so they cannot be forgotten.

### [ ] Nit. `seconds_to_total_time_str()` belongs in `src/utils`, and it silently swallows non-finite/negative input
Location: src/image/info.rs:4-27
Verified it is used only by UI code (`ui_camera.rs:1532`, `:1537`, `:1673`; `ui_darks_library.rs:1078`; `ui_preview.rs:829`) and by nothing in `src/image` or `src/core`. Ran the real body: `NaN → "0 sec."`, `-5 → "0 sec."`. The "59 min. vs 1 h. 0 min." transition quoted in the report is correct but is not a defect (seconds are dropped below 1 h too).
Resolution: move it to `src/utils` (`timer.rs` is the natural home) and clamp the input, e.g. `let secs_total = f64::max(seconds, 0.0) as u64;`.

## src/image/io.rs

### [x] Severe. TIFF strip→row mapping ignores `PlanarConfiguration`, so a planar RGB TIFF aborts the app
Location: src/image/io.rs:153-159 (`chunk_dimensions().1`, `strip_count()`, `y1 = chunk_index * chunk_size_y`), slicing at :119-121
For `PlanarConfiguration::Planar`, tiff-0.9.1 `strip_count()` returns strips **x samples** (`decoder/mod.rs:1021-1023`) while `chunk_dimensions().1` is only `rows_per_strip` (`decoder/image.rs:494-498`) and `samples_per_pixel()` reports 1 per chunk (`image.rs:461-465`). Reproduced end-to-end with a hand-made 4x3 planar RGB TIFF (`RowsPerStrip=2` → `strip_count = 6`): at chunk 2, `y1 = 4 > height = 3` and `img.r.as_slice_mut()[from..to]` panics → abort. Independently of the panic, planar chunks hold a single plane but are consumed as interleaved RGB via `.tuples()` (:123), so the pixels are garbage. Reachable from the user: `handler_action_open_image` (`src/ui/ui_preview.rs:2016-2034`) has no `FileFilter`, and ImageJ/GDAL/Photoshop export planar TIFFs. Tiled TIFFs are not affected (`strip_count()` errors out cleanly).
Resolution: iterate `0..strips_per_band` and use `chunk_data_dimensions(i)` for the y-extent (`y1 = (chunk_index % strips_per_band) * rows_per_strip`), or `eyre::bail!` when `planar_config == Planar`. Do NOT "just call `read_image()`" — it reads only the first band and would silently return R-only.

### [x] Severe. Writes are non-atomic and the final flush error is swallowed (file reported OK while truncated)
Маловероятно
Location: src/image/io.rs:79 and :186 (also the `File::create` at :54)
`tiff.finish()` does not flush the outer `BufWriter` (verified in the vendored `tiff-0.9.1/src/encoder/mod.rs` — only the internal PackBits buffer is flushed), so the IFD and residual bytes are written by `BufWriter::drop`, which ignores errors: on ENOSPC/EIO the function returns `Ok(())` while the tail/IFD never reaches the disk. With `rows_per_strip(256)` the bulk strips do write through, so the silent window is specifically the final IFD plus the residual buffer. Separately, `File::create` truncates the destination up front, so any mid-write failure destroys a pre-existing file. Unattended capture to SD cards makes ENOSPC realistic.
Resolution: keep the `BufWriter` bound and add `file.flush()?` (ideally `sync_all()?`) after `tiff.finish()?`; better, write to a temp file, `flush`+`sync_all`, then `rename()` — apply the same to the FITS path at :54 (which propagates write errors but is still non-atomic).

### [ ] Warning. Any 2-D HDU is accepted as an image, so a BINTABLE loads as a garbage CCD frame
Location: src/image/io.rs:18-22 (`headers.iter().find(|hdu| hdu.dims().len() == 2)`)
`dims()` is built purely from `NAXISn` (`simple_fits.rs:191-211`); the parser never inspects `SIMPLE`/`XTENSION`/`TFIELDS` (grep: `SIMPLE` appears only in writers, `raw.rs:178`, `astrometry.rs:280`). A binary table is `NAXIS=2, BITPIX=8`, and the app writes exactly that shape itself (`simple_fits.rs:348-352`), so such a file is silently loaded as a mono image of meaningless values and `FromFileCameraShot::new` (:331-333) classifies it as `RawCcdData`. Worse than reported: `load_image_from_fits_reader` prefers the mono HDU, so a colour FITS whose table extension comes first loads the table, and a garbage master flat picked in `src/core/raw_calibration.rs:110,147` silently corrupts calibration. No crash (`data_len >= w*h` and `read_data` reads exactly `len()`), i.e. silent data corruption.
Resolution: add an `is_image_hdu()` predicate — image iff there is no `XTENSION` (primary) or `XTENSION == "IMAGE"`, and reject when `TFIELDS` is present. Do not hard-require `SIMPLE` (third-party colour HDUs may omit it).

### [ ] Warning. Channel-first (`[3, w, h]`) RGB FITS is rejected, and the official multi-HDU RGB convention silently loads only the red plane
Location: src/image/io.rs:221 (`dims()[2] == 3`), :241 (`bytes_len() / 3`)
Only `NAXIS3 == 3` matches, so interleaved `MxNx3` files (astropy/matplotlib convention → `NAXIS1=3, NAXIS2=w, NAXIS3=h`) fall through to `bail!("No image found in FITS")`. Found while verifying: the official RGB FITS standard (Pence 2006 — three separate 2-D HDUs) matches the mono finder, so those files load **only the R plane** with no error, which is worse than a rejection. The `checked_div` half of the report is vacuous (`dims()[2] == 3` already guarantees divisibility). The app never writes 3-D FITS itself, so this is purely foreign-file interop via File→Open.
Resolution: also accept `dims()[0] == 3` and branch on which axis carries the channels when computing plane offsets; reject `pcount != 0 || gcount != 1`; optionally honour `EXTNAME == "RGB"` / `CTYPE3 == "RGB"` so the mono fallback cannot silently win.

### [x] Warning. Saved RAW FITS data is not padded to the 2880-byte block boundary
Location: src/image/io.rs:65 (`FitsWriter::write_data`)
`simple_fits.rs:300-336` writes exactly `item_len * data.len()` bytes with no trailing padding, while `write_data_f64` (:392-397) pads to 2880 — so any RAW whose `w*h*item_len % 2880 != 0` is non-compliant (e.g. 4656x3520x2 = 32 789 760 = 11385.33 blocks). The app's own reader tolerates it (it seeks by the unpadded `bytes_len` and writes single-HDU files), so the harm is external-tool interop (astropy/Siril/PixInsight expect block-aligned data). Hits every RAW save path.
Resolution: pad `write_data` with zeros to 2880, mirroring `write_data_f64`.
Fixed: `FitsWriter::write_data()` now pads the data unit to a 2880-byte block.

### [ ] Warning. FITS bit depth ignored: `max_value` hardcoded to `u16::MAX`
Location: src/image/io.rs:235, :240
`read_data` leaves `BITPIX = 8` values in 0..255 (`simple_fits.rs:236-241`) but the image is declared full-range 16-bit, and `BITPIX = -32` is scaled to full range, so behaviour is BITPIX-dependent; the TIFF path scales u8 by 256 (`io.rs:162`), making the loaders inconsistent. "8-bit loads near-black" was not confirmed: `calc_levels` derives levels from histogram percentiles, which the empty high bins do not shift. The real damage is a wrong histogram domain, a wrong `light_max` slider top and a wrong `bg_percent`. Reachable only through foreign ReadyImage FITS loading (the app writes Images as TIFF).
Resolution: derive `max_value` from `BITPIX` (8→255, 16/-32→65535) and honour `BITDEPTH` when present.

### [ ] Warning. `panic!("Internal error")` / `unreachable!()` in the public API where an `Err` belongs
Location: src/image/io.rs:214 (`save_image_to_tif_file` on an empty `Image`), :386 (`FromFileCameraShot::save_to_file`)
Both are currently unreachable — verified: `save_image_to_tif_file` has one caller behind an `is_empty()` guard (`ui_preview.rs:1525`, and the TOCTOU window across the modal dialog is closed because `preview.image` writers hold the write lock across `clear()`+populate), and `FromFileCameraShot` is only built by `open_image_from_file`, which leaves `save_raw_params: None` so `complete_img_process_params` never routes to `save_to_file` (`frame_processing.rs:844`). They are traps for the next caller under `panic = "abort"`.
Resolution: replace both with `eyre::bail!` (4 lines, no behaviour change); no trait restructuring needed.

### [ ] Nit. 8-bit TIFF scaling maps 255 → 65280 instead of 65535
Location: src/image/io.rs:162 (`|v| v as u16 * 256`)
A flat 0.389 % gain error, and the only loader that cannot reach full scale (the identity path at :165, the `* u16::MAX` paths at :168/:171 and the pixbuf path at :277-279 all do). Related: `hdu.get_i64("PEDESTAL").unwrap_or_default() as i32` (:35) — the `as i32` wrap needs a value above 2^31, but `Header::get_value_impl` (`simple_fits.rs:53-58`) parses strictly as `i64`, so a float-formatted PEDESTAL silently becomes 0.
Resolution: `v as u16 * 257` (bit replication) and `i32::try_from(...).unwrap_or_default()`.

### [ ] Nit. `PEDESTAL` is used for a non-standard quantity, and is ingested from foreign files as the camera offset
Location: src/image/io.rs:61 (written), :35 (read)
Verified `data_offset` is not removed from the samples (`simple_fits.rs:321-329` only subtracts `bzero` = 32768), so the keyword describes a pedestal that is still inside the data. Correction to the report: `PEDESTAL` is not a FITS standard keyword (absent from the HEASARC dictionary and the reserved-keyword list); it is a MaxIm DL→PixInsight/Siril convention for pedestals added to master calibration frames, which PixInsight would subtract — double-subtracting against bias/dark. The read side (:35) also blindly adopts a foreign `PEDESTAL` as `data_offset`, while `OFFSET` (`raw.rs:198-200`) already carries a different quantity.
Resolution: use a project-specific keyword of **at most 8 characters** — `ASTRAOF`/`ASTROFF` (`ASTRA_OFF` is 9 and would corrupt the 80-byte card, since `simple_fits.rs:279` formats the name with `{:8}= `); keep reading `PEDESTAL` for old files.

### [ ] Nit. `u8_to_u16` runs 3 `powf` per pixel on a user-triggered load path
Location: src/image/io.rs:276-280 (called 3x per pixel at :294-296, :304-306)
Confirmed there is no LUT and LLVM cannot constant-fold `powf(x, 2.4)`. Scale: `get_image` caps at `max_size = 6000` (:366), so up to ~108 M `powf`; measured 72 M `powf` = ~350 ms on x86 single-thread, i.e. 1-3 s on a Pi — but the only caller chain is File→Open of a JPEG/PNG (`ui_preview.rs:2034` → `engine.rs:654`), never live stacking.
Resolution: a `static` 256-entry LUT (exact: same float ops, only 256 distinct inputs). Drive-by cleanups verified: `#![allow(dead_code)]` at :1 masks the whole module and looks vestigial; `assign_img_data` returns an always-`Ok` `eyre::Result` (:115, :134); `FromFileCameraShot::new` (:326-327) duplicates the extension sniffing of `file_ext()` (:377) — extract a free `fn ext_of(&Path) -> &str`.

### [ ] Nit. Header-derived dimensions are trusted when allocating (cross-reference)
Location: src/image/io.rs:32 (`vec![0; image_hdu.data_len()]`), :141/:145 (TIFF `dimensions()`), :235/:240 (`make_monochrome`/`make_color`)
`simple_fits.rs:196-208` builds `data_len` from `NAXIS*` with unchecked `*=`, and `read_all_headers` (:152-154) only seeks past EOF — nothing compares the declared size with the real file length. Verified outcomes: on 64-bit a huge-dims/tiny-file usually resolves to a clean `UnexpectedEof` from `read_exact` (not an abort), unless the allocation itself exceeds address space/overcommit; on 32-bit a multi-GB request aborts, and a wrapped `data_len` yields a tiny buffer with huge stored dims (the UB already tracked under `src/image/image.rs`).
Resolution: fix at the IO boundary (the only place the real file length is known) — reject zero/absurd dims, use `usize::checked_mul` for `data_len`/`bytes_len`, and compare `bytes_len` against the file length before allocating. Coordinate with the `src/image/image.rs` "unchecked `width * height`" entry so the two are fixed as one change.

## src/image/mod.rs

### [ ] Low. `pub mod utils;` is over-broad — the module is internal to `src/image`
Location: src/image/mod.rs:13
`FloodFiller` (`src/image/utils.rs`) has zero references outside `src/image`; its only consumer is `src/image/stars.rs:5` via `super::utils::*`. Keeping it `pub` puts it in the library's public surface (`astra_lite::image::utils`), where it is confusable with the crate-level `astra_lite::utils`. Verified that siblings do narrow internal-only submodules (`src/ui/mod.rs:6` `mod utils;`, `src/core/mod.rs:16-20`).
Resolution: change to `mod utils;` — `stars.rs` keeps access because it is a descendant of `image`.

### [ ] Nit. Missing trailing newline at EOF
Location: src/image/mod.rs:13
The last byte is `;` (verified), so Git prints `\ No newline at end of file` on the next edit. 7 of the 8 sibling `src/*/mod.rs` files end with a newline (only `src/sky_math/mod.rs` shares the omission).
Resolution: add a trailing newline.

### [ ] Info. `pub mod image;` is module inception, masked by a crate-wide lint allow
Location: src/image/mod.rs:3 → `crate::image::image`, silenced only by `#![allow(clippy::module_inception)]` at `src/lib.rs:10`
Renaming the submodule would be the real fix but touches every `image::image::` call site. Verified there is no per-module allow today.
Resolution: if the rename is not wanted, move the `#[allow(clippy::module_inception)]` onto this declaration so the exception is recorded where it belongs instead of being blanket-applied to the crate.

## src/image/utils.rs

### [ ] Warning. `FloodFiller::fill` has no visited set — dedup is an undocumented contract on the callback, and the field name `visited` is misleading
Location: src/image/utils.rs:4 (`visited: VecDeque<(isize, isize)>`), :34, :37, :43-44
The field is used purely as a BFS frontier (`push_back`/`pop_front`) with no membership test, so a point is re-queued every time the callback returns `Hit`. Correctness depends entirely on the callback marking the point itself and returning `Miss` on re-contact; a callback that answers `Hit` twice for the same coordinate makes `fill` loop forever while the queue grows until OOM. Both real callbacks honour it today (`stars.rs:296` inserts into `processed_points`; `stars.rs:388` inserts into `star_points` and errors past the size cap), so the queue is bounded in practice.
Resolution: document the contract on `fill` ("the callback must mark the point on the first `Hit` and return `Miss` on re-contact, and must return `Miss` for out-of-image coordinates — the filler knows no image dimensions") and rename `visited` → `frontier`. An internal visited set is not needed given the callers.

### [ ] Nit. `Error` abort is delayed by up to 7 callback calls; the seed match has a wildcard that defeats exhaustiveness
Location: src/image/utils.rs:36-52 (delayed abort), :27-31 (wildcard arm)
`error_flag` is only checked after all 8 neighbours are expanded, so the callback runs up to 7 more times after signalling `Error` and keeps mutating its accumulators. Verified no corruption today: `stars.rs:404-407` discards the whole star on `!fill_ok` and every mutated accumulator is per-iteration scratch re-initialised at `:352-360`, while the only persistent state (`all_star_coords`, `:409-411`) is written after the bail. Separately, the *seed* match at :27-31 ends in `_ => {}`, so a future `FillPtSetResult` variant would be queued and flood-expanded instead of failing to compile (the neighbour path uses `== Error` / `!= Hit`, so it is inconsistent with the seed path).
Resolution: return early from inside `check_neighbour` (or `break` the `dx` loop when `error_flag` is set) so the API does not depend on callers discarding results, and replace `_ => {}` with an explicit `FillPtSetResult::Hit => {}`.

### [ ] Nit. No docs on the public API; missing derives; small style issues
Location: src/image/utils.rs:3-21 (`FillPtSetResult` derives only `PartialEq`; `FloodFiller::new()` without `Default`), :29, :31, :38
Undiscoverable from the code: 8-connectivity (`:46-49`, `-1..=1` x `-1..=1` skipping `(0,0)`), that a `Miss` seed returns `true` (success, not failure) — which is why `stars.rs:401` has to re-check `star_points.is_empty()` — and the must-mark contract above. Style: `FillPtSetResult::Error=> return false;` at :29 is missing a space (`cargo fmt` fixes it), the `;` after the statement-position match at :31 is redundant (`rustfmt` keeps it, so delete manually), and the closure params `x, y` at :38 shadow `fill`'s own `x, y` (`:23-24`) while meaning neighbour coordinates — a real readability trap since :34 uses the outer pair.
Resolution: 3-4 `///` lines on `fill`, `#[derive(Debug, Clone, Copy, PartialEq, Eq)]` on `FillPtSetResult`, `Default` for `FloodFiller`, rename the closure params to `nx, ny`.

## src/image/preview.rs

### [x] Critical. `pos()` underflows `usize` when the corner/centre rect is larger than the source image → panic (abort)
Location: src/image/preview.rs:538-544 (`1 => (img_size - rect_size) / 2`, `2 => img_size - rect_size`), called at :552-553, rect built at :547-548, slices at :578-580/:606
`pos()` subtracts a destination-derived rect size from the **source** dimension with no clamp. For `PreviewScale::CenterAndCorners`, `result_width/height` come purely from the widget allocation (`:58-62`, `max(min(pr_area_width, pr_area_height), 42)`, and `pr_area_*` are the GTK widget size — 400-1000 px on desktop, so the rect is 130-330 px), never from the source. `get_preview_rgb_data` only guards *empty* images (:137), never a minimum size, so a source smaller than the rect wraps `src_left`/`src_top` to a huge value and the `row()`/slice panics → abort (no `catch_unwind` on this path).
Reproduction: open a small image via File→Open (FITS dims are taken verbatim at `io.rs:232`; the pixbuf loader only caps the longest side) or use a tiny ROI/subframe, with the preview area large enough that the corner rect exceeds it.
Resolution: clamp the rect before building it (`rect_width = usize::min(rect_width, usize::min(src_width, width))`, same for height), use `usize::saturating_sub` inside `pos()`, and clamp the copied length to `min(src, dst)` so the `izip!` slices stay equal length.

### [x] Severe. `GammaTables::new_or_keep(None, ...)` in `to_grb_bytes` rebuilds 4x65536 LUT entries on every draw, with the image and histogram read locks held
Location: src/image/preview.rs:482 (reuse arms :323-324 unreachable from here; doc at :305-307)
`get_preview_rgb_data` always passes `None`, so the "capacity is reused / rebuilt only when levels or gamma/wb change" doc is untrue for this path: 4 x 65536 entries (including `powf`) are recomputed per call. Measured on x86 with `rustc -O`: 0.56 ms for a narrow range, 3.48 ms when every entry hits `powf` (`levels_locked` full range), i.e. ~20-30 ms on a Pi. The callers hold `preview.image.read()` + `img_hist.read()` from `ui_preview.rs:1140`/`:1146` until :1177-1178, i.e. across the whole conversion, so the frame worker's `img_hist.write()` (`frame_processing.rs:495`) contends with it. Scope: only `CenterAndCorners` (other scales use the cached part path, `ui_preview.rs:1288`) — but that mode has no cache at all, so the entire conversion also repeats on every `queue_draw` (frame result :1643, scroll :733).
Resolution: thread `Option<GammaTables>` through `get_preview_rgb_data` like `get_preview_part_rgb_bytes` does at :265, keeping the tables where the CenterAndCorners path can own them (a field on `PreviewUi`, or a small cache in `preview.rs`), and fix the doc comment.

### [x] Severe. A histogram sentinel above `light_max` silently produces reversed levels → fully white (or fully black) preview
Location: src/image/preview.rs:400-405, :411-418 (also :428 for `wb`)
`get_percentile` returns the `u16::MAX` sentinel when `count == 0`, which the `freq.resize(max_value + 1, 0)` truncation in `histogram.rs` can cause. Recomputed with the defaults (`dark_lvl 0.2`, `light_lvl 0.8`): all three percentiles are 65535, so `light = 64854.31`, `dark = 65535`; the `if (light - dark) < 2.0` guard makes it **worse** (dark 65534, light 64854 → reversed), and since `light_max` is the camera's `max_value` (4095 on a 12-bit sensor) every LUT entry saturates to 255 → all-white preview with no diagnostic. With `light_lvl = 1.0` the guard yields `light > dark` and the LUT comes out all-zero (all-black). The same sentinel also poisons `g_wb`/`wb`, driving the r/b `dark` to ~-65535.
Reachability: file-loaded images are safe (loaders set `u16::MAX`); the camera path uses the driver-reported `max_value` (ASCOM `MaxADU`, Alpaca `max_adu`, FITS `bitdepth` at `raw.rs:149`), so a driver reporting a bit depth below the real data range plus a bright frame triggers it.
Resolution: clamp every percentile to `..=light_max` and enforce `dark < light` (e.g. `if light <= dark { light = dark + 1.0 }`); cheap, and it also covers the `wb` path.
**Done via the alternative remedy: `get_nth_element`/`median`/`get_percentile` now return `Option<u16>` (no sentinel at all).** `calc_levels` treats an empty channel as "no data": l/g fall back to the neutral default levels, r/b inherit the g levels; percentiles are additionally clamped to `0..=u16::MAX` and `dark < light` is kept. The root cause (silent `max_value` truncation in `take_from_freq`) is untouched — see the `histogram.rs` `Vec::resize` item.

### [ ] Nit. `fill_table` divides by zero when `dark == light` (non-zero), producing a binary black/white LUT
Location: src/image/preview.rs:356 (guard), :361 (`linear_interpolate` with `x2 - x1 == 0`, `utils/math.rs:168-170` has no epsilon)
Verified numerically for `min == max == 2000`: the LUT contains only {0, 255} with `lut[2000] = 0` (the `0/0 = NaN` case saturating through `as u8`), i.e. a hard binary step and no diagnostic. The claimed trigger (`hist.g == None` while `hist.r`/`hist.b` are `Some`) is unreachable — the histogram fills r/g/b in lockstep and no `Image` exists with a non-empty `r` and an empty `g`; when `hist.g` is `Some`, :417 guarantees `g_range != 0`.
Resolution: 2 defensive lines — treat `max_value - min_value < 1e-9` as "no stretch" — since `calc_levels` is untrusted input.

### [ ] Nit. The `levels_locked` branch ignores its own `light_max` argument; magic `42` duplicated; no tests
Location: src/image/preview.rs:388, :60
`full = DarkLightLevels { dark: 0.0, light: (u16::MAX - 1) as f64 }` ignores the passed `light_max` (intentional per the comment at :386, but display-referred images with a smaller `max_value` would render crushed). The `min_size.max(42)` at :60 is the only thing preventing `(width - 2*SPACING)/3` from underflowing (`SPACING = 6`), and the same bare `42` is repeated in `ui_preview.rs:1214` with no shared constant. Verified there is no `mod tests` in this file despite the LUT and rect math, and `PreviewParams` (:11), `PreviewRgbData` (:121) and `get_preview_rgb_data` (:131) are undocumented while their siblings are.
Resolution: `const MIN_PREVIEW_SIZE: usize = 42;` shared with the UI, use `light_max` in the locked branch, add doc comments and unit tests for `get_rects_for_corners_and_center`/`fill_table`.

### [ ] Nit. Duplicated logic and a systematic `grb` typo
Location: src/image/preview.rs:399-440 (percentile/stretch copy-pasted for l/g/r/b), :485-510 (dispatch duplicated twice), :344 (`fill_all_tables` always builds all 4 tables though mono uses 1 and rgb uses 3), :620-752 (8 near-identical converters — no_reduct/reduct2/3/4 x rgb/mono — plus the 2 corners helpers), :475 (`to_grb_bytes` + 10 helpers, 11 `fn to_grb*` in total)
Also `result_width`/`result_height` (:483) are computed for every scale but read only in the CenterAndCorners branches (:487, :503, :513).
Resolution: one generic/macro-generated converter over `(layer(s), table(s), N)`, build only the tables that are used, mechanical `grb`→`rgb` rename, and move `result_*` into the CenterAndCorners arm.

### [ ] Nit. `calc_reduct_ratio` uses `>` for FitWindow, and the LUT cache key compares f64 with `==`
Location: src/image/preview.rs:73, :323
Two low-value robustness items, both verified as non-defects: (a) when `floor(img_dim/2)` exactly equals `pr_area_dim`, the ratio falls back to 1 and the draw thread runs `pixbuf.scale_simple` (`ui_preview.rs:1340`) over a part ~4x the pixels it needs — bounded cost, gated by an exact integer coincidence; `>=` is the intended rule. (b) `t.levels == *levels && t.gamma == gamma && t.wb == wb` would be permanently false if any value were NaN, forcing a full rebuild each draw — but NaN is not producible here (percentiles are `u16`, `linear_interpolate`'s denominator is the constant -1.0, and serde_json rejects NaN in options).
Resolution: change the three FitWindow `>` to `>=`; optionally gate the cache key on `f64::is_finite`. (A further reported risk — no-reduct converters returning a buffer shorter than the width/height they report — was reviewed and **rejected**: both callers derive width/height from the very layer they pass, `ImageLayer::row()` panics rather than under-filling, and `Pixbuf::from_bytes` wraps a known-length `GBytes`.)

## src/image/raw.rs

### [x] Critical. `demosaic_row` underflows `width - 2` and the unsafe loop walks pointers out of bounds (heap OOB read **and** write)
Location: src/image/raw.rs:939-941 (pointers :935-937), border path :1038
`izip!(0..img.info.width - 2, row_cfa.iter().cycle().tuple_windows())` — `row_cfa` is a `&'static [CfaColor; 2]`, so `cycle().tuple_windows()` is infinite and the range is the sole bound. With `width == 1` the subtraction wraps (`1usize - 2 == usize::MAX`, verified with `rustc -C overflow-checks=off`, which is what `[profile.release]` uses), so the loop runs unboundedly: `*_row[1..].as_mut_ptr()` is a one-past-end pointer of an empty slice and `row{1,2,3}.offset(1..=2)` march past the 3-element channel `Vec`s (channels are exactly `width*height`, no padding) → heap OOB reads and writes.
Reproduction (verified path, no guard anywhere on it): File→Open a colour FITS with `NAXIS1 = 1`, `NAXIS2 >= 3` and `BAYERPAT` set to one of `BGGR|GRBG|RGGB|GBRG` (case-sensitive) → `io.rs:324-334` classifies it `RawCcdData` → `frame_processing.rs:465` `demosaic_into` → `demosaic_linear` → `demosaic_row`. The only pre-steps are the histogram (tolerates 1 px) and calibration, whose missing masters merely `log::warn!`. Width==2 is safe (`0..0`); height<3 never enters.
Resolution: reject `width < 3 || height < 3` before `demosaic_linear`, preferably at the root (`load_raw_image_from_fits_reader`/`new_from_fits_header`) so every consumer is protected. Note that `saturating_sub(2)` in `demosaic_row` alone is NOT sufficient — the border pass would then hit `(sum + cnt/2) / cnt` with `cnt == 0` at :1038 (see the last entry).

Fixed in `demosaic_into` instead of at the FITS root: for `width < 3 || height < 3` it now logs a warning and falls back to `copy_into_monochrome` (a tiny mosaic frame cannot be interpolated anyway). `demosaic_linear` is private and `demosaic_into` is its only caller, so the guard covers every consumer of the demosaic path; other degenerate-size consumers (`find_hot_pixels_*`, `filter_flat`) keep their own entries. Regression tests: `image::raw::tests::test_demosaic_into_*`.

### [x] Critical. `(1 << bitdepth) - 1` overflows for a header-controlled bitdepth, yielding `max_value = 0` and silent 8-bit truncation on save
Location: src/image/raw.rs:148-152 (bitdepth from :129)
`let bitdepth = image_hdu.get_i64("BITDEPTH").unwrap_or(image_hdu.bitpix() as i64) as i32;` then `((1 << bitdepth) - 1) as u16` — the literal is `i32` (verified). Measured in release: bitdepth 32 → **0**, 33 → 1, 48 → 65535, 64 → 0; debug aborts from bitdepth **31** (`attempt to subtract with overflow`) and >=32 (shift overflow). `new_from_fits_header` runs at `io.rs:31`, i.e. before `read_data`'s `BITPIX in {8,16,-32}` validation (`io.rs:33`, `simple_fits.rs:220`), so a plain `BITPIX = 32` file aborts in debug. In release, the silent-corruption route is an accepted BITPIX 8/16 plus a `BITDEPTH >= 31` card (which ASCOM writes — `crates/ascom/src/image.rs:344`): `max_value = 0` makes `bzero_and_bitpix_for_fits_file` (:229-234) choose BITPIX 8 and `simple_fits.rs:320` writes `*v as u8`, truncating every pixel to its low byte.
Resolution: `let max_value = if (1..16).contains(&bitdepth) { (1u16 << bitdepth) - 1 } else { u16::MAX };` (preserves 8→255, 12→4095, >=16→65535). Widening is not a fix — `((1i64 << 64) - 1) as u16 == 0`. Optionally hoist the BITPIX whitelist above the info parse so debug builds error cleanly. (The landed form differs — see `Done` below; this snippet would no longer compile, since `bitdepth` is `i64` now.)
Done (`raw.rs:148-153`). `bitdepth` now stays in `i64` (the `as i32` at :129 is gone), and:
`let max_value = if (1i64 .. u16::BITS as i64).contains(&bitdepth) { (1u16 << bitdepth as u32) - 1 } else { u16::MAX };`
(`bitdepth as u32` is lossless by construction: the guard already restricts the value to 1 … 15; `u32` is required because the shift operator takes an unsigned width).
Keeping `i64` also closes the aliasing route the shift clamp alone left open: `BITDEPTH = 2^32 + 8` used to alias to 8 → `max_value = 255` → the same silent 8-bit truncation. Measured over `-128 … 2^40`: every value `<= 16` (incl. `BITDEPTH = 0` from ASCOM dumps, negatives, and the `BITPIX = -32` fallback) is bit-identical to the old code; every bogus value now yields 65535. `u16::BITS` ties the bound to the shifted type, and the lower bound is required, not cosmetic (`1u16 << 0` minus 1 is 0).
No unit test was added (maintainer decision), so the `(1i64 .. u16::BITS as i64)` range plus the comment are the only guard against re-widening.
The landed block is one line longer than the code it replaced, so `raw.rs` references below :153 in the later items of this file drift by +1.
The optional BITPIX hoist was not needed: the clamp removes the only overflow in `new_from_fits_header`, so a `BITPIX = 32` file now reaches `read_data`'s existing check and reports `BITPIX = 32 is not supported` in debug as well as release.
NOT fixed (cannot be): frames already saved by the old code are truncated to their low byte and carry no `BITDEPTH`/`MAXADU` card (`save_to_fits_header` never writes one), so they cannot be detected or recovered — worth a line in the release note so the fix is not read as data recovery.

### [x] Severe. `RawImage::new` does not check the buffer against the header, so a FITS with `PCOUNT > 0` aborts in `copy_into_monochrome`
Location: src/image/raw.rs:257-264 (`new`), :910-912 (`copy_from_slice`), sizing at `src/image/io.rs:32`
`data_len = (w*h + pcount) * gcount` (`simple_fits.rs:197-205`), so `data.len() >= w*h` always (the "short buffer → `get()` UB" and "demosaic silently drops rows" sub-claims were checked and refuted — `chunks_exact_mut` iterates the destination, and the `pcount` tail is simply ignored). But the destination is sized exactly `w*h` (`image.rs:66`), so `copy_from_slice(&self.data)` panics whenever `pcount > 0` or `gcount > 1` on a 2-D image HDU — reachable via File→Open, INDI blobs and master-file loading, all under `panic = "abort"`.
Resolution: allocate the true pixel count at `io.rs:32` (`vec![0; info.width * info.height]` — `read_data` reads sequentially from `data_pos`, so the `pcount` tail is skipped naturally) and add `debug_assert!(data.len() >= info.width * info.height)` in `new` to document the invariant.

### [x] Severe. `normalize_flat` accepts a negative 99th-percentile maximum, producing an all-zero master flat that later whites out every light
Location: src/image/raw.rs:779-798 (guard at :793, consumer at :736)
`l_max = get_99(&mut l_values) - offset as i64` and only `max == 0` is rejected, so `p99 < offset` gives a negative divisor: :797 divides by it and :798 clamps with `.max(0)` → the master flat becomes all zeros, and `apply_flat` maps `flat_value 0 → 1` (:736) so `(s - offset) * 65535` saturates every pixel of the light to 65535. That master flat is then saved and reused. The empty-channel variant of the claim is refuted (`CfaType::None` yields an empty list but only `l_max` is selected; Bayer arrays contain only R/G/B), and the mid-loop bail is cosmetic since the caller normalizes a throwaway clone (`mode_camera.rs:555-558`). Trigger: a capped/essentially-black flat combined with an overshooting `camera_offset` estimate (which `mode_camera.rs:1081-1117` itself calls a "rough calculation").
Resolution: hoist the validation above the pixel loop and test `max <= 0` **only for the channels the CFA actually uses** (guard on `self.info.cfa`) — a mono camera with `offset > 0` legitimately has `r/g/b_max = -offset`.

### [x] Severe. Flat normalization subtracts `camera_offset` instead of the pedestal the flat actually carries, systematically undercorrecting
Location: src/image/raw.rs:750, :796; caller `src/core/mode_camera.rs:557`
`mode_camera.rs:395-419` only measures the camera offset when no master dark/bias is in use, so the two cases are mutually exclusive: when a master **does** exist, `camera_offset` stays `None` → `flat_offset = 0`, yet the flat is still dark/bias-subtracted (`raw_calibration.rs:172-181` has no flat guard), so its data carries the re-added pedestal `diff` (`raw.rs:722`, `:727`) while :796 subtracts 0. The leftover pedestal ≈ the mean illumination compresses the flat's structure — worked example: a 30 % vignette yields a correction gain of ~1.19 instead of ~1.43, i.e. a weak flat in the common "bias master exists" setup. Note the originally proposed fix (use `self.data_offset`) is a no-op: the hand-written `Clone` at :250 zeroes `data_offset`, and the caller clones before normalizing.
Resolution: compute the true pedestal at the call site **before** cloning — `if raw_image.data_offset() != 0 { raw_image.data_offset() as u16 } else { self.camera_offset.unwrap_or_default() }` — and pass it; add a unit test asserting a synthetic flat with a pedestal normalizes to a near-zero floor.

### [ ] Warning. `save_to_fits_header` never writes `BITDEPTH`, so a 12/14-bit frame reloads as `max_value = 65535`
Location: src/image/raw.rs:175-227 (read side :129)
On reload the code falls back to `bitpix`, so the saved sensor range is lost and the histogram domain/stretch changes (calibration math works on raw values, so the impact is on preview/levels). Two parts of the original claim were corrected: `MAXADU` is not read anywhere, and the X-vs-YBINNING concern is moot because :193-194 always write both equal.
Resolution: write `BITDEPTH` (or `MAXADU`) when saving.

### [ ] Warning. Calibration clamps to `u16::MAX` rather than `info.max_value` — correct, but undocumented
Location: src/image/raw.rs:724, :744
Reviewed and **refuted as a defect**: the full u16 working scale is deliberate — `normalize_flat` (:797) normalizes the master flat to 65535 regardless of sensor bit depth and `apply_flat` (:741) multiplies by `65535 / flat_value`, so clamping to `max_value` would clip exactly the flat-corrected highlights; `subtract_dark_or_bias` (:722, `:727`) also re-adds a pedestal, legitimately pushing values above 4095/16383. The autolevel consequence does not apply here either, because the autolevel histogram (`frame_processing.rs:330`) runs before calibration (:370-378). Only the post-calibration result-image histogram is affected, which is tracked under `src/image/histogram.rs`. One narrow real consequence: `simple_fits.rs:322` wraps when saving as BITPIX 8 for a `max_value <= 255` sensor.
Resolution: one-line comments at :724/:744 stating that calibrated data lives on the full u16 scale (flat normalized to 65535); fix the truncation at the histogram, not here.

### [ ] Nit. Hot-pixel border threshold is unchecked i32 and is never clamped to >= 0
Location: src/image/raw.rs:383-386 (used at :457, :506)
`(3 * p100_value + max) / 4` in unchecked i32; a negative border would make `diff > border` match nearly every pixel (the whole frame replaced by neighbour averages). Verified unreachable from real dark data: `diffs` stay in [1, 393210] and the p60..p90 sequence is increasing, so the quadratic extrapolation at x=100 is positive and well below the overflow threshold.
Resolution: `let b = i32::saturating_mul(3, p100_value).saturating_add(max) / 4; border = b.max(0);`

### [ ] Nit. `DATE-OBS` is parsed with one rigid format and the parse error is swallowed
Location: src/image/raw.rs:160-163 (format at :125)
`.ok()` discards the error with no log, and the rigid `%Y-%m-%dT%H:%M:%S%.3f` rejects a trailing `Z`, 6-digit fractions and date-only values, all common in third-party FITS and imported masters → `time: None` with no diagnostic, so saved filenames/metadata quietly lose the timestamp.
Resolution: trim a trailing `Z`, try a fallback format, and `log::debug!` on failure.

### [ ] Nit. Hand-written `Clone` zeroes `data_offset` while the pixel data keeps the pedestal
Location: src/image/raw.rs:250
Metadata and data become inconsistent, with no comment. Verified nothing consumes a clone's `data_offset` today (`mode_camera.rs:555` clones a flat before `normalize_flat`, which ignores it; `raw_stacker.rs:100` stores clones; `apply_flat`/`subtract_dark_or_bias` read only the light's own value) — in fact `normalize_flat` currently depends on this implicitly.
Resolution: add a comment at :250 stating the invariant (a clone is treated as pedestal-free).

### [ ] Nit. Dead `hits` counters and non-deterministic hot-pixel output
Location: src/image/raw.rs:450, :459, :499, :508 (`hits`); :433 → :513-516 (`HashSet`)
`hits` is only ever `+=`'d, never read (and `+=` counts as a read, so no lint fires). The defect list is built from `HashSet` iteration order, so the saved bad-pixel file is not reproducible across runs (`mode_camera.rs:1155`).
Resolution: delete `hits`; sort the pixels (or use a `BTreeSet`) before building `BadPixels`.

### [ ] Nit. Fixed 7-pixel windows and zero-size arithmetic panic on degenerate frames
Location: src/image/raw.rs:405-416 (`data[0..7]`, `data[width-7..]`), :474 (`step_by(width)`), :532/:561 (`par_chunks_exact(width)`), :808
All real callers receive camera frames or master darks/flats (hundreds of pixels); only a hand-opened tiny/malformed FITS can reach a row shorter than 7 or `width == 0`.
Resolution: guard `width >= 7 && height >= 7` (and `width != 0`) at the `find_hot_pixels_*`/`filter_flat` entry points.

### [x] Nit. Demosaic border pass divides by `cnt` unguarded and underflows `height - 1`
Location: src/image/raw.rs:1038 (`(sum + cnt/2) / cnt`), :1045 (passes `height - 1` == -1 when height==0), :1049 (`1..height-1`)
`cnt == 0` requires a CFA image of 1x1 (or width/height < 2), reachable only through a degenerate opened FITS. This is the follow-on hazard of the `demosaic_row` fix above, so fix them together.
Resolution: early-return from `demosaic_linear` unless `width >= 2 && height >= 2`, and skip the border write when `cnt == 0`.

Fixed together with the entry above: the `width >= 3 && height >= 3` guard in `demosaic_into` also makes the border pass unreachable for degenerate frames (every border pixel of such a frame has a full 2x2 CFA cell in its clipped 3x3 window, so `cnt >= 1`).

### [ ] Nit. Assorted: `set()` panics from calibration code, `cfa_arr` duplicates `info.cfa`, demosaic loops duplicated
Location: src/image/raw.rs:311 (`set()` `panic!`, reachable only via a corrupt/out-of-range defect file), :661-664 (`remove_bad_pixels` silently drops `cnt == 0` pixels), :998-1016 (mt/non-mt demosaic duplication), :242/:319-333 (`cfa_arr` as a second source of truth next to `info.cfa`)
Two parts of the original report were refuted: the `debug_assert!`/release-`zip` partial-calibration concern (`:710`, `:733`) is unreachable because `check_master_frame_is_compatible` (:688-695) already bails on size mismatch, and `rect_iter` (`:1079-1081`) is never called with `x1 > x2`.
Resolution: log skipped pixels, factor the two demosaic loops into one helper, and drop `cfa_arr` in favour of `info.cfa`.

## src/image/simple_fits.rs

### [x] Critical. HDU navigation is broken for multi-HDU files: `bytes_len` is never padded to 2880 and `NAXIS = 0` yields 1 instead of 0
Location: src/image/simple_fits.rs:197-209 (sizes), :152-154 (`seek(SeekFrom::Current(size as i64))`)
`let mut data_len = 1_usize;` and the `for idx in 1..=ndim` loop never runs when `NAXIS == 0`, so `bytes_len = data_len * byte_per_value == 1` although the spec says no data follows `NAXIS = 0`; and `bytes_len` is never rounded up to the 2880-byte group. `read_all_headers` therefore lands 1 byte (or inside the padding) into the next header block, so every card is read shifted — `END` never matches, keys lose their first character, and `from_utf8()` fails on binary.
Reproduced against astropy-written fixtures: an empty-primary + IMAGE-extension file fails with `InvalidData "invalid utf-8 sequence of 1 bytes from index 79"`; empty-primary + BINTABLE yields one bogus HDU whose `read_data` returns `[88]`; a valid two-image-HDU file parses as **1 HDU** with the second silently dropped; and when the payload length is a multiple of 80 but not 2880 (100x100x16-bit) the cards re-align and `data_pos` ends up 25760 instead of 25920 → silently wrong pixels with no error. Astra's own single-HDU files mask all of this.
Resolution: `data_len = 0` when `ndim == 0 && pcount == 0`, and seek by `(data_len * byte_per_value).next_multiple_of(2880)`. Keep the **unpadded** data length available separately — `io.rs:241` divides `bytes_len()/3` to find RGB plane offsets and must not include trailing padding.
Fixed: `bytes_len()` remains unpadded, while HDU navigation uses `checked_next_multiple_of(bytes_len, 2880)`.

### [x] Critical. `Header::get_str()` panics on a lone-apostrophe card value → app aborts on a corrupt FITS
Location: src/image/simple_fits.rs:106-108 (now :119-128)
A card whose value is a single `'` (truncated or malformed file) parses to `result == "'"`, which satisfies both `starts_with("'\")` and `ends_with("'\")`, so `&result[1..result.len()-1]` is `&s[1..0]` → "byte range starts at 1 but ends at 0" (reproduced in both debug and `-C opt-level=3`) → abort under `panic = "abort"`. Reachable from the ordinary load path via `raw.rs:134/138/139/140` (`BAYERPAT`, `FRAME`, `DATE-OBS`, `INSTRUME`); the writer itself never emits a lone quote, so the trigger is a third-party or partially-written file.
Resolution: require length before slicing — `if result.len() >= 2 && result.starts_with("'") && result.ends_with("'")` (or `strip_prefix`/`strip_suffix`).
Fixed: `strip_prefix`/`strip_suffix` (commit 557686a); unterminated quotes are returned verbatim. Does not close the quote-blind card-parsing item in `read_header`.

### [x] Critical. `BITPIX = -32` conversion ignores BZERO/BSCALE and assumes float pixels are normalized 0..1 → total silent data loss
Location: src/image/simple_fits.rs:248-253 (`(value * 65535 + 0.5) as i32`)
Reproduced on a float32 array `[1000.0, 20000.5, 50000.0, -50.0, 370000.0]` → `[65535, 65535, 65535, 0, 65535]` with no error. Astra never writes BITPIX -32 (`write_data` accepts 8|16 only), so this branch only ever meets third-party float products (Siril/PixInsight/astropy/NOIRLab calibrated or stacked frames), where physical values make the assumption wrong essentially always. The 8/16 arms do apply BZERO, so the behaviour is also inconsistent within the function.
Resolution: apply BZERO/BSCALE (or derive the range from the header) — and if no sane mapping exists, return `Unsupported` rather than clipping.
Fixed: `read_data_f32` applies `physical = raw * BSCALE + BZERO` (parsed as `f64`), scans the value range over the whole HDU to choose between the 0..1-normalized (stretch by 65535) and physical-counts (direct) conventions (per-plane reads of RGB triplet files share one mapping), and returns `Unsupported` when neither maps into 0..65535 (slight negatives/saturation clip with a warning). The 8/16 arms are unchanged; the shared integer `BZERO` cast no longer runs for float HDUs.

### [x] Severe. BZERO is parsed as an integer and BSCALE is never read, so standards-compliant signed int16 FITS get a +32768 pedestal
Location: src/image/simple_fits.rs:226, :242-247 (`get_i64("BZERO")`) (now the `real_card`/`IntMapping` helpers at :159-209 and `read_data` at :362-406)
astropy emits `BZERO =  32768.0` / `3.27680000000000E+04`; `i64::from_str` fails on both and `unwrap_or(0)` silently drops the offset. Verified with the identical payload `[32767, 32768, 32769, 0, 65535]`: integer-form BZERO reads back shifted by 32768, float-form BZERO is dropped entirely. `BSCALE` does not appear anywhere in `src/`. Astra's own files use `set_i64` (`"32768"`) and round-trip, which hides the bug in self-testing.
Resolution: read BZERO/BSCALE as `f64` and apply the mapping deliberately (note that casting a float BZERO straight into `wrapping_add` would *create* a wrap-around). Error or log on unparsable numeric cards.
Fixed: BZERO/BSCALE are read as real values (`32768`, `32768.0`, `32768.` and `3.27680000000000E+04` are equivalent — verified against files written by astropy 7.2, which emits the integer form, and hand-forced float-form cards). A unit scale with an integral offset stays exact integer math; anything else is mapped in `f64`, and the result saturates instead of wrapping (the old `wrapping_add` turned 72767 into 7231). `BITPIX = 16` bits are treated as signed only for a non-identity mapping (a nonzero BZERO or a BSCALE other than 1), so `uint16` payloads without cards — or with a `BSCALE = 1.` that declares nothing — keep loading unsigned. Unparsable cards are logged and treated as absent, non-finite cards and `BSCALE = 0` are `InvalidData`, and a mapping that leaves no pixel inside 0..65535 is `Unsupported`. `BSCALE = 2` or `BZERO = 40000` now saturate with a warning instead of being silently ignored.

### [x] Severe. Unchecked size arithmetic and unbounded `NAXIS` in the header parser
Location: src/image/simple_fits.rs:191-209, :200 (`.unwrap_or(1)` for missing NAXISn), :154
`data_len *= dim`, `+= pcount`, `*= gcount` and `bytes_len = data_len * byte_per_value` are all unchecked: debug panics with multiply overflow, release wraps (measured `100000^4` → `bytes_len = 15532559262904483840`, whose `as i64` is negative → the seek returns `InvalidInput`). `ndim` is not bounded by the header block size — a single `NAXIS = 999999999` card is enough, and the loop pushes one `dims` entry per axis (measured `NAXIS = 20 000 000` → 830 ms and a 160 MB Vec; ~999M extrapolates to tens of seconds and ~8 GB) — the FITS limit is 999. Also `bytes_len`/`data_len` are consumed by callers (`io.rs:32 vec![0; data_len()]`) before the BITPIX whitelist runs, so a huge value becomes an allocation abort. No memory unsafety: `read_data` reads exactly `result.len()` and `read_exact` fails at EOF.
Resolution: reject `ndim > 999` and any missing `NAXIS1..NAXIS{ndim}` card (drop the `unwrap_or(1)`), and use `checked_mul`/`checked_add` returning `ErrorKind::InvalidData`.
Fixed: `NAXIS` is capped at 999, missing/negative axes are rejected, and size math uses checked arithmetic.

### [x] Severe. BITPIX is whitelisted only in `read_data`, after `bytes_len`/`data_pos` are computed and after callers use the sizes
Location: src/image/simple_fits.rs:194, :207, :220
BITPIX `-128` reaches `i8::MIN.abs()` → "attempt to negate with overflow" panic inside `read_header` (abort), or in release wraps into `/ 8` giving `bytes_len ≈ 1.8e19`. BITPIX 1/0/3 make `byte_per_value == 0`, so `bytes_len == 0` and the parser re-reads the pixel area as header blocks, stopping silently at EOF (`Ok` with 1 HDU). `bitpix` also flows into `RawImageInfo` as BITDEPTH (`raw.rs:129`); the `max_value` derivation is now clamped to 1..16 (see the Critical `raw.rs` item), so only the `read_header` damage above remains. Only malformed/corrupt input triggers it — no real writer emits BITPIX 0/1/3/-128.
Resolution: validate BITPIX in `read_header` and use `checked_abs`/`checked_mul`.
Fixed: `BITPIX` is parsed as `i64`, whitelisted in `read_header`, and skipped only when no data follows.

### [ ] Severe. The 8-bit write path truncates instead of clamping (256 → 0)
Location: src/image/simple_fits.rs:322 (`(*v as u8).wrapping_sub(bzero as u8)`)
Reproduced: `write_data(8, 0, [0,1,255,256,257,300,1000,65535])` → `[0,1,255,0,1,44,232,255]`. `bzero_and_bitpix_for_fits_file` (`raw.rs:229-234`) chooses BITPIX 8 from `info.max_value` (the camera's max ADU) rather than from the data, so an 8-bit camera with calibrated data (flat scaling multiplies up to 65535) writes a truncated master flat/dark, silently.
Resolution: `u16::min(*v, 255) as u8` and pick BITPIX from the data maximum; `bzero` should also be range-checked rather than silently truncated to `u8`.

### [ ] Severe. Card parsing: quote-blind `/` split, dropped CONTINUE cards, and whole-file rejection on one non-UTF-8 byte
Location: src/image/simple_fits.rs:166-185
Verified: `OBJECT  = 'NGC 123/A'` yields `"'NGC 123"` (truncated plus a stray quote) because `split_once('/')` runs before any quote-aware parsing; CONTINUE cards (no `=`) are dropped, truncating long strings; a single Latin-1 byte rejects the entire file at the UTF-8 check — that last one is the practical "file won't open" bug. The keyword-shadowing half of the claim was refuted: `HISTORY NAXIS1 = 1000` is stored under the name `"HISTORY NAXIS1"`, so `get_i64("NAXIS1")` still returns the real value.
Resolution: parse by FITS columns (1-8 keyword, 9-10 CONTINUE, 11-80 value), do the `/` split quote-aware, decode header blocks lossy (or Latin-1), skip HISTORY/COMMENT, and merge CONTINUE.

### [ ] Warning. Truncated headers are reported as success, and no SIMPLE/XTENSION validation is done
Location: src/image/simple_fits.rs:144-149
`UnexpectedEof` is treated as "no more HDUs", so a file whose header block is cut off mid-way returns `Ok` with the HDUs parsed so far — which also masks every misalignment bug above (they end in EOF and "succeed" with HDUs silently missing). Arbitrary ASCII containing `=` cards is accepted as an HDU with the default `bitpix = 8`.
Resolution: require `SIMPLE`/`XTENSION` at the first card of a block and distinguish EOF-mid-header from EOF-between-HDU groups.

### [ ] Warning. `write_data()` never pads the data segment to 2880 (this is where the `io.rs` padding entry must be fixed)
Location: src/image/simple_fits.rs:316-334 (compare `write_data_f64`, which pads at :392-397)
Only `stream.write_all(buf)` per chunk, so `save_raw_image_to_fits_file` emits non-conformant files for most sensors (verified: 6248x4176x2 % 2880 = 576; 5496x3676x2 % 2880 = 192; 4056x3040x2 % 2880 = 1920 — only sizes like 3840x2160 are accidentally fine). Astra re-reads its own single-HDU files fine, so the impact is external readers rejecting or mis-mapping the data.
Resolution: pad at the end of `write_data` (extract the `write_data_f64:392-397` logic into a shared `fn pad_to_block(written, stream)`); `io.rs:65` needs no change — consider that entry superseded by this one.

### [ ] Warning. `write_header` can emit a card that is not 80 bytes, corrupting the whole header
Location: src/image/simple_fits.rs:279-296 (:286 comment fallback, :287-288 byte-based pad/trim)
Reproduced: a long non-ASCII value yields a 79-byte card, which shifts the entire block (`ITPIX  =`, `ND`) and makes the file unreadable (needs a >= ~58-byte non-ASCII device name). Also :286 falls back to the keyword as the comment, so every card astra writes reads `SIMPLE  = T / SIMPLE` (cosmetic), and `set_str` does not double embedded apostrophes as FITS requires (astropy tolerates it). Values over 18 characters are fine; keys over 8 characters would be rejected by astropy but astra uses none.
Resolution: build cards from `chars().take(n)`, escape `''`, and assert the final card length is 80 bytes.

### [ ] Warning. `write_header_and_bintable_f64` divides by `cols.len()` and assumes 8-byte TFORMs
Location: src/image/simple_fits.rs:345-351
`cols = &[]` panics with divide-by-zero (the existing assert only guards `data`), and `NAXIS1 = 8 * cols.len()` is wrong for any non-double TFORM (verified `TFORM1 = '1J'` still emits `NAXIS1 = 8`). Latent: the only caller (`src/plate_solve/astrometry.rs:295`) hardcodes three `"1D"` columns.
Resolution: `assert!(!cols.is_empty())` and derive the row width from each column's `type_`.

### [ ] Nit. Blocking header parsing on the GTK main thread; style and typing
Location: src/image/simple_fits.rs:132-137 (via `engine.rs:624-692`)
Reported as "blocking multi-GB decode on the UI thread" — **mostly refuted**: `engine.rs` only runs `FromFileCameraShot::new` (header scan) inline and queues the rest, so `read_data` runs on the `FrameProcessing` worker; measured `FitsReader::new` = 24 ms on a 48 MB file (1.3 s for a pathological BITPIX=1). Remaining nits: `DEFAULT_BITPIX` is declared `u16` then cast `as i8` (:194), `bytes_len`/`data_len` do not say bytes-vs-elements although `io.rs:241` depends on the bytes meaning, a redundant `<'b>` on `get_str`, an `io::*` glob import, `SeekNWrite` where `Write` would do, and there are no doc comments and no `mod tests` for a hand-rolled binary format parser.
Resolution: if the millisecond-level stall matters, move `FromFileCameraShot::new` into the queued command; otherwise just the typing/naming cleanups plus a few parser unit tests (the fixtures built while verifying this file make good regression cases).

## src/image/raw_stacker.rs

### [ ] Warning. `get()` silently discards the trailing partial group, so a master is built from fewer frames than were stacked
Location: src/image/raw_stacker.rs:86-101 (buffering), :124 (fallback only when `divisor == 0`)
In `AverageOfMedians`, frames 1..4 of an incomplete group stay in `self.images` and are never folded into `self.data`; once at least one full group of 5 exists the guard at :124 no longer fires, so 7 darks produce a master from 5 with no warning. Reachable through `MasterBiasesOptions.count = self.frames_count` unrounded (`src/ui/ui_darks_library.rs:422`), `DefectPixelsOptions` Count mode whose default is **18** (`:203` → master from 15), and free-entry `raw_frames.frame_cnt` (`src/core/mode_camera.rs:155`). (The reported `ccd_temp_ok` desync is not real: `frame_is_ok = stars_is_ok() && ccd_temp_ok` at `frame_processing.rs:848-851`, and a cold frame neither `add()`s nor increments the counter.)
Resolution: in `get()`, if `!images.is_empty()`, fold the leftovers as a mean into `data` and `divisor += 1` — this also fixes the `TOTALEXP`/offset inconsistency below. Alternatively `eyre::ensure!` the requested count is a multiple of 5 when the session starts.

### [ ] Warning. `TOTALEXP` and the mean offset count frames whose pixels are absent from the master
Location: src/image/raw_stacker.rs:76-77, :104, :116
`integr_exp` and `offset_sum/offset_div` accumulate on every `add()`, while `divisor` counts only complete groups, so the saved header overstates the integration depth. Verified metadata-only: the only consumers of `integr_time` are the FITS writer/reader (`raw.rs:188-190`, `:133`); no dark scaling uses it, and PEDESTAL is 0 for live darks/biases anyway (calibration is skipped for them).
Resolution: no separate change needed — folding the leftovers as proposed above makes `divisor`, `integr_exp` and `offset_div` agree by construction.

### [ ] Warning. Master flats always get `PEDESTAL` 0 because `RawImage::Clone` zeroes `data_offset`
Location: src/image/raw_stacker.rs:76 (reads `raw.data_offset()`), `src/image/raw.rs:250`, call site `src/core/mode_camera.rs:555-561`
The Flats branch clones the frame before `add()` and `impl Clone for RawImage` hardcodes `data_offset: 0`, so `offset_sum` only ever accumulates zeros and `Average` mode can never write a non-zero PEDESTAL (`io.rs:59-62`) — even though `subtract_dark_or_bias` really has advanced it (`raw.rs:727`). Side effect: the same zero offset is what `normalize_flat` is then called with, which is the pedestal problem recorded in the `raw.rs` section.
Resolution: avoid the clone here — `RawImage::new(info.clone(), data, cfa_arr, raw.data_offset())` (or add `set_data_offset`) — and note that fixing it also changes flat normalization output, so it should land together with the `normalize_flat` pedestal fix.

### [ ] Low. `data: Vec<u32>` can wrap and `*v + divisor2` can overflow at very large frame counts; the 4/5 group size is magic
Location: src/image/raw_stacker.rs:81, :86, :156, :7-10
Accumulating `u16` samples into `u32` wraps past 65 535 samples (`Average`) or 327 680 frames (`AverageOfMedians`, since `divisor` counts groups), and release builds wrap silently; the dark-library spins allow up to 1 000 000 frames (`src/ui/ui_darks_library.rs:609-620`), so it is theoretically reachable. The literal `4`/`5` at :86/:92-94 are not tied to `median5`'s arity (`src/utils/math.rs:50`), and `RawStacker`/`RawStackerMode`/`add`/`get` carry no docs — notably that `AverageOfMedians` averages non-overlapping 5-frame medians and that `get()` takes `&mut self` without mutating.
Resolution: `const GROUP: usize = 5` used with `median5`, a two-line doc on the mode semantics, and `debug_assert!(self.divisor < 65535)` rather than widening to `u64` (which would double the accumulator).

### [ ] Info. Cheap hardening and cleanups verified as non-defects
Location: src/image/raw_stacker.rs:39, :51-68, :66/:74, :114, :131, :153-156, :97-100
Each was checked and found unreachable or cosmetic, and is listed only so the fixes are not re-derived: the accumulator length is never checked against `width * height` (but :66/:74 make master length identical to frame 0, and `find_hot_pixels_in_master_dark` uses the safe `row()`); `divisor` division looks unguarded but the `info.is_some() && divisor == 0 && images.is_empty()` state cannot occur; the mean-of-<5 `ok` flag needs ragged slices that :66 forbids (though `Vec::new()` at :131 should be `Vec::with_capacity(self.data.len())`, and the loop could be one `izip!`); `add()` validates only width/height/cfa/frame_type while `get()` stamps frame 0's exposure/gain/bin/temperature (unreachable — `cam_options` is a constructor snapshot and master filenames come from `cam_options`); master `max_value` inherited from frame 0 can contradict normalized flat content (needs an 8-bit camera); and the per-group `clear()+shrink_to_fit()` churn matters only if profiling says so.


## src/image/stars_offset.rs

### [x] High. Unmatched frames are stacked with a zero offset, silently corrupting the stack
Location: src/image/stars_offset.rs:66 (`calculate()` returns `Option`), src/core/frame_processing.rs:670, src/core/frame_processing.rs:37

`Offset` derives `Default`, and `calculate()` signals failure only as `None`. The caller
does `offset.clone().unwrap_or_default()` and then `stacker.add(...)` unconditionally,
because the live-stacking gate uses `FrameQuality::is_ok()`, which is
`stars_is_ok() && ccd_temp_ok` and deliberately excludes `offset_is_ok`.
Default stacking mode is `Average` (no outlier rejection), so a frame whose star matching
failed is permanently averaged into the stack at zero shift, producing doubled/ghosted
stars. The only hint is the red history flag in the UI (`ui_preview.rs:1629`).

Resolution: gate `stacker.add` on `offset.is_some()` the same way the guiding path already
does (`src/core/mode_camera.rs:762`). Recheck confirmed the reasoning path exactly.

### [ ] High. O(n^3) triangle generation is repeated for each of 3 attempts
Location: src/image/stars_offset.rs:80-100 (attempt loop), triangle construction

Measured on a replica harness: n=100 generation 23.7 ms + 20.4 ms, search 29.9 ms
(26.5 M window visits); full `calculate()` about 9-11 ms/frame on success and
**86-95 ms/frame when all three attempts fail**. `similar_triangles` is uncapped
(162,968 entries at n=100; worst-case constructions 472,080). On a Raspberry Pi this is
several hundred ms per frame, paid on every frame while matching keeps failing.

Resolution: generate triangles once per attempt-cap change, index candidates with a
bucket/k-d structure, and cap `similar_triangles`.

### [ ] Medium. Consensus counted in triangles, not distinct stars
Location: src/image/stars_offset.rs:65, :94, :166 (`MAX_SIMILAR_TRIANGLES_CNT = 10`)

5 matched stars yield exactly C(5,3) = 10 triangles, so a 5-star consensus passes while a
clean 4-star match returns `None`. Causes false `None` in sparse fields (which feeds the
zero-offset stacking bug above) and weak 5-star accepts.

Resolution: threshold on distinct matched stars, or require vertex-disjoint triangles.

### [ ] Medium. Non-deterministic tie-breaking in the angle histogram
Location: src/image/stars_offset.rs:101 (`angle_hist: HashMap`), :114-123 (fold keeps first strict max)

`HashMap` iteration order with `RandomState` decides ties. Reproduced different chosen
angles (-37 vs 41) across processes for identical input, i.e. the same frame can produce
different offsets run to run.

Resolution: use `BTreeMap` or a sorted `Vec` with an explicit deterministic tie-break.

### [ ] Low. No validation of the recovered solution
Location: src/image/stars_offset.rs (return path of `calculate()`)

No finite/magnitude/residual check on the returned offset. NaN was proven unreachable
(fuzzed), so the concern is the missing sanity bound rather than a crash. Note: the
reviewer's claim that NaN would degrade to "no alignment" is refuted — `cos_a = sin_a = 0`
collapses every destination onto one source pixel, i.e. a full-frame smear.

Resolution: reject implausible results, e.g. `|offset| > image_diag / 2`, and verify the
residual on a few inlier stars.

### [ ] Low. `cmp_f64` maps NaN to Equal, breaking the total-order contract
Location: src/image/stars_offset.rs:300-304

A fuzz run panicked with "user-provided comparison function does not correctly implement a
total order" in 40/200 runs. The same code path also risks a `lower > upper` slice.

Resolution: use `f64::total_cmp`, and `partition_point` for both window bounds.

### [ ] Low. Misleading names and an undocumented effective tolerance
Location: src/image/stars_offset.rs (`min_triangle_len`, `Triangle::len`, `edge_len_err`)

`min_triangle_len` and `Triangle::len` are perimeters, not lengths. `edge_len_err` sums
three *squared* differences against `max_err^2`, so the effective per-edge tolerance is
`max_err/sqrt(3)` — verified as 0.866 px for `max_err = 1.5`.

Resolution: rename to perimeter-based names; document or correct the tolerance semantics.

### [ ] Low. Dead parameter and an untried fallback strategy; no tests in-file
Location: src/image/stars_offset.rs:237 (`_triangulate`, always `false`)

Attempts 2 and 3 *tighten* `max_err` (2.5 to 1.5) rather than loosening it, so a field
solvable only at the loose threshold is never retried with more points. The module has no
`#[cfg(test)]`; it is covered only indirectly by `tests/goto_image_offset.rs` and
`tests/live_stacking.rs`.

Resolution: drop the dead parameter, consider a looser retry, add unit tests.

#### Rejected after recheck (do not "fix" these)
- Mirror-image / degenerate-triangle false positives: replicated runs return `None` for
  mirrored input and the correct offset for collinear or duplicate stars.
- `binary_search_by` losing candidates: only reproducible on an engineered lattice, where
  the result was still correct (MISSED = 0).
- Median-position mutation: impossible — `select_nth_unstable_by` does not resize; the
  reported borrow issue was an artifact of the reviewer's harness.
- `unreachable!()` at :272 is genuinely unreachable.

## src/image/stars.rs

### [ ] High. Slice-index panic (app abort) on layers narrower than 16 px, defeating the function's own "image too small" error
Location: src/image/stars.rs:117-119 (the graceful `eyre::bail!` is later, at :135-137)

`image.as_slice().chunks_exact(image.width())` panics on a zero-width layer ("chunk size
must be non-zero"), and `row[start..]` with `start = y & 0xF` panics whenever
`start > row.len()`. The indexing loop runs to completion BEFORE the bail, so the panic
pre-empts the intended graceful error. This is a slice bounds check, not arithmetic
overflow, so it aborts in release too (`panic = "abort"`).
Reproduced (compiled `-O`): width=0 panics in `chunks_exact(0)`; width=1 panics at y=2;
width=10 at y=11; width=14 at y=15 ("range start index 15 out of range for slice of
length 14"); widths 15/16/31 reach the bail; width=32 produces chunks.
Reachable from a real user path: `handler_action_open_image` (`ui_preview.rs:2016-2034`,
GTK file chooser with no FileFilter) to `engine.open_image_from_file` (`engine.rs:654`) to
`io.rs:353`/`:232-244`, which take `hdu.dims()[0..1]` with no minimum-size check;
`Histogram::from_image` (`histogram.rs:188-206`) does not stop it; `frame_processing.rs:539`
then calls `find_stars_and_get_info`, whose `Err` arm is handled gracefully but whose panic
kills the app. A 10x1000 PNG/TIF or a corrupt FITS with small `NAXIS1` triggers it.
Corrections to the original report: the panic set is width in 1..=14 AND height >= width+2
(a 14x12 image reaches the bail); width=15 also survives, so the test at :771 is not
special to 16.
Resolution: bail on both dimensions up front —
`if image.width() < MAX_STAR_DIAM || image.height() < MAX_STAR_DIAM { bail }`.
Verified this rejects nothing that can currently succeed (a layer already needs width >= 32
to reach any chunk). The suggested `usize::min(y & 0xF, row.len())` clamp is a no-op once
width >= 32, so it is harmless but unnecessary. Note it turns the wide-but-short case (see
the informational entry below) from `Ok(empty)` into `Err("too small")` — a defensible but
real behavior change. Add tests for width 0, 8 and 14 asserting `is_err()`.

### [ ] Medium. `Star.max_value` is never the peak pixel, so FWHM is systematically over-read in the normal sharpness range
Location: src/image/stars.rs:207 (source), :302-305, :458 (stored), :565/:632 (normalisation), :695/:724 (half-max)

The stored value is `s = median3(*s1,*s2,*s3)` — the median of three HORIZONTAL row pixels.
`median3` (`src/utils/math.rs:15-17`) returns the larger neighbour when the centre is the
strict max (`median3(9,10,7) = 9`, `median3(5,10,7) = 7`), so it can never return the peak.
Worse, the detection rule never compares the centre with its immediate neighbours (l/r are
`median4` over offsets +2..+5) and never checks vertical neighbours at all.
`calc_common_star_image` normalises by `r = max_value - background` so that
`max_value -> u16::MAX`, and `calc_fwhm`/`calc_ovality` take `u16::MAX/2` as half maximum,
so an under-reported peak lowers the effective threshold and inflates widths.
Measured with a faithful pipeline replication (150 stars, random sub-pixel phases),
max_value-caused FWHM error: +25-26% at true FWHM 2, +9-10% at 3, +9% at 5
(the original report's 17/8/3% understated it by 1.5-3x). The bias is
sharpness-dependent, not a constant offset. V-curve minimum location is preserved (autofocus
searching is OK), but absolute readouts (`ui_preview.rs:833+`, arcsec via
`calc_angular_fwhm`) and the absolute gate `fwhm < max_fwhm` (default 5.0 px,
`options/quality.rs:31`, `frame_processing.rs:561-562`) are skewed — a true 4.4 px frame
reads 5.0 and is rejected.
Resolution: set `max_value` from the real peak (max over `star_points` values) as a second
pass after the flood fill, keeping `median3` for detection itself (it is a useful hot-pixel
robustness filter for POSITION). Beware: `border` (:350) and centroid weights (:389) also
consume `max_v`. Migration concern: FWHM numbers drop ~10-25% for every existing user, so
`max_fwhm=5.0` silently becomes laxer and focus-history plots get a step — bump the options
version and rescale, or at minimum document it in the changelog.
Sub-claim REFUTED: the `u16::MAX` clamp at :638-640 does NOT inflate the half-max area.
Clamping values above 65535 cannot change the set {v >= 32767}; measured half-max area is
identical (96 px) with and without the clamp. Only HFD shifts (~+2%).

### [ ] Medium. The overexposure plateau heuristic false-flags unsaturated round stars at typical sharpness
Location: src/image/stars.rs:522-532 (`plateau_border = max - range/10`, `under_plateau_cnt > points_count/4`), consumed at :438, :472-482, :562-570

The test is purely RELATIVE (90% of the row max, no absolute saturation evidence) while the
fill border is ~1/3 of the peak, so an ideal Gaussian always presents a wide-enough
"plateau": the ratio is sqrt(ln(10/9)/ln 3) = 0.31 > the 25% limit.
Measured fraction of NOISE-FREE, unsaturated (peak 12000 over bg 1000) round Gaussian stars
flagged, 600 stars per point, random sub-pixel phases: FWHM 2 -> 0%, 3 -> 11.5%,
4 -> 37.3%, 5 -> 90.8%, 6 -> 15.2% (non-monotone: integer `points_count/4` opens
phase-dependent escape windows), 8 -> 100%, 10-16 -> 72-100%.
Adding shot+read noise does NOT rescue it (rates move by only a few points).
Consequences: at best focus (FWHM 2-4 px) up to ~38% of healthy stars are painted red and
counted in the user-facing "stars (overexposed)" field (`ui_preview.rs:848-850`,
`preview.rs:215-218`); at FWHM >= 8 nearly all are dropped, `calc_common_star_image` can
return `None`, so HFD/FWHM become `None` and the autofocus re-shoots up to 4x per V-curve
position then force-fits early (`mode_focusing.rs:268-305`, limits at :10-11).
Simultaneously UNDER-inclusive: a genuinely clipped FWHM-2 star is caught only ~4.5% of the
time (the `points_count < 4` guard at :524 fires first).
Corroborated in-tree: `tests/full_frame_processing.rs:59-60` deliberately uses sigma=1.0
"so they aren't flagged as overexposed by the plateau detector".
Resolution: require absolute evidence of clipping — flag only when a fraction of the scanned
pixels sit within a few ADU of the sensor full well / `image.max_value` (an unsaturated star
cannot produce a hard-clipped core), and/or raise the required fraction well above 31%.
Do NOT simply delete the filter: letting truly saturated stars into
`calc_common_star_image` biases HFD/FWHM low and shifts the autofocus V minimum.

### [ ] Low. Point budget discards every star found so far, silently
Location: src/image/stars.rs:326 (`max_all_stars_points = width*height/10`), :332-335 (`return StarItems::new()`)

Confirmed mechanically: on overflow the function returns an EMPTY `StarItems`, discarding
all collected stars, and there is no log at the discard site — the only trace is
`log::debug!` at :85 printing `0`, indistinguishable from genuinely finding no stars.
Downstream chain confirmed: empty items -> `calc_common_star_image` returns `None` (:568-570)
-> hfd/fwhm/ovality all `None` -> `frame_processing.rs:561-566` leaves `fwhm_is_ok` /
`ovality_is_ok` at their `FrameQuality::default()` value `true` (:27-32).
Downgraded from the reported High because the branch is nearly unreachable: the budget
counts unique CORE pixels (pre-dilation, non-overlapping), the per-star cap for a COUNTED
star is ~855 px, and `star_centers` is capped at 500, so the maximum accumulation is
~427,649 px, i.e. the area branch cannot fire at all when width*height >= ~4.3 MP — every
typical main sensor. Triggering a small (<=2 MP) frame needs ~10% NON-OVERLAPPING coverage
by grossly defocused stars (FWHM ~7-14 px), which is the pathological case where the
discarded FWHM would be worthless anyway.
Also note: the "starless frame reported OK" behaviour is this codebase's deliberate
fail-open ("abstain, don't reject what you cannot measure") pattern, confirmed as a design
choice, not a defect introduced here.
Resolution: `break` instead of discarding (stars are processed brightest-first, so the
partial set is the most reliable subset) but only accept it if it still clears the existing
minimum-star gates, otherwise return empty; add `log::warn!` with accumulated count vs
budget; make the budget star-count-aware or drop it on large sensors where it is provably
unreachable.

### [ ] Low. Dead safety valve: `wrong_cnt > 1000` can never fire
Location: src/image/stars.rs:327, :333, :405

`wrong_cnt` is incremented at most once per iteration of the `star_centers` loop, and
`find_possible_stars_centers` truncates to `MAX_STARS_CNT = 500` (:309-311), so
`wrong_cnt <= 500`. Confirmed independently by two verification passes.

### [ ] Low. `ovality` is an un-normalised 0.25 px-quantised difference, presented to the user as a dimensionless roundness scale
Location: src/image/stars.rs:745 (`(max_width - min_width) / self.k as f64`, k = `COMMON_STAR_MAG` = 4 at :87)

Confirmed: `min_width` is taken at `+ANGLE_CNT/2` = +16 of `ANGLE_CNT = 32` = exactly 90 deg
(:705, :741-742); the common image is magnified x4 so dividing by k converts to SOURCE
pixels; there is no normalisation by star size, so at the same aspect ratio 1.2 the metric
reads 0.25 at FWHM ~2 px and 1.25 at FWHM ~8 px, while the gate
`quality.ovality_is_ok = ovality < max_ovality` (`frame_processing.rs:564-565`) penalises
bigger stars. `widths.push(to - from + 1)` (:736) is an integer inclusive span, so the value
is ALWAYS a multiple of 0.25, while the UI spin button offers 0.1 steps with 1 digit
(`ui_camera.rs:577-579`, range 0.2-2.0, default 2.0 from `options/quality.rs:31`) — so
thresholds 0.3/0.4/0.5 are identical and 0.6/0.7 are identical. The `chb_max_oval` tooltip in
`src/ui/resources/cam_quality.ui` ("0 - ideal round / ~0.2 - little oval / >0.5 too oval")
presents a dimensionless scale, and `ui_preview.rs:845` prints `{:.1}` with no unit, which
also rounds OFF the data grid (0.25 -> "0.2", 0.75 -> "0.8", 1.25 -> "1.2").
Resolution: keep the formula and document it ("absolute difference, in source pixels,
between the major axis and the axis 90 deg from it; quantised to 0.25 px"), fix the tooltip
to say px, and set the spin increments to 0.25.
REFUTED as a fix: redefining ovality as a normalised ratio. `max_ovality` is persisted in the
options JSON (serde, `#[serde(default)]`, range tuned to the px-difference scale), so a
silent switch to a [0,1) ratio would invalidate every existing user's stored threshold.

### [ ] Low. `Star` field names actively mislead; the module's public API has zero doc comments
Location: src/image/stars.rs:13-22 (fields set at :454-464)

Confirmed with repo-wide greps: `Star.width`/`height` are `3 * max(half-extent)` (:434-435),
not a bounding box, and are read ONLY inside this file (:606, :610) — `Star` is constructed
only at :454 (the `xyls::Star` hits in `crates/starsolver` are a different type).
`max_value` is the detection-row median (see the Medium entry above); `background` is the
~33rd percentile of a 65x65 clipped window (:338-347, `MAX_STAR_DIAM = 32`);
`brightness` is a SUMMED flux above background (:393), written out as the astrometric FLUX
column by `plate_solve/astrometry.rs:292`. `grep -n '^\s*///' src/image/stars.rs` returns 0
matches: `Star`, `StarItems`, `StarsInfo`, `Stars`, `StarsFinder`, `find_stars_and_get_info`
have no doc comments, and the units of `fwhm` (px, :698) vs `fwhm_angular` (RADIANS, :761)
are discoverable only from callers (`ui_preview.rs:836`, `:1837-1840`).
Resolution: add brief `///` comments with unit + definition for each field, and rename
`width`/`height` (or make them private and pass them to `calc_common_star_image`).
Purely additive, no behaviour impact. The merely-undocumented parts are low value on their own.

### [ ] Low. Four dead guards mislead the reader — but the fifth is load-bearing and must NOT be deleted
Location: src/image/stars.rs:119-123, :351, :432, :566, :613/:616

Verified exhaustively (all widths 1..=1024 x all dithers 0..=15, 250,729 chunks; all
2,147,450,880 `max_v > bg` pairs; 3^9 and 3x4^9 core enumerations; 200k-image fuzz):
- :119-123 DEAD. `area` is always exactly 32 long (`as_chunks::<32>().0` keeps only full
  chunks), so `first_chunk::<5>()`, `last_chunk::<5>()` and `&area[15..=17]` never fail.
  Cleanest fix: destructure the `&[u16; 32]` with an irrefutable pattern.
- :351 `if border == 0` IS LIVE — the reviewer was WRONG. Exhaustive proof: `border == 0`
  for exactly one input, `bg = 0, max_v = 1`, which passes the :349 gate whenever
  `threshold == 1` (the clamp floor at :149, hit on low-noise frames). Deleting it makes the
  fill condition `v >= border` match EVERY pixel (including out-of-image, `unwrap_or(0)`),
  so stars start failing the bbox check, `wrong_cnt` climbs and `get_stars` returns empty on
  black-sky offset-0 frames. KEEP and add a one-line comment.
- :432 `brightness > 0` DEAD, though the reviewer's reasoning was wrong (dilated ring pixels
  are NOT summed — the sum completes in the fill callback at :393 before the dilation at
  :414-430; and `border == bg` is possible so terms can be zero). The invariant still holds:
  a successful non-empty fill implies `brightness >= 1`. Convert to `debug_assert!`.
- :566 `.filter(|(_, r)| *r > 0)` DEAD as a re-test (`Star` is built only at :454, past the
  :349 gate, never mutated). LATENT HAZARD FOUND while verifying: :565 computes
  `s.max_value - s.background` as a u16 subtraction BEFORE the filter and before the i32
  cast, so a future violation panics in debug and, in release (`overflow-checks = false`),
  wraps to a large positive value that PASSES the filter; its only real protection is
  against the `/ *r` division-by-zero at :632. Fix by filtering
  `s.max_value > s.background` before the subtraction (or casting to i32 first).
- :613/:616 always taken (`min(avg, 32) * 4` is always even) but the branch is what keeps the
  result odd for ANY k; keep with a one-line comment.

### [ ] Low. Timing instrumentation measures nothing
Location: src/image/stars.rs:77-79

`find_possible_stars_centers` runs on :77; `TimeLogger::start()` is created on :78 and logged
on :79, so the BENCH line measures the gap between two adjacent statements. `TimeLogger`
(`src/utils/log_utils.rs:9-17`) has no `Drop` impl, so the reading is guaranteed ~0 rather
than a Drop-ordering artifact. Every other stage is timed correctly (:67-69, :72-74, :82-84,
:88-90, :65/:107). Since this stage is a flood-fill clustering pass over up to 5000 extrema,
the bogus reading actively misleads performance analysis on a Pi. Fix: move
`TimeLogger::start()` above :77.

### [ ] Low. The module's only unit test documents a panic that cannot happen
Location: src/image/stars.rs:772-778

The comment claims "no 32-pixel chunks, so `diffs` stays empty and `median` panics on the
empty slice", but `median` is never reached: :135-137 `eyre::bail!` first — which is exactly
why `assert!(bad_res.is_err())` passes. Independently confirmed by the size-validation
verification pass. This is the only test in a 779-line file of pure numeric logic;
`calc_threshold`, `check_is_star_ok`, `calc_ovality`, `calc_angular_fwhm` and `get_stars` on
a synthetic star field have none (which is also what makes refactoring this module risky).

### [ ] Low. Dead `mt == false` path
Location: src/image/stars.rs:159, :224-227 (parameter threaded from :58, :73)

Complete call-site list (repo-wide, including `tests/`, `benches/`, `examples/`,
`cfg(windows)`): `frame_processing.rs:544` and `:748` pass literal `true`;
`benches/stars_find_extremums.rs:61` uses `let mt = true;`. No config option, env var, or
single-core fallback ever computes it, and even a 1-thread rayon pool still takes the
parallel branch, so the sequential branch never executes in any build or test. Fix: drop the
parameter (update the bench) or add a test asserting the mt=true and mt=false extremum SETS
are equal — `find_extremums` is `pub` in a lib crate, so the test is the cheap correct move.

### [ ] Low. `calc_ovality` records width 0 for an unclosed bright run (latent only)
Location: src/image/stars.rs:726-739

The code logic is real — `last_over_index` is assigned only on an over->under transition, so
a run still over at the last sampled `j` yields `widths.push(0)` (replicated in traces).
But the reported reachability is REFUTED: `j` spans `[-size/2, size/2-1]` with
`center = size/2`, so angle 0 samples x = 0..w-2 and the last column is never sampled; the
common image is sized FROM the stars themselves (:604-617, x4), so the half-max crossing
measured at 36-40% of the sampled line (~2.6x margin, `zero_widths = 0` in every run).
Reaching the edge needs a flat top, which is flagged `overexposed` and filtered at :564.
Also REFUTED: the claim that a spurious 0 inflates ovality via the perpendicular index. An
open line is open at BOTH ends, so the zeroed band is centrally symmetric and the
perpendicular of the largest CLOSED chord is itself closed; synthetic cases gave ovality
0.5/1.0 against a true 2.25, i.e. an UNDER-estimate. Gap-bridging is partially real
(`over,under,over,under` spans first-to-last including the gap) but unreachable for the same
reason. Resolution (hardening only, no migration): treat an unclosed run as
`width = j_end - first + 1`, or skip the angle.

### [ ] Low. `get_stars` is the 3rd-longest function in the tree and mixes six responsibilities
Location: src/image/stars.rs:315-489 (also `CommonStarsImage` at :649-765)

Measured exactly: 175 lines, 4 nested loops plus an innermost `if` (depth 5), a 34-line
`fill()` closure over NINE mutable captures (`min_x,max_x,min_y,max_y,star_points,x_summ,
y_summ,crd_cnt,brightness`) — not seven — and SIX responsibilities (background estimation,
flood-fill segmentation, centroid photometry, morphology rejection, area-outlier filtering,
truncation). Census of `src/image` + `src/core` (590 functions): median 11 lines, mean 21,
only 3 functions >= 175 — `process_command` (596), `apply_calibr_data_and_remove_hot_pixels`
(188), `get_stars` (175) — so this is large but within this codebase's norms.
Correction to the original report: the cited load-bearing ordering is wrong. Centroid (:436)
and width/height (:434-435) read only flood-fill accumulators that the dilation never
touches, so they are INVARIANT to dilation position. The genuinely load-bearing (and
uncommented) ordering is: dilation must run AFTER the `all_star_coords` insert (:409-411)
and BEFORE `check_is_star_ok` (:433), `check_is_star_overexposed` (:438) and the `points`
capture (:446).
Resolution: no refactor is warranted on its own — the ordering is subtle, the closure has 9
interdependent captures, and there is neither a benchmark nor a unit test that would catch a
silent change to the detected-star set. If anything, add tests FIRST, then extract the
dilation and the area-outlier filter. Splitting `CommonStarsImage` into `star_metrics.rs` is
optional taste: the type is private and its only producer consumes `&[Star]`, and `AGENTS.md`
already groups "stars" under `src/image`.

#### Rejected after recheck (do not "fix" these)
- Subtraction underflow at :170/:202/:203 for short images: `||` short-circuits on `y < 16`,
  so `height - 16` is only ever evaluated when `height >= 17` (:170) or `height >= 32`
  (:203), and `width - 16` only after `calc_threshold` has guaranteed `width >= 32`. Measured
  `border_subtractions_evaluated = 0` for 1000x10 and 1000x1, identical in debug and `-O`
  builds; a wide-but-short layer simply yields zero stars, correctly and profile-independently.
- "Zero-star frame reported as good quality" as a defect: a deliberate, codebase-wide
  fail-open when a metric cannot be measured.
- Uninterruptible-cancellation framing and the proposed `par_chunks_mut` / `&AtomicBool`
  stop-flag changes: `stop_flag` is checked 11 times in `process_command`, including :517
  immediately before and :583 immediately after the detection segment, so cancel latency is
  bounded by one segment, not the pipeline; abort only sets an atomic and needs no image
  write lock; the UI is a separate thread. `par_chunks_mut` over `calc_common_star_image`
  would be correctness-safe (the star set is immutable shared data; only the `values` scratch
  at :623 must become per-chunk) but is a likely net loss on a Pi at typical sizes (~40x40
  output, ~60 stars, a few ms) due to rayon overhead plus pool contention with
  `find_extremums`. Real (separate, throughput) note worth keeping in mind: star detection
  runs on EVERY light frame, and twice per frame in live stacking (:539 and :743).
- The reviewer's counter-claim that another section's `stars.rs:631`/`:723` `get_f64_crd`
  entry is affected is itself refuted — see the correction recorded in the `image.rs` section.

#### Verified clean
- No lock is taken in this file; `find_extremums_try` shares only an `AtomicUsize` plus a
  `&ImageLayer<u16>` across rayon workers with per-chunk output vectors, no nested
  `par_iter`, no `unsafe`; `overexp_buffer` is reachable only through `&mut self`. No
  deadlock or race possible. No IO at all, so no sync-IO-in-async issue.
- No 32-bit / `overflow-checks=false` hazard on these paths: detection math is u16/i32 with a
  guard at :128-130, composite math is i64, `avg_width` sums are <= 9,900, and
  `width*height` at :326 cannot overflow `u32` (24 MP << u32::MAX).
- The hand-rolled `median`/`median3`/`median4`/`median5`/`median9`, `FloodFiller` and block
  median filter are deliberate speed choices, not duplicates of a project dependency.

## src/core/mode_camera.rs

### [x] Severe. The camera-offset gate ignores `calibr.dark_frame_en`, so a flat whose master is disabled still gets offset 0
Location: src/core/mode_camera.rs:395-419 (conditions at :395-399, `is_file()` at :409); consumer `src/image/raw.rs:741-742`; option `src/options/calibration.rs:11` (default `true` at :21, checkbox `src/ui/ui_camera.rs:833`), read as `extract_dark` at `src/core/engine.rs:388`, applied at `src/core/raw_calibration.rs:56-73`, `:172-181`
The gate that decides whether to measure `camera_offset` tests only `frame.offset != 0` and `!subtract_file_name.is_file()`; it never reads `options.calibr.dark_frame_en`, while the subtraction itself runs only when `extract_dark` is set. So with the calibration checkbox off and the master file present, neither side acts: `subtract_image` stays `None`, `subtract_dark_or_bias` never runs and `data_offset` stays 0, yet the samples still carry the camera's ADC zero (`mode_camera.rs:1081-1118` exists precisely because that value is not known a priori); and the measurement is skipped because the file exists, so `camera_offset` stays `None`. The flat then reaches `normalize_flat(None)`, which resolves to `unwrap_or(0)` at `raw.rs:742` — offset 0 against data that is not pedestal-free. Magnitude scales with the `offset` setting (harmless when it is 0, because then there is no pedestal either), and it is the same mechanism as the master-exists case in the `raw.rs` section, only with a different constant: there the leftover pedestal is the mean illumination, here it is the whole ADC zero. Reachable whenever a master flat is built (`SavingRawFrames` + `raw_frames.create_master`) with the checkbox off and a matching master file in the dark library; the weakened master is then saved and reused by every light.
Second symptom of the same gate: `data_offset == 0` cannot be read as "no master was applied", because "file absent" and "file present but disabled" are indistinguishable downstream.
Written against the tree where the `raw.rs` pedestal fix has landed (`RawImage` derives `Clone`, `normalize_flat` takes `Option<i32>`); the master-exists case from the `raw.rs` section is fixed there, this one is not.
Resolution: make the gate use the same condition as the subtraction — read `options.calibr.dark_frame_en` in the `:395-419` block and measure unless `dark_frame_en && subtract_file_name.is_file()`. Add a test asserting that normalizing a synthetic pedestal-carrying flat does not depend on the presence of a master file in the dark library.
DONE: The gate now reads `options.calibr.dark_frame_en` under the same options lock and measures via a new `TakingPicturesMode::need_camera_offset_calc(dark_frame_en, master_exists) == !(dark_frame_en && master_exists)`, mirroring the subtraction gate in `RawCalibration`. The second symptom is closed by construction: the measurement now runs exactly when the subtraction does not, so `camera_offset == Some` iff no master was applied. Tests intentionally not added (the predicate is a one-line mirror of the subtraction gate). Note: with `dark_frame_en` off and a master present, the 8 offset-calc shots are now taken before the flat sequence (same cost as the file-absent case).
