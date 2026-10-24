# starsolver

A Rust clone of a small subset of `astrometry.net solve-field`: given star pixel
coordinates (a `.xyls` FITS file) and a directory of index files, it plate-solves
the field and prints the WCS (center RA/Dec, field size, rotation, parity) to
stdout. The solver is implemented and matches the reference within tolerance.

The scope is frozen (see Constraints); solver tunables are frozen constants,
mostly chosen to match `solve-field` (a few, like `MAX_OBJS`, are project
decisions) and none are exposed on the CLI. The solver is complete:
`cargo fmt --check` and `clippy -D warnings` pass, `README.md` documents the CLI.

Original software github: https://github.com/dstndstn/astrometry.net

## How it works

Pipeline (`src/sweep.rs::solve_field`): read `.xyls` stars (then run the
`solve-field` preprocessing that reproduces its star order, `augment.rs`) ->
select index files by scale range (tried largest quad size first,
`sweep_cmp`) -> sweep in object-count rounds (stars 1-10, 11-20, ... by steps
of 10 up to `MAX_OBJS`), one index in memory at a time -> per index: generate
field quad codes, query the codes kd-tree, fit a TAN WCS per hit, verify
against index stars and compute log-odds; a hit that verifies as solved is
refined on a tight matched-pair set (`refine.rs`, 4-parameter "tweak-lite"),
re-centred to the field-center tangent frame (`wcs.rs::Wcs::recentered_at`)
and reported from there, so the printed rotation/scale/center form the
field-center frame a consumer can rebuild from. With `--ra/--dec` the index
selection additionally prunes healpix sub-region tiles whose cell cannot
overlap the hint circle (`sweep.rs::tile_in_hint`: all-sky indices are always
in range, blind mode tries every tile); the hint also rejects code hits whose
matched stars fall outside the circle (before fit/verify) and discards a
finished solution whose (refined) center lies outside it.

The input order is part of the pipeline contract: `solve-field` preprocesses a
`.xyls` before solving without a flux sort (drop the stars in a dense source
line, then spread every group over the field), so `xyls::load` returns that
position-interleaved order, not the file order and not descending flux. The
depth ramp only matches that per-round star set because of it (`1920x1080-2`
blind: 243 s when the list arrives flux-sorted vs ~5 s here; the reference run
of the same field recorded 23.9 s of engine CPU). The four shipped fields
solve in ~5.9 s pooled wall time (min-of-3, blind: 0.25/0.56/0.25/4.9 s) vs
~25.8 s of pooled engine CPU recorded in the entry comments of
`tests/ground_truth.rs`; letting the whole round of the winning index run to its
end (the deterministic-winner rule below) costs +4.7% on top of the pre-rule
total (~0.26 s pooled). Our own
timings in this section are dev-box measurements; this file is their
provenance.

Quad enumeration is parallelized with rayon per `(A,B)` backbone pair. The
`on_hit` callback runs on workers: keep it `Sync` and self-guarded.
`solve_field`'s callback never returns `Flow::Stop` (solver-level Stop stays
for the tests): the round of the first-solving index runs to its end and the
capture slot keeps the lexicographic-best `(log-odds, quad)` solved hit of the
round (`sweep.rs::better_hit`), so the hit set of a round - and stdout - no
longer depend on rayon scheduling (a `--cpulimit` deadline that trips
mid-round truncates the round; its winner is timing-dependent by design).
Historically the racing variants of `4656x3522-2` printed a bimodal raw fit
1.196 deg of rotation apart; reporting from the field-center tangent frame
(`wcs.rs::Wcs::recentered_at`), refining before that report (`refine.rs`) and
keeping one deterministic winner per round removed the accuracy gap by
construction: every solved hit of the winning round converges through
refine+recentre to one fixed point, which is the field's published sky truth
(`refine::tests::refined_solved_hits_converge_to_one_sky_point` and
`sweep::tests::blind_field2_print_matches_sky_truth`), so
the end-to-end gate `tests/ground_truth.rs` is again a single-shot proof of the
printed WCS.
The concrete log-odds/rotation numbers behind that conclusion were properties
of one index file; the suite no longer pins index-level numbers (see
"Data directories").

Modules (all in `src/`, no external FITS crate - the reader is hand-rolled):

| Module | Role |
|---|---|
| `lib.rs` | crate root: the module declarations, the public API (`Args`, `run`, `run_into`, `Solution`) and the glue `run_into()` that used to live in `main.rs` - resolving the index dir, loading the stars, selecting indices, driving the sweep, plus the `--dump-index` branch; the modules below stay private |
| `main.rs` | thin binary: clap parse + exit-code mapping (0 solved, 1 `Field not solved`, 2 `Error: {e:#}`) |
| `cli.rs` | CLI surface (`clap` `Args`, flag names follow `solve-field`), scale/angle parsing, `SolveOptions` (CLI units converted; frame-size fallback to the .xyls `IMAGEW`/`IMAGEH` headers) |
| `sweep.rs` | the index sweep: the object-count depth ramp, index selection + the `tile_in_hint` position gate, and `solve_field` (drives `solve_index`; `RoundCapture` holds the deterministic winner) |
| `output.rs` | success-block output formatting: the C `%g` number formatting, the sexagesimal strings, `write_solution` (into any `Write`) |
| `xyls.rs` | input `.xyls` reader (BINTABLE X/Y columns); frame-size fallback to the `IMAGEW`/`IMAGEH` header keys when `-w`/`-e` are omitted (the pair is all-or-nothing); hands the list to `augment::preprocess` |
| `augment.rs` | the `.xyls` preprocessing `solve-field` applies before solving: drop the stars in a dense source line, then spread every group over the field by position (must stay bit-exact with numpy) |
| `fits.rs` | minimal FITS reader: headers, BINTABLE columns (`Fits` mmaps the file and exposes it through lazy byte views (`BlobView`), `FitsHeaders` reads headers only, which is what an index-directory scan uses) |
| `index.rs` | old-format index loading (`AN_FILE='QUAD'`, `quads` + `codes`/`stars` kd-trees); all-sky (`HEALPIX=-1`) and healpix sub-region tiles (`HEALPIX>=0` + `HPNSIDE`, xy-scheme numbering; `HPNCOMP=NESTED` refused) |
| `indexdir.rs` | index directories when `--index-dir` is absent: cfg `add_path` dirs, then the built-in install dirs; unusable indices there are skipped, not fatal |
| `healpix.rs` | healpix cell centers (the xy-scheme numbering our index reader accepts) and the conservative tile/hint overlap test for the position gate |
| `kdtree.rs` | kd-tree over the quantized arrays as the index files store them |
| `codes.rs` | field quad/code generation (the quad-code convention is the critical detail) |
| `wcs.rs` | gnomonic (TAN) projection + WCS fit from a quad + the report recentring (`Wcs::recentered_at`, analytic tangent-frame Jacobian) |
| `verify.rs` | verification and odds (first-come matching, `effA = W*H`; no radius-of-rejection filter, no conflict resolution) |
| `refine.rs` | post-verification "tweak-lite": tight-radius `match_pairs` (3 px, anchors kept), iterated similarity refit, parity/residual guard; a reject keeps the raw WCS and never unsolves a field |
| `solver.rs` | solve loop: codes -> kd-tree hits -> WCS fit; also the `PositionHint` struct (`precompute`/`contains`), the single source of the hint-circle chord^2 geometry shared by the pre-fit solver gate and the post-fit capture gate |
| `coords.rs` | unit-sphere xyz / RA-Dec math |
| `params.rs` | all solver tunables frozen as constants (mostly chosen to match `solve-field`; not exposed on CLI) |
| `testutil.rs` | `#[cfg(test)]` helpers: the index set of the machine's astrometry.net install (`metas`, `smallest_index`, `dim4_index`, `field_run` - which index/round actually solved a field) plus `testdata/` gating; every gate self-skips when its data is absent, `ASTROMETRY_REQUIRE_TEST_DATA=1` turns a skip into a panic. Also the shipped fields' sky truth consts and the gate budgets, pinned against `tests/ground_truth.rs` so the two hardcoded copies cannot drift. The out-of-crate targets under `tests/` (`common`, `run_pipeline`, `ground_truth`, `cli_contract`) re-implement that gating for integration use |

## Data directories

- Index files are not in the repository and have no directory of their own:
  both the solver and the tests use the astrometry.net install found on the
  machine (`/etc/astrometry.cfg` -> `add_path /usr/share/astrometry`: 96 old-format
  2MASS files, ids 4204..4219 - healpix tiles 4204..4207 and all-sky 4208..4219).
  `ASTROMETRY_TEST_INDEX_DIR` (a path list) overrides the discovery for tests.
  On this install the four shipped fields are solved by 4211 (`4656x3522-1`),
  4212 (`4656x3522-2`) and the healpix tile 4206/2 (both `1920x1080-*`) - the
  tests DISCOVER that at runtime via `testutil::field_run`, they do not pin it,
  and no test names an index id. The `index_id` `solve-field` used per field is
  recorded in the entry comments of `tests/ground_truth.rs`, as provenance of
  those reference runs.
- `testdata/` - input `.xyls` samples, also not in the repository (gitignored,
  never committed); image size is encoded in the filename
  (e.g. `4656x3522-1.xyls`), not in the file. The only data the tests still need
  locally.
- `.tmp/` - the one scratch root for generated data (gitignored, safe to
  delete): `.tmp/out/` `solve-field`'s per-run outputs for the `4656x3522-*`
  fields (`.log`/`.summary` are the captured console), `.tmp/runs/` the full
  artifact set of another run of those fields, `.tmp/runs1080/` the
  artifacts plus run logs for the two `1920x1080-*` fields (provenance for
  the reference runs their gate entries record).
  Anything else found under `.tmp/` is incidental scratch. There is no
  repo-local copy of the reference-run record: it lives in the entry comments of
  `tests/ground_truth.rs`, and the scripts that once assembled it
  (`summarize.py`, `run_reference.sh`) are gone.

## Build & validate

```
cargo build --release
./target/release/starsolver -w 4656 -e 3522 testdata/4656x3522-1.xyls   # system indices
cargo test                                 # unit + integration tests incl. the tests/ground_truth.rs
                                           # tolerance gate, blind and with --ra/--dec
                                           # (need the installed indices + testdata/)
cargo test --release                       # the same gate at the release-calibrated budgets
GATE_FIELD=4656x3522-1 cargo test --test ground_truth   # one field of the gate
ASTROMETRY_REQUIRE_TEST_DATA=1 cargo test  # same, but a data skip fails instead of passing
```

After each edit: diagnostics on the changed file -> `cargo check` -> `cargo test`
(the solver tolerances gate inside it, `tests/ground_truth.rs`; its time budgets
are release-calibrated and scaled x4 under the debug profile, while
`cargo test --release` applies them as written). The gate uses the indices
installed on the system (`/usr/share/astrometry` here);
`ASTROMETRY_TEST_INDEX_DIR` overrides that. Hidden debug flag
`--dump-index <FITS>` dumps one index file's tables (index-format work).

### Running the reference `solve-field` for tests (nothing outside `.tmp/`)

The installed `/usr/bin/solve-field` is the reference C binary (0.98); it
drives the astrometry engine and, by default, writes a set of output files
next to the input: `.axy .match .rdls .corr .wcs .solved
-indx.xyls` (`.new` only for FITS-image inputs, never for `.xyls`). Never let
it write next to the input: route every output into `.tmp/` (gitignored), or
disable it for a throwaway run. To get only the console summary:

- Disable each output with the literal filename `none`: `--match none --rdls none
  --corr none --solved none --index-xyls none` (`--new-fits none` for images).
- The `.axy` output cannot be set to `none`; use `--temp-axy` (writes it to a
  temp file that solve-field deletes) or `--axy <path>` to keep it.
- Plots off with `--no-plots`; keep engine temp files in RAM with `--temp-dir
  /dev/shm/...`.
- **Gotcha:** the console summary (`Field center/size/rotation/parity`) is read
  back from the WCS file, so `--wcs none` makes the engine still solve but print
  `Did not solve (or no WCS file was written)`. Keep WCS, routed into `.tmp/`
  - a WCS file is unavoidable for the summary.

Ready command (run from the project root; writes only under `.tmp/runs/`, which
is gitignored and can be blown away anytime):

```
mkdir -p .tmp/runs
d=$(mktemp -d /dev/shm/sf.XXXXXX)
solve-field --no-plots --overwrite --temp-dir "$d" --temp-axy \
  --match .tmp/runs/4656x3522-2.match --rdls .tmp/runs/4656x3522-2.rdls \
  --corr .tmp/runs/4656x3522-2.corr --solved .tmp/runs/4656x3522-2.solved \
  --index-xyls .tmp/runs/4656x3522-2-indx.xyls --wcs .tmp/runs/4656x3522-2.wcs \
  --index-file /usr/share/astrometry/index-2mass-12.fits -w 4656 -e 3522 \
  -u arcsecperpix -L 2 -H 4 testdata/4656x3522-2.xyls
rm -rf "$d"
```

Pin the solving index with `--index-file` (not the ambiguous `--index`, which
getopt rejects) plus `-w/-e` and a scale range so a specific ground-truth field
reproduces fast instead of sweeping the whole system index directory. For a run
whose artifacts are not wanted, swap each `.tmp/runs/...` value for the literal
`none` (the WCS still needs a `.tmp/` path or the summary is not printed).

## Constraints

- Monolithic console app (crate = library target + thin bin: the public API is
  `run`/`run_into`, every module stays private); keep dependencies minimal
  (currently: anyhow, clap, memmap2, rayon, and nothing else - the cargo gates
  need no crates of their own, they carry the field truth as constants)
- Memory-constrained targets (2 GB ARM SBCs): index files are mmap'd and read
  lazily (kd-tree data, `lr`/`split`, `quads` are never copied into anonymous
  memory; peak anonymous footprint was ~4 MB even on the largest shipped
  index), peak RSS target < 200 MB regardless of index file size; resident
  pages are clean page cache, evictable under pressure.
- Frozen scope: `.xyls` input only, stdout-only output, old QUAD index format
  only (all-sky `HEALPIX=-1` and ring/xy-scheme healpix sub-region tiles; the
  new index format, H4, `HPNCOMP=NESTED` tiles and multiindex remain out of
  scope), no CLI-exposed tunables. Bit-exact match with `solve-field` is not
  required; correctness is defined by the tolerances encoded in
  `tests/ground_truth.rs` (the unit gates in `src/testutil.rs` keep their own
  copy of them and of the field truth, pinned against the gate by
  `gate_budgets_match_the_cargo_gate` and `truth_consts_match_the_cargo_gate`):
  center 1.5 px of the field's own pixel scale and rotation 0.5 deg (the
  center is deliberately re-expressed in pixels because an absolute 1" would
  be 0.4 px on the 2.71"/px fields and 1 px on the 1.0"/px ones; the shipped
  fields measure 0.14/0.41 px of center and 0.021-0.188 deg of rotation, and
  most of that center residual is the truth's own fit noise, not ours),
  scale/size 1%/0.5%, parity exact;
  the per-field overrides (an absolute `tol_center_arcsec` center budget, a
  `timeout_s` budget) are fields of the gate's `FieldTruth` table, whose entry
  comments also record the reference run behind each field - the index that
  solved it, its log-odds and match counts, engine CPU and, where it published
  one, its full WCS. The real-centroid 2MASS fields keep their
  `tol_center_arcsec: 2.0` pin and it is NOT inflated the way an older 3 px
  one was: `solve-field` ran a SIP tweak there and our similarity-only refine
  cannot follow that quadratic term, so `1920x1080-1` measures 1.29" (1.29 px,
  1.6x of margin); shrinking it further would mean modeling distortion, out of
  the frozen scope. Printed rotation/scale semantics: field-center frame,
  within ~0.2 deg rotation / ~0.02% scale of the reference's off-center print
  on the shipped fields - that frame difference (not solver noise) is what the
  0.5 deg rotation gate is measured against.
- Comments state how and why our code works, in English, on their own: no
  citations of the original program's sources - no paths into them
  (`*.c`/`*.py`) or line numbers, no identifiers taken from its code, no
  "mirror of `<file>`" when the file named is not one of ours, no section
  anchors of foreign docs, no task ids. Names we are compatible *with* are
  fine (`solve-field` CLI flags, FITS header keys, cfg keys), and so are this
  repo's own modules and tests. The reference program's behavior may be named
  directly (`astrometry.net's solve-field`) when the compatibility itself is the
  reason for the code. A comment must not depend on
  data that only exists on one machine: the installed index set or the `.tmp/`
  scratch (a `testdata/` fixture is fine, but only inside the test that reads
  it and self-skips without it).
- Exit codes: 0 solved, 1 not solved, 2 input/format error.
