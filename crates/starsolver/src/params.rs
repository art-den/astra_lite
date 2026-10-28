//! Every solver tunable, frozen as a constant.
//!
//! None of them is a switch: a few are only the default behind a CLI flag
//! (`--cpulimit`, `--radius`, and the blind `--scale-*` range), the rest are
//! fixed behavior the CLI cannot reach. Each comment says what the value
//! controls here and what changing it would cost. Odds thresholds are written
//! as plain odds; the odds accumulator works in nats, so the gates the code
//! compares carry an `ln` twin. A constant no code reads is marked
//! "documentation only".

/// Radius of the codes kd-tree ball search, L2 distance in code units (the
/// components are dimensionless and O(1)), so it is how far a field quad's code
/// may sit from a stored index code and still be reported as a hit.
///
/// Two gates are derived from the same number: a non-backbone star's code must
/// lie within CODE_TOL of the valid-code region (`codes::in_circle` accepts the
/// disc of radius 1/sqrt(2) + CODE_TOL about (0.5, 0.5)), and the requested
/// scale range gets +/- CODE_TOL relative slack in the pre-fit |AB| scale gate
/// (`Searcher::new`); the post-fit gate uses the range with no slack.
///
/// Lower it and the index code that corresponds to the field can fall outside
/// the ball, so that hit is never fitted or verified and the field stops
/// solving. Raise it and every query returns more codes to fit and verify: the
/// ball volume grows as CODE_TOL^ndim with ndim = 2 or 4.
pub const CODE_TOL: f64 = 0.01;

/// Slack factor on CODE_TOL for the canonicalization filters a field code must
/// pass before it is queried at all: `Searcher::new` uses
/// `margin = CXDX_MARGIN_FACTOR * CODE_TOL`, and a rejected variant is simply
/// not queried (`codes::canonical_ok`).
///
/// The margin is what makes that pruning safe instead of lossy. Stored codes
/// obey the canonicalization their index declares exactly, a query may sit up
/// to CODE_TOL from the code it matches, and passing from one to the other can
/// shift the filter's difference by at most sqrt(2) * CODE_TOL (the two
/// coordinate deltas bounded by Cauchy-Schwarz). 1.5 > sqrt(2) so nothing inside
/// the ball is pruned; below sqrt(2) true hits would be dropped silently.
/// A larger factor costs only pruning strength - more variants get queried.
pub const CXDX_MARGIN_FACTOR: f64 = 1.5;

/// Parity is always tried both ways: `Searcher::variants` queries each field
/// code together with its parity-flipped copy (and both 180-degree `1 - code`
/// variants), and no gate depends on the field's handedness.
///
/// Documentation only. Trying one parity would drop half of the queries and
/// make every field of the other handedness unsolvable, so there is nothing to
/// switch here.
pub const PARITY_BOTH: bool = true;

/// Prior probability that a detected field star has no index counterpart
/// (noise detection, moving object, source below the index's flux limit). It is
/// `delta` in `verify::odds_scan_inner`: a star that adds no match contributes
/// `ln(delta + (1-delta) * matches/nr)` nats, and a match's Gaussian peak
/// carries the factor (1-delta).
///
/// The solve gate is an absolute ln(1e9), so this value sets the price of a
/// miss: at 0.25 each unmatched star costs ln(0.25) = 1.4 nats early in the
/// scan. Raise it and junk stars become nearly free, so a wrong fit can climb
/// to the gate; lower it and a real solution with many unmatched detections -
/// or a sparse index - never accumulates enough odds.
pub const DISTRACTOR_RATIO: f64 = 0.25;

/// Field side of the per-star match sigma used in verification:
/// `sigma2 = VERIFY_PIX^2 + (INDEX_JITTER_ARCSEC / arcsec_per_pix)^2`, in px^2
/// (`verify::verify_hit`). It stands for centroid error plus the raw fit's own
/// error, so unlike the jitter term it does not scale with the field.
///
/// sigma2 then sets three things at once: the dedup radius (a star within 1
/// sigma of an earlier one in list order never reaches the scan), the match
/// search radius, and the width of the Gaussian that pays each match. Shrink it
/// and real pairs fall outside the radius, get counted as distractors and the
/// odds collapse; grow it and chance pairs are banked as matches, inflating
/// log-odds toward the ln(1e9) gate.
pub const VERIFY_PIX: f64 = 1.0;

/// Index side of the same sigma: assumed positional uncertainty of an index
/// star's sky coordinates, arcsec, converted to pixels by the fitted field
/// scale as `(INDEX_JITTER_ARCSEC / arcsec_per_pix)^2`.
///
/// Our index reader never reads a per-index uncertainty - the files carry no
/// such value - so this is a fixed assumption about the catalog. Its weight is
/// field-scale dependent: on the shipped ~2.7 "/px fields it adds
/// (1/2.7)^2 = 0.14 px^2, small next to VERIFY_PIX^2, while on the ~1 "/px
/// fields it doubles sigma2 to 2. Direction of change is as for VERIFY_PIX,
/// but tuning it alone moves the wide-field and narrow-field tolerances by
/// different amounts.
pub const INDEX_JITTER_ARCSEC: f64 = 1.0;

/// Floor for the odds accumulator, as plain odds: once the running log-odds of
/// one hit drops below ln(this), `verify::odds_scan_inner` abandons the rest of the
/// field list and sets `VerifyResult::bailed`. Documentation only as a
/// constant: the value the code compares is `BAIL_THRESHOLD_LN` below, and the
/// threshold itself is live.
///
/// A time-saver for hopeless hits, not a correctness gate. At
/// DISTRACTOR_RATIO = 0.25 a star with no match costs ~1.4 nats, so the floor
/// is about 166 such stars deep and only a long run of misses reaches it.
/// Raising it (1e-50 still looks harmless) abandons a hit that dips early -
/// bright detections whose index counterparts are missing - before the later
/// stars can pay for themselves.
pub const BAIL_THRESHOLD: f64 = 1e-100;

/// `ln(BAIL_THRESHOLD)`, precomputed because `f64::ln` is not a `const fn`.
/// This is the value `verify::verify_hit` passes to `odds_scan_inner` as `bail_ln`;
/// the plain-odds form above documents the same threshold.
pub const BAIL_THRESHOLD_LN: f64 = -230.25850929940458;

/// Odds at which a solution would count as worth writing out. Documentation
/// only: this solver prints exactly one solution, the best solved hit of the
/// round that wins, so its print gate is the solve gate below.
pub const ODDS_TO_PRINT: f64 = 1e6;
/// Odds at which a solution would be polished further. Documentation only: a
/// solved hit always goes through `refine::refine_wcs`, however far above the
/// solve gate its odds are.
pub const ODDS_TO_TUNE: f64 = 1e6;
/// Odds at which a solution would be retained for a later stage. Documentation
/// only: inside a round the capture slot keeps the lexicographic-best
/// (log-odds, quad) solved hit (`sweep::better_hit`) with no threshold of its
/// own beyond the solve gate.
pub const ODDS_TO_KEEP: f64 = 1e9;
/// Odds a hit must reach to be a solution: `ln` of this is the gate in
/// `verify::VerifyResult::solved`, and the first round that produces one stops
/// the sweep. Documentation only as a constant: `ODDS_TO_SOLVE_LN` is the value
/// compared.
///
/// Lower it and a wrong quad that banks a few chance pairs can report a bogus
/// WCS; raise it and a hit needs more (or tighter) matches, so a narrow or
/// sparse field may never solve before the depth ramp ends at MAX_OBJS.
pub const ODDS_TO_SOLVE: f64 = 1e9;

/// `ln(ODDS_TO_PRINT)`, precomputed for the same reason as BAIL_THRESHOLD_LN.
/// Documentation only: neither this nor the odds form above is compared
/// against today.
pub const ODDS_TO_PRINT_LN: f64 = 13.815510557964274;
/// `ln(ODDS_TO_SOLVE)`: the gate in nats, the same units the accumulator runs
/// in, so no per-call logarithm is needed at the comparison sites.
pub const ODDS_TO_SOLVE_LN: f64 = 20.72326583694641;

/// Cap on the number of quads tried against one index; 0 means unlimited.
/// Documentation only - nothing reads it. Enumeration here is bounded by the
/// depth ramp's star window (MAX_OBJS) and the `--cpulimit` deadline, never by
/// a quad count, so the only levers that try fewer quads are a shallower round
/// or a tighter scale range.
pub const MAXQUADS: usize = 0;
/// Cap on the number of matched stars a solution may use; 0 means unlimited.
/// Documentation only: `verify::odds_scan_inner` walks the whole test-star list,
/// stopping only at the bail floor or at its early-stop threshold, which
/// `verify::verify_hit` passes as infinity.
pub const MAXMATCHES: usize = 0;

/// Default for `--cpulimit`, seconds. `run_into` turns it into the sweep's
/// deadline (floored at `sweep::MIN_CPULIMIT_S`); past the deadline the sweep
/// prints the timeout line and exits 1 without a solution.
///
/// 300 s is the solve budget the two narrow shipped fields carry in the
/// end-to-end gate, set from their slowest reference run (23.9 s of engine
/// CPU); it is not the 600 s astrometry.net's solve-field defaults to. In the
/// record of those runs the same number was the checker's wall-clock override.
/// Shortening it can cut the depth
/// ramp, or a round in progress - the winner of a truncated round is
/// timing-dependent by design; lengthening it only delays the not-solved
/// answer of a field that will not solve.
pub const CPU_LIMIT_SECS: f64 = 300.0;

/// Smallest quad backbone |AB| worth enumerating, as a fraction of the shorter
/// image side. `Searcher::new` rejects a shorter backbone before computing any
/// of its codes, and `IndexMeta::fits_scale` uses the same product (times the
/// finest allowed scale) as the bottom of the angular quad range an index must
/// overlap to be selected.
///
/// Raise it and the small quads of an index stop being enumerated: at a scale
/// where that index's quads are shorter than the bound nothing can match, and
/// the round is wasted. Lower it and many more (A, B) pairs pass the gate,
/// each opening the C/D loops under it - O(n) code queries for a 3-star index,
/// O(n^2) for a 4-star one.
pub const QSF_LO: f64 = 0.1;
/// Largest quad backbone, as a fraction of the frame diagonal
/// (`hypot(width, height)`). At 1.0 the bound equals the greatest distance two
/// pixels of the frame can have, so the enumeration never prunes a backbone
/// from above; the value's real work is as the top of the angular quad range in
/// `IndexMeta::fits_scale`. Lowering it drops indices whose quads are all
/// larger than that top, i.e. it declares the field too small for them.
pub const QSF_HI: f64 = 1.0;

/// Blind-mode scale range: when neither `--scale-low` nor `--scale-high` is
/// given, `cli::SolveOptions::from_args` reads them as the field width spanning
/// 0.1 to 180 degrees and converts each to arcsec/px by dividing by the frame
/// width.
///
/// Those bounds then drive index selection (`IndexMeta::fits_scale`) and both
/// hit scale gates, so together they decide between "sweep every installed
/// index" and "the index that carries the field's scale was pruned". The 0.1 deg
/// floor is loose enough that blind selection drops nothing (pinned by
/// `sweep::tests::blind_scale_range_keeps_every_installed_index`). Raising the
/// floor forbids fields narrower than MIN_WIDTH_DEG and prunes the fine-scale
/// indices; lowering the 180 deg ceiling forbids fields wider than it and
/// prunes the coarse-scale ones.
pub const MIN_WIDTH_DEG: f64 = 0.1;
pub const MAX_WIDTH_DEG: f64 = 180.0;

/// Nearest-neighbour match radius of the odds scan, in sigmas: each field star
/// takes the nearest still-unused reference star whose squared pixel distance
/// is at most `NN_RADIUS_SIGMA^2 * sigma_i^2` = `25 * sigma_i^2`
/// (`verify::odds_scan_inner`), where `sigma_i^2` is the VERIFY_PIX/INDEX_JITTER
/// sigma2 expanded by the star's distance from the quad backbone's midpoint
/// (`verify::verify_hit`).
///
/// A star found inside it is only a candidate: the match is banked when its
/// Gaussian term beats the distractor term. Widening the radius banks more
/// chance pairs, which inflates log-odds toward the absolute ln(1e9) gate;
/// narrowing it pushes real pairs into the wings, and each miss costs
/// ln(DISTRACTOR_RATIO) nats.
pub const NN_RADIUS_SIGMA: f64 = 5.0;

/// Ceiling on how many of the brightest field stars take part in quad
/// enumeration: `solve_index` clamps its depth to it and `solve_field` stops
/// the depth ramp's rounds there. Verification and refine still see the whole
/// list, so only the enumeration loses the faint stars.
///
/// An emergency brake, and a project decision rather than a value taken from
/// elsewhere. The quads enumerable from n stars grow as n^dimquads, so without
/// a ceiling the deep rounds of a dense list would be unrunnable inside
/// `--cpulimit`. Lower it and a field whose solving quad needs a star fainter
/// than the cap never solves; raise it and the late rounds cost far more than
/// the shipped fields, which solve in the first rounds, ever need.
pub const MAX_OBJS: usize = 150;

/// How many indices one depth round attempts at once (`sweep::solve_field`).
///
/// The round structure and the sweep order are unchanged: consecutive indices
/// of the round are issued together on the rayon pool and the chunk's results
/// are folded in sweep order, so the reported solution is still the first
/// solving index of the round. The lever is the idle pool: a round that walks
/// its indices one at a time keeps all but one worker empty, and the shallow
/// rounds of a small field are mostly index-opening overhead.
///
/// It is also the memory knob, and the reason it is 3 rather than larger:
/// validating an index faults most of its tables in, so the resident set of a
/// round is ~K times that of one index, and the < 200 MB peak-RSS target is what
/// bounds K (the measured MiB series is in `AGENTS.md`, since it depends on the
/// files one machine happens to have). The cost is clean page cache, evictable
/// under pressure - but the target is a target.
pub const INDEX_FANOUT: usize = 3;

/// Target leaf size of a hint-filtered codes tree (`kdtree::KdTree::filtered`):
/// the build stops halving once a leaf would hold fewer records than this, so a
/// query descends about `log2(records / this)` levels and then scans a handful of
/// codes.
///
/// It trades descent against scanned records and changes nothing else: the tree
/// holds the same records whatever the value, so no hit set depends on it. Lower
/// makes the tree deeper (more nodes per query), higher makes each visited leaf
/// scan more codes.
///
/// The halting rule is not the leaf occupancy. A tree's leaf count is a power of
/// two and the build halves only while the smaller half would still hold this
/// many, so a built leaf averages 16 to 31 records (a file whose subset holds
/// fewer than 32 records gets a single leaf holding all of them).
pub const HINT_TREE_LEAF: usize = 16;

/// How many split dimensions one node of a hint-filtered tree tries, widest
/// spread first, before it gives up and hands all its records to the left child
/// (`kdtree::KdTree::filtered`).
///
/// A node whose records share a coordinate in every dimension has no valid split
/// plane at all, so this only bounds the search for one; with `ndim` of 4 or fewer
/// it never binds.
pub const HINT_TREE_MAX_PART: usize = 8;

/// Most records a hint-filtered codes tree is built for
/// (`kdtree::KdTree::filtered`). It is the per-file RSS guard for that tree:
/// unlike an index table it is anonymous memory (`ndim` integers per record plus
/// two `u32` per node), so past this size the tree is not built and the search
/// stays on the index's own tree.
///
/// It is a byte budget and nothing else - counted at the row width
/// [`HINT_TREE_MAX_ROW_BYTES`] states, and the per-file term of the sweep-wide
/// bound [`HINT_TREE_MAX_FILES`] gives. It is not a claim about when a tree stops
/// paying, and there is a cliff at it: a file whose eligible count lands one past
/// the cap pays the full star and quad walk, gets no tree, and searches its own
/// codes tree in every later round, which costs time and no memory. Widening the
/// cap was measured to buy that time back only on a wide hint circle that cannot
/// contain the field, at a twice- to four-fold larger worst case for the same
/// anonymous memory, and to cost a little on a circle that can; the value
/// therefore stays where the memory budget is, not where the last file that could
/// have used a tree is.
pub const HINT_TREE_MAX_RECORDS: usize = 16384;

/// Widest record row (in bytes) a hint-filtered codes tree is built for
/// (`kdtree::KdTree::filtered`): four code dimensions of `u32` data, the widest
/// codes tree the index format reaches (`DIMQUADS` of at most 4 means `ndim` of
/// at most `2 * (4 - 2)`).
///
/// This is the row [`HINT_TREE_MAX_RECORDS`] was counted at. A caller with wider
/// rows would get a tree bigger by the same factor as its row, so `filtered`
/// refuses them instead of letting the record cap silently mean a larger budget.
pub const HINT_TREE_MAX_ROW_BYTES: usize = 4 * 4;

/// How many index files of one hinted sweep may hold a hint-filtered codes tree
/// (`sweep.rs`): the aggregate RSS guard those trees need and the per-file cap
/// cannot give them, because a hint can leave many selected files each with a
/// tree of its own, retained for the rest of the sweep. Together the two caps
/// hold the trees to `HINT_TREE_MAX_FILES * HINT_TREE_MAX_RECORDS` records of
/// anonymous memory - at `ndim = 4` and `u32` codes some 20 MB.
///
/// The budget is spent in sweep order, which settles which files get a tree with
/// no cross-file negotiation: a file past the budget keeps searching the index's
/// own codes tree, which costs time and no memory. A total is deliberately not
/// charged as the files are opened: that would decide the set of tree-holding
/// files by the order the pool workers arrive, and every cross-attempt aggregate
/// of a sweep must be schedule-independent (see `sweep.rs`).
pub const HINT_TREE_MAX_FILES: usize = 64;

/// Bin budget of the input reordering in `augment::uniformize`: the survivors
/// of the line filter are binned on an `nx x ny` grid over their bounding box,
/// with the budget split by the box's aspect ratio so `nx * ny` lands near this
/// (`augment::uniformize_grid`), then emitted round-robin over the bins. The
/// result is a list in which every group of stars is spread over the frame.
///
/// Frozen because the depth ramp reads its rounds out of that order: each round
/// enumerates the next `sweep::OBJ_ROUND_STEP` stars, and the shipped fields
/// verify their true match in an early round precisely because the bins spread
/// each round over the whole field. Changing the budget reorders the list, so
/// the rounds enumerate different quads; 10.0 keeps that order identical to the
/// one `solve-field`'s preprocessing produces, and our CLI has no `--uniformize`.
pub const UNIFORMIZE_BINS: f64 = 10.0;

/// Cut of the dense-line filter in `augment::hist_remove_lines`: a 1-pixel bin
/// of X (or of Y) whose score `k*ln(mean) - mean - k*(k-1)/2` falls below this
/// value is declared part of a source line, and every star in such a bin is
/// dropped before anything else runs. `c` is the bin's star count, `k = c - 1`
/// and `mean` the average of `c - 1` over the bins that hold more than one star.
///
/// The score falls as a bin's pile-up grows past the typical one, so -100 needs
/// an extreme spike: at `mean = 2` a bin of 3 stars scores -1.6 while one of 17
/// scores -111 and gets flagged, which is what keeps a genuinely crowded field
/// intact. Moving the cut toward 0 flags ordinary dense groups and deletes real
/// stars, which then cannot match anything; pushing it far lower stops flagging
/// the pile-ups the filter exists for (many stars sharing one exact
/// coordinate), and those would otherwise flood the early rounds with junk
/// quads. Frozen because our CLI has no `--no-remove-lines`.
pub const REMOVELINES_LOGCUT: f64 = -100.0;

/// Radius, degrees, of the `--ra/--dec` hint circle when `--radius` is omitted
/// (`cli::SolveOptions::from_args`; a non-positive or non-finite radius is
/// rejected there). The same circle gates three things: index selection
/// (`sweep::tile_in_hint` drops sub-region tiles that cannot overlap it), the
/// matched stars (`Searcher::process_hit` rejects a hit with any quad star
/// outside) and the finished solution's center (`sweep::RoundCapture`).
///
/// Too small and a hint that is slightly off - or a field whose center sits
/// near the edge of the circle - yields exit 1 with nothing solved; too large
/// and the gate prunes almost nothing, so a hint loses its point and the sweep
/// costs what a blind one costs.
pub const DEFAULT_RADECS_RADIUS_DEG: f64 = 10.0;

/// The `--cpulimit` deadline is re-read once the GLOBAL count of enumeration
/// iterations crosses a multiple of this (project decision). Sampling the clock
/// per iteration would dominate the inner loop, so `Searcher::should_stop` only
/// looks at the clock on a boundary crossing.
///
/// It sets the overshoot granularity: after the deadline passes, up to one
/// period of counted iterations can still run before the shared flag trips, on
/// top of the work already in flight. Must be an exact multiple of
/// DEADLINE_EVENT_BATCH - `solve_index` debug-asserts that, because the
/// crossing test compares `count / DEADLINE_CHECK_PERIOD` across a single
/// batched increment.
pub const DEADLINE_CHECK_PERIOD: usize = 1024;

/// How many iterations a worker accumulates locally before adding them to the
/// shared counter in one atomic, so the counter's cache line is not hammered
/// once per iteration. Larger means fewer atomics but a coarser count toward
/// the next clock sample, since a worker contributes only every batch. Must
/// divide DEADLINE_CHECK_PERIOD.
pub const DEADLINE_EVENT_BATCH: usize = 64;

/// Radius, pixels, used to rebuild the matched-pair set the refine pass re-fits
/// on (`refine::match_pairs`, called for every solved hit). It is deliberately
/// far tighter than the 5-sigma radius verification matches at, which expands
/// with distance from the quad backbone (NN_RADIUS_SIGMA): `fit_tan_wcs` has no
/// outlier rejection, so a single bad pair tilts the whole similarity fit.
///
/// Looser, and mismatched pairs drag the refit off the raw fit's fixed point -
/// the failure the refine design exists to remove. Tighter, and too few pairs
/// survive for REFINE_MIN_PAIRS, so the accept guard keeps the raw WCS and the
/// printed solution keeps the anchor noise of the 3-star fit.
pub const REFINE_MATCH_PIX: f64 = 3.0;

/// Cap on the match-and-refit loop of `refine::refine_wcs`. The loop exits on
/// its own as soon as the pair set stops changing, so the cap only binds while
/// the set keeps moving; each iteration is one similarity fit plus one pass
/// over the index stars of the frame.
///
/// Lowering it can stop the loop before the pair set settles, leaving the
/// accept guard to judge a fit built on a half-converged set; at 0 the loop
/// never runs and the refine is a no-op that hands back the raw WCS. Raising it
/// costs time and changes the answer only if the pair set oscillates, which the
/// shipped fixtures do not do.
pub const MAX_REFINE_ITERS: usize = 3;

/// Smallest pair set the accept guard lets replace the raw fit
/// (`refine::accept_refined`). A 4-parameter similarity fit on a handful of
/// pairs is under-determined: it can land on a flipped parity, or fit its own
/// pairs well while being wrong everywhere else, which is why the guard also
/// re-checks parity and mean residual rather than trusting the count.
///
/// With `dimquads = 3` the quad's own three anchors are always in the set, so 5
/// pairs means at least two independent matches. Raising it makes the refine
/// opt out more often on sparse fields, whose printed WCS then keeps the raw
/// fit's anchor noise.
pub const REFINE_MIN_PAIRS: usize = 5;

// ---- Pre-verification probe (`verify::verify_probe`) -----------------------
//
// Calibrated on a dump of every code hit of the five shipped fields and of 196
// generated blind fields, with the probe's per-probe distances recorded next to
// the oracle `nmatch`. Outcome, recorded here because the dump itself is not in
// the repository: 0 of the 474 hits that verify as solved are rejected, while
// 94.8% of the shipped fields' hits and 83.3% of the generated ones are. Each
// number below is the value that keeps some solving hit's margin; the
// accompanying risk of any of them is that a false rejection loses a field
// (exit 1), so they move only together with a new calibration.

/// Cap on how many of the brightest field detections the probe projects through
/// a fitted hit and looks up in the index.
///
/// Set by the solving hits, not the garbage: the hit that solves the shipped
/// `platesolve_...` field banks 20 of 91 test stars but only 4 of 16 probe
/// matches at 3 sigma, which is a 6.3e-2 binomial risk of falling under an
/// acceptance count of 2. At 32 it banks 10, and the worst risk over the dumped
/// solving hits is 1.2e-3 while the garbage chance-pass grows only 0.17% ->
/// 0.18%. Every rejected hit pays for its probes - one tree search each on the
/// fallback path, or `PROBE_STARS x collected` distance tests against one ball
/// (see PROBE_BALL_MAX_STARS) - so raising this further buys safety with the
/// time the probe exists to save.
pub const PROBE_STARS: usize = 32;

/// Floor on the detections the probe needs before it will judge a hit at all:
/// below it a good fit's count is dominated by luck. The dumped solving hits go
/// down to 10 probes and still bank 4-10 matches there.
pub const PROBE_MIN_PROBES: usize = 8;

/// Radius of the probe's index lookup, in UNWIDENED sigmas.
///
/// Deliberately not the per-star variance `verify_hit` matches with: that one
/// grows with the distance from the quad backbone, reaching ~10 px on an 800x600
/// frame, so a "few sigma" probe becomes a search over a visible slice of the
/// frame and stops discriminating.
///
/// 3 sigma is where the measured solving-hit counts saturate: at 1 sigma two of
/// the nine shipped-field solving hits fall to 1-2 matches and would be
/// rejected, while 4-5 sigma pushes the density-derived acceptance count to 3-5
/// without the true counts growing, eating the margin back.
pub const PROBE_RADIUS_SIGMA: f64 = 3.0;

/// Smallest number of probe matches that lets a hit through.
///
/// The worst margin over the 474 dumped solving hits is +2 (count 4 against 2)
/// on the all-sky family, +4 on the tile family, so this has no room to go up:
/// at 3 the shipped `platesolve_...` field and one generated field stop solving.
pub const PROBE_MIN_MATCHES: usize = 2;

/// Multiplier of the expected chance-match count in the acceptance rule: a hit
/// passes when its measured count reaches `ceil(PROBE_RANDOM_FACTOR * expected)`
/// (or [`PROBE_MIN_MATCHES`], whichever is larger).
///
/// Keeps the garbage chance-pass share at 0.18% of all shipped-field hits, and
/// is what makes one floor mean different things per index: 2 at 563
/// stars/deg^2 and 2.9 "/px, but 10 for the same tile at 7.8 "/px.
pub const PROBE_RANDOM_FACTOR: f64 = 4.0;

/// Probe abstains - answers "verify it" - when the index holds fewer than this
/// many stars under the frame, where a good fit may have nothing to match.
///
/// This is the value the margin above was measured at (the worst dumped solving
/// hit sits at 15.0 catalog stars in frame). Judging below it was never tested,
/// and abstaining is always safe - it only costs the verification the probe
/// exists to avoid - so the guard sits at the validated value, not one below it.
pub const PROBE_MIN_CAT_STARS: f64 = 12.0;

/// Ceiling on the catalog stars the probe's batched lookup may collect, and on
/// the frame's estimated catalog content for which it is tried at all.
///
/// Pure performance threshold, no effect on the verdict: the ball it collects is
/// a superset of the per-probe lookup balls, so both paths answer the same
/// question. Two quantities are gated on it: the frame's estimated catalog
/// content (`cat_in_frame`, the tile-mean estimate that decides whether the
/// batched path is tried at all), and the number of stars the collection
/// actually holds, which abandons the ball for the per-probe searches once it
/// reaches this ceiling. The batched test costs one distance evaluation per
/// (probe, collected star) pair against the ~31 tree descents it replaces, so it
/// pays while the collected count stays around a hundred.
///
/// One value serves both ceilings on purpose: the entry estimate and the
/// collection abort ask the same question, once of the tile mean and once of the
/// ball itself.
///
/// Lower and more calls fall back to one search per probe (slower, never wrong);
/// higher and the coarse-scale end pays `PROBE_STARS x collected` evaluations on
/// one big ball.
pub const PROBE_BALL_MAX_STARS: usize = 128;
