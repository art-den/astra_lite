# starsolver

A Rust clone of a small subset of `astrometry.net solve-field`: given star pixel
coordinates (a `.xyls` FITS file) and a directory of index files, it plate-solves
the field and prints the WCS (center RA/Dec, field size, rotation, parity) to
stdout. The solver is implemented and matches the reference within tolerance.

The scope is frozen (see Constraints); solver tunables are frozen constants in
`src/params.rs` (mostly chosen to match `solve-field`) and none are exposed on
the CLI. The solver is complete: `cargo fmt --check` and `clippy -D warnings`
pass, `README.md` documents the CLI.

Original software github: https://github.com/dstndstn/astrometry.net

Original sources are in the `orig_astrometry.net` folder.
There should be no links to the original sources in the source comments!

## How it works

Pipeline (`src/sweep.rs::solve_field`): read `.xyls` stars, then run the
`solve-field` preprocessing that reproduces its star order (`augment.rs`) ->
`run_into` keeps the brightest `params::TOP_OBJS` stars
(`xyls.rs::brightest_first`, descending flux, stable) -> select index files by
scale range (largest quad size first) -> sweep in object-count rounds (stars
1-10, 11-20, ... up to the shorter of `params::TOP_OBJS` and `MAX_OBJS`); each
round
issues its indices in consecutive chunks of `params::INDEX_FANOUT` attempts
that run concurrently and are folded back in sweep order -> per index:
generate field quad codes, query the codes kd-tree, fit a TAN WCS per hit, drop
the fit on a cheap NN probe (`verify.rs::verify_probe`) unless it looks real,
then verify against index stars and compute log-odds; a solved hit is refined on
a tight matched-pair set (`refine.rs`), re-centred to the field-center tangent
frame (`wcs.rs::Wcs::recentered_at`) and reported from there.
With `--ra/--dec` the index selection prunes healpix tiles that cannot overlap
the hint circle (`sweep.rs::tile_in_hint`) and solutions/matched stars outside
the circle are rejected. From round 2 on a hinted attempt also knows which quads
of its own file the circle can reach at all (`index.rs::Index::hint_index`,
cached per file path in `sweep.rs`): a file with none leaves the sweep, and a
file with some is queried through a compact tree over only those code records
(`kdtree.rs::KdTree::filtered`) instead of the file's own codes tree. A sweep
keeps such trees for the first `params::HINT_TREE_MAX_FILES` files of its list;
the rest search their own tree and only use the verdict.

Invariants that are easy to break in future edits:

- The input star order is part of the pipeline contract: `xyls::load` returns
  the position-interleaved order preprocessing produces, not file order and not
  descending flux. `run_into` then stably sorts that list by descending flux and
  truncates it to `params::TOP_OBJS` (`xyls::brightest_first`), so the depth-ramp
  window, the verification walk and refine all run on the brightest prefix in
  descending-flux order; the sort is stable, so equal fluxes - and a file with no
  FLUX column - keep the preprocess order. Paths that enter `solve_field`
  directly (`testutil`, the `sweep` unit tests) still pass the uncapped order.
- A prior may narrow the search, not move the answer: the tolerance gate solves
  the same fields under different priors and compares the prints
  (`ground_truth.rs::priors_do_not_move_the_solution`), because the reported
  winner must not depend on which index files the hint pruned. A negative leg is
  only worth its name if it had something to reject, so each one also checks that
  index files were selected, and `reject_beyond_hint` that a finished solution was
  actually reached and discarded.
- A hint-filtered codes tree must answer a query with exactly the records the
  file's own tree answers that also pass the pre-fit star gate. A tree built by
  `KdTree::filtered` prunes on its own split planes, so that equivalence is never
  true by argument, only by test
  (`kdtree.rs::tests::filtered_codes_tree_returns_the_source_hits_that_pass_the_filter`,
  `crafted_filtered_tree_matches_a_linear_scan`,
  `crafted_filtered_tree_deep_subset_matches_a_linear_scan`,
  `index.rs::tests::hint_index_matches_the_star_by_star_gate`), and the
  `process_hit` gate stays in force as the sole accept test of a hinted hit.
- A dropped index is one whose cached verdict proved it can produce no hit, so the
  drop removes neither a fit nor an `Err` at its position; the price is that a file
  unreadable only after its verdict is cached is no input error at all, so a run
  can solve (exit 0) where reading it gave exit 2. And every file gets its verdict
  in the round that first fills the cache, where the sweep list is still `selected`
  - that is what makes spending the tree budget by list position sound.
- stdout is the contract; stderr is not (verbose per-index lines of one chunk
  interleave). The printed solution must not depend on rayon scheduling: the
  winning attempt runs to its end and its capture slot keeps the
  lexicographic-best `(log-odds, quad)` solved hit (`sweep.rs::better_hit`);
  the chunk fold reproduces the sequential loop's precedence and cancels only
  strictly later attempts. `solve_field`'s callback never returns
  `Flow::Stop`, and the pre-verification probe - which decides which hits are
  verified at all - must stay a pure function of `(index, hit, stars, frame)`.
- Quad enumeration is parallelized with rayon per `(A,B)` backbone pair; the
  `on_hit` callback runs on workers - keep it `Sync` and self-guarded, and do
  not spawn rayon work from inside an attempt (`verify_hit` holds its
  thread-local `Scratch` `RefCell` borrow for the whole call).
- `--cpulimit` is checked at the top of a chunk and re-checked in the fold, so
  deadline granularity is chunk-sized.

Modules (all in `src/`, no external FITS crate - the reader is hand-rolled):

| Module | Role |
|---|---|
| `lib.rs` | crate root: public API (`Args`, `run`, `run_into`, `Solution`) and the glue that resolves the index dir, loads stars, selects indices, drives the sweep; other modules stay private |
| `main.rs` | thin binary: clap parse + exit-code mapping |
| `cli.rs` | CLI surface (flag names follow `solve-field`), scale/angle parsing |
| `sweep.rs` | the index sweep: depth ramp, index selection + hint gate, chunked concurrent fold with the deterministic winner |
| `output.rs` | success-block output: C `%g` formatting, sexagesimal strings, `write_solution` |
| `xyls.rs` | `.xyls` reader (BINTABLE X/Y); frame-size fallback to `IMAGEW`/`IMAGEH` headers; hands the list to `augment::preprocess`; `brightest_first` is the brightest-first cap the pipeline list comes from |
| `augment.rs` | the `.xyls` preprocessing `solve-field` applies before solving (must stay bit-exact with numpy) |
| `fits.rs` | minimal FITS reader: mmap'd lazy byte views, headers-only mode for directory scans |
| `index.rs` | old-format index loading (`AN_FILE='QUAD'`); all-sky (`HEALPIX=-1`) and xy-scheme healpix tiles (`HPNCOMP=NESTED` refused); `Index::hint_index` -> `HintIndex`, the per-file hint verdict and its compact codes tree |
| `indexdir.rs` | index directories when `--index-dir` is absent: cfg `add_path` dirs, then built-in install dirs |
| `healpix.rs` | healpix cell centers (xy-scheme) and the tile/hint overlap test |
| `kdtree.rs` | kd-tree over the quantized arrays as the index files store them; callback-based `ball_search_visit`/`try_ball_search_visit`; hot decode hoisted into a per-traversal `Scan` that must stay bit-identical to the generic accessors |
| `codes.rs` | field quad/code generation (the quad-code convention is the critical detail) |
| `wcs.rs` | TAN projection, WCS fit from a quad, report recentring, trig-free star projection |
| `verify.rs` | verification and odds (first-come matching, `effA = W*H`, per-worker `Scratch`, x-windowed NN scan) and the pre-verification probe that rejects garbage fits (one star-tree search per probe star, or one footprint ball for sparse frames) |
| `refine.rs` | post-verification "tweak-lite": tight-radius pair matching, iterated similarity refit, parity/residual guard; a reject never unsolves a field |
| `solver.rs` | solve loop (codes -> kd-tree hits -> WCS fit), `PositionHint` circle geometry, cooperative stop of `FieldSpec` |
| `coords.rs` | unit-sphere xyz / RA-Dec math |
| `params.rs` | all solver tunables frozen as constants (`INDEX_FANOUT` and the star-list cap `TOP_OBJS` among them) |
| `genfield.rs` | synthetic fields with exact sky truth, tests only, behind the non-default `test-util` feature |
| `testutil.rs` | `#[cfg(test)]` helpers: index-set discovery (`field_run` tells which index solved a field), `testdata/` gating (gates self-skip without data; `ASTROMETRY_REQUIRE_TEST_DATA=1` turns a skip into a panic), shared field-truth consts and gate budgets |

## Data directories

- Index files are not in the repository: the solver and the tests use the
  astrometry.net install found on the machine (`/etc/astrometry.cfg` ->
  `add_path /usr/share/astrometry`). `ASTROMETRY_TEST_INDEX_DIR` (a path list)
  overrides the discovery for tests. Which index solves which field is
  discovered at runtime (`testutil::field_run`), never pinned in a test.
- `testdata/` - the input `.xyls` samples, committed with the repository; the
  only data the tests still need locally, and only for the gates on the
  shipped fields (generated fields build their own `.xyls` from the installed
  indices). For the `<WxH>-N.xyls` fields the frame size is encoded in the
  filename; `platesolve_*.xyls` carries `IMAGEW`/`IMAGEH` header keys instead.
  `testdata/astro-*.xyls` are dense real-observation fields (up to ~30k
  stars): benches run all of them, and only the ones carrying `FieldTruth`
  entries in `tests/ground_truth.rs` join the tolerance gate.
- `.tmp/` - the one scratch root for generated data (gitignored, safe to
  delete); reference `solve-field` runs go there, never next to the input.

## Build & validate

```
cargo build --release
./target/release/starsolver -w 4656 -e 3522 testdata/4656x3522-1.xyls   # system indices
cargo test                                 # unit + integration tests incl. the
                                           # tests/ground_truth.rs tolerance gate,
                                           # every field under each prior variant
                                           # (blind, --ra/--dec/--radius, --scale-*)
                                           # and three legs that must not solve,
                                           # those on a three-field shortlist
                                           # (needs the installed indices; testdata/
                                           # only for the gates on the shipped fields)
cargo test --release                       # the same gate at the release-calibrated
                                           # budgets (debug scales them x4)
GATE_FIELD=4656x3522-1 cargo test --test ground_truth   # one field of the gate
GATE_VARIANT=hint_tight cargo test --test ground_truth  # one variant of it (its
                                           # own test fn: gate_blind, gate_hint_*
                                           # gate_scale_*, gate_reject_*)
ASTROMETRY_REQUIRE_TEST_DATA=1 cargo test  # a data skip fails instead of passing
cargo test --test generated_fields         # only the generated fields: no testdata/ needed
cargo clippy --all-targets --features test-util -- -D warnings   # the lint gate;
                                           # --all-targets so the test-only modules are linted
cargo check --all-targets                  # NOT plain `cargo check`: that skips the
                                           # test-only `genfield`/`testutil`
```

After each edit: diagnostics on the changed file -> `cargo check --all-targets`
-> `cargo test`.

`test-util` must stay off a normal build; check with
`cargo tree -e features,no-dev` (or that the release binary holds no `genfield`
symbols). Hidden debug flag `--dump-index <FITS>` dumps one index file's tables.

`python3 scripts/bench.py` times starsolver alone (the reference `solve-field`
is deliberately not benchmarked) on every `testdata/*.xyls` blind and prints
per-field wall times; for three of them (`--reject-files`) a second table times
a run under a positional prior that cannot contain the field (`--ra/--dec` at
its antipode, `--radius 3`) and must not solve
(`--repeat N`, `--files <stems>`, `--skip-build`, `--index-dir`).

### Running the reference `solve-field`

The installed `/usr/bin/solve-field` writes outputs (`.axy .match .rdls .corr
.wcs .solved -indx.xyls`) next to the input by default - never let it: route
every output into `.tmp/` or disable each with the literal filename `none`
(`--match none ...`; `.axy` cannot be `none`, use `--temp-axy`).
**Gotcha:** the console summary is read back from the WCS file, so keep
`--wcs <path in .tmp/>` or it prints "Did not solve" even on success.
**Gotcha:** it exits 0 whether or not the field solved, so success is only the
center line it printed (or the WCS file it left behind), never the exit code.
Pin the index with `--index-file` (not `--index`, which getopt rejects), plus
`-w/-e` and a scale range; use `--no-plots` and `--temp-dir /dev/shm/...`.

## Constraints

- Monolithic console app: crate = library target + thin bin (public API is
  `run`/`run_into`), every module private except the test-only `genfield`
  behind the non-default `test-util` feature. Dependencies stay minimal:
  anyhow, clap, memmap2, rayon, nothing else.
- Memory-constrained targets (2 GB ARM SBCs): index files are mmap'd and read
  lazily, never copied into anonymous memory; peak RSS target < 200 MB. This
  is what bounds `INDEX_FANOUT` at 3 (~47 MiB per extra concurrent index).
- Frozen scope: `.xyls` input only, stdout-only output, old QUAD index format
  only (all-sky and ring/xy-scheme healpix tiles; new index format, H4,
  `HPNCOMP=NESTED`, multiindex are out of scope), no CLI-exposed tunables.
- Bit-exact match with `solve-field` is not required; correctness is defined
  by the tolerances in `tests/ground_truth.rs` (center 1.5 px of the field's
  own scale, rotation 0.5 deg, scale/size 1%/0.5%, parity exact, with
  per-field overrides in its `FieldTruth` table). Printed rotation/scale are
  in the field-center frame.
- Comments in code state how and why our code works, in English, on their own:
  no citations of the original program's sources (no paths into them, no line
  numbers, no identifiers taken from its code). Names we are compatible *with*
  are fine (`solve-field` flags, FITS header keys). A comment must not depend
  on data that only exists on one machine (installed index set, `.tmp/`).
- Exit codes: 0 solved, 1 not solved, 2 input/format error.

## Keeping this file short

This file must stay brief: keep only the key facts needed to understand the
project and to run its tests. Anything an agent can rediscover from the
code, `README.md` or `--help` does not belong here: no timing provenance, no
optimization history, no machine-specific details. When editing, prefer
cutting to adding.
