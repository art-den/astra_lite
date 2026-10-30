//! The index sweep: the object-count depth ramp, index selection by scale
//! range plus the `--ra/--dec` position gate, and `solve_field` - the round
//! loop that drives `solve_index` over chunks of concurrent index attempts and
//! reports the deterministic winner: the lexicographic-best `(log-odds, quad)`
//! solved hit, so the print does not depend on worker scheduling.

use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use crate::cli::SolveOptions;
use crate::healpix;
use crate::index::{HintIndex, Index, IndexMeta};
use crate::params::{INDEX_FANOUT, MAX_OBJS};
use crate::refine::refine_wcs;
use crate::solver::{FieldSpec, Flow, PositionHint, QuadHit, solve_index};
use crate::verify::{FieldPre, ProbeStars, VerifyResult, probe_stars, verify_hit, verify_probe};
use crate::wcs::pixel_center_of_size;
use crate::xyls::Star;

/// A solved field: the values `output::write_solution` prints (center, sizes,
/// rotation, parity), plus the identity of the hit that produced them (never
/// printed). `winner` lets the sweep's tests re-run the index that solved a
/// field instead of hardcoding an index id, so the suite works with any
/// installed index set.
#[derive(Debug, Clone)]
pub struct Solution {
    pub ra: f64,
    pub dec: f64,
    pub width_deg: f64,
    pub height_deg: f64,
    pub orient_deg: f64,
    pub parity_neg: bool,
    // Carried for the sweep's tests (which index/quad/round won); the shipped
    // print path never reads it. `pub(crate)` so re-exporting `Solution` does
    // not drag `Winner` into the public API; the lint is allowed for tests.
    #[allow(dead_code)]
    pub(crate) winner: Winner,
}

/// Which index and quad the sweep reported as solved, the log-odds it won by
/// ([`better_hit`] defines that order), and the depth-round window that produced
/// it (lets tests replay the exact winning round, `testutil::FieldRun`, without
/// re-deriving the ramp). Consumed only by the tests, not the shipped print
/// path (see [`Solution::winner`] for the lint allow).
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Winner {
    pub indexid: i64,
    /// `-1` for an all-sky index; the healpix cell number for a sub-region tile.
    pub healpix: i64,
    pub quad: usize,
    pub log_odds: f64,
    /// The winning round's `[obj_start, max_objs)` object window.
    pub obj_start: usize,
    pub max_objs: usize,
}

/// Floor for `--cpulimit` when computing the deadline: `Instant + 0` would
/// expire on the first check; a sub-millisecond budget is meaningless anyway.
pub(crate) const MIN_CPULIMIT_S: f64 = 0.001;

/// Index selection for the sweep: keep an index when its quad-size band
/// overlaps the one the field can produce (`IndexMeta::fits_scale`) AND - under
/// a `--ra/--dec` hint - when its sky cell passes [`tile_in_hint`].
/// `index::load_metas_in` hands the metas over in sweep order (largest index
/// quad size first) and `filter` preserves it. Shared with
/// `testutil::discover`, which runs the production sweep on installed indices.
pub(crate) fn select_indices(metas: &[IndexMeta], opts: &SolveOptions) -> Vec<IndexMeta> {
    metas
        .iter()
        .filter(|m| m.fits_scale(opts.app_min, opts.app_max, opts.width, opts.height))
        .filter(|m| tile_in_hint(m, opts.radecs))
        .cloned()
        .collect()
}

/// The healpix tile position gate: all-sky is always in range; a tile is tried
/// only when its cell overlaps the `--ra/--dec` hint circle; blind mode tries
/// every scale-fitting tile. A performance filter, never a correctness one
/// (`healpix::tile_overlaps_circle` errs towards keeping).
fn tile_in_hint(meta: &IndexMeta, radecs: Option<PositionHint>) -> bool {
    match (meta.healpix >= 0, radecs) {
        (true, Some(h)) => healpix::tile_overlaps_circle(
            meta.healpix,
            meta.hpnside,
            h.ra_deg,
            h.dec_deg,
            h.radius_deg,
        ),
        _ => true,
    }
}

/// Timeout message for the sweep: prints the effective (floored) limit - the
/// same value `run_into` built the deadline from - not the raw `--cpulimit`
/// argument, so `--cpulimit 0` reads `cpulimit 0.001 s`.
fn timeout_msg(cpulimit: f64) -> String {
    format!(
        "Timeout (cpulimit {} s) reached; field not solved",
        f64::max(cpulimit, MIN_CPULIMIT_S)
    )
}

/// What one hinted sweep knows about each index file it has opened: the quads
/// the hint can reach, and the compact codes tree over them (`index.rs`).
///
/// Keyed by the index file path and NEVER by `INDEXID`: one installed series
/// gives every healpix tile the same id (a duplicate is only a warning in
/// `index::load_metas_in`), so an id-keyed cache would apply one tile's verdict
/// to dozens of others and drop files that do hold hint-reachable quads.
///
/// Tests pin the two counters: a regression that quietly turns the mechanism
/// off is invisible in the prints (searching a file's own codes tree is always
/// correct) and shows only on a bench. Never printed to stdout.
#[derive(Default)]
struct HintCache {
    by_path: Mutex<HashMap<PathBuf, Arc<HintIndex>>>,
    /// Verdicts that came with a compact codes tree, one per file.
    trees_built: AtomicUsize,
    /// Files that left the sweep because their verdict reached no quad.
    files_dropped: AtomicUsize,
}

impl HintCache {
    fn get(&self, path: &Path) -> Option<Arc<HintIndex>> {
        self.by_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .cloned()
    }

    fn put(&self, path: &Path, hint: HintIndex) {
        if hint.codes.is_some() {
            self.trees_built.fetch_add(1, Ordering::Relaxed);
        }
        self.by_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_path_buf(), Arc::new(hint));
    }

    /// Counters of a finished sweep; the run that filled them has joined its
    /// attempts, so no writer is left.
    #[cfg(test)]
    fn trees_built(&self) -> usize {
        self.trees_built.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn files_dropped(&self) -> usize {
        self.files_dropped.load(Ordering::Relaxed)
    }
}

/// Whether an index stays in the sweep under a hint: a cached hint that reaches
/// no quad drops the file; everything else - including a file the cache has not
/// seen - is kept. Only a hint computed from a successfully loaded index enters
/// the cache, so the elimination cannot hide an input error (`Err`, exit 2) at
/// the position of a never-readable file.
fn hint_kept(meta: &IndexMeta, hints: &HintCache) -> bool {
    match hints.get(&meta.path) {
        Some(hint) => hint.any,
        None => true,
    }
}

/// Attempts of a chunk that may keep a compact hint codes tree: this chunk's
/// share of the sweep-wide `params::HINT_TREE_MAX_FILES` budget, spent in sweep
/// order over the round's list.
///
/// Every file gets its verdict in the same round (the one that first fills the
/// cache, where the list is still what `select_indices` handed over), so the
/// budget falls on files by position, not by how the pool schedules the chunk.
/// A file past the budget loses only the tree: its own codes tree answers the
/// same queries and the elimination verdict still applies.
fn hint_tree_slots(chunk_no: usize, fanout: usize) -> usize {
    crate::params::HINT_TREE_MAX_FILES
        .saturating_sub(chunk_no * fanout)
        .min(fanout)
}

/// The entries of `prev` that `kept` has dropped, in `prev` order. Both lists
/// preserve `selected` order, so this is exactly the set newly eliminated by the
/// hint cache; it exists so the verbose line for a dropped index prints once
/// rather than in every later round.
fn newly_dropped<'a>(prev: &[&'a IndexMeta], kept: &[&'a IndexMeta]) -> Vec<&'a IndexMeta> {
    prev.iter()
        .copied()
        .filter(|m| !kept.iter().any(|k| k.path == m.path))
        .collect()
}

/// Step of the depth ramp: sweep every selected index over field stars 1-10,
/// then 11-20, 21-30, ..., stopping at the first round + index that verifies a
/// solved hit; capped at `MAX_OBJS` and the star-list length.
///
/// Shallow-first is what makes the ramp useful: a round enumerates the quads
/// whose newest star falls inside its window, and that count grows with the
/// window top, so a coarse first round (say 1-100) is deep work on the first
/// index tried, while stepping by 10 gives every index its bright stars before
/// any index spends the whole `--cpulimit` on faint ones.
pub(crate) const OBJ_ROUND_STEP: usize = 10;

/// The round's replace rule: the lexicographic-best `(log-odds, quad)` solved
/// hit wins; the quad tie-break keeps even exact float ties deterministic, so
/// the winner never depends on the order in which worker threads report hits.
fn better_hit(candidate: (f64, usize), current: Option<(f64, usize)>) -> bool {
    match current {
        None => true,
        Some(cur) => candidate > cur,
    }
}

/// Loop-local constants of one index round's capture, so [`RoundCapture`]
/// needs neither the `SolveOptions` nor the loop-local `meta`/round window on
/// every `collect` call.
#[derive(Clone, Copy)]
struct CaptureCtx {
    indexid: i64,
    healpix: i64,
    obj_start: usize,
    max_objs: usize,
    width: f64,
    height: f64,
    verbose: bool,
    radecs: Option<PositionHint>,
}

/// The winner capture for one index's round (`solve_field`'s `on_hit`).
///
/// [`collect`](Self::collect) takes an already-verified hit (`verify_hit` stays
/// in the `solve_field` glue, it needs the `&Index`). Refine + recentring run
/// on the worker thread BEFORE the winner lock (CPU work is never serialized
/// across rayon), then the hint gate on the refined center, then the
/// lexicographic compare-and-replace as ONE atomic operation under the slot
/// lock. Always [`Flow::Continue`]: stopping the enumeration from the callback
/// would make the winning attempt's hit set depend on which worker got there
/// first.
///
/// In a chunked sweep ([`Self::with_cancel`]) the winner replace also publishes
/// to the board in the same critical section, so the chunk's later attempts can
/// stop.
///
/// The glue closure is `Sync` and shares one capture across workers, so the
/// mutable state is `Mutex`-guarded; `panic = "abort"` makes poisoning
/// unobservable.
struct RoundCapture<'a> {
    /// The field list: `refine_wcs` matches index stars against it.
    stars: &'a [Star],
    ctx: CaptureCtx,
    /// Running max of `vr.bestlogodds` over EVERY verified hit of the round
    /// (updated before the solved check, so the verbose `best logodds` line is
    /// right even when nothing solves).
    best_logodds: std::sync::Mutex<f64>,
    /// The lexicographic-best `(log-odds, quad)` solved hit captured so far.
    winner: std::sync::Mutex<Option<(f64, usize, Solution)>>,
    /// Chunk cancel board plus this attempt's slot ([`CancelBoard`]); `None` for
    /// a capture that runs alone (a test).
    cancel: Option<(&'a CancelBoard, usize)>,
}

impl<'a> RoundCapture<'a> {
    fn new(stars: &[Star], ctx: CaptureCtx) -> RoundCapture<'_> {
        RoundCapture {
            stars,
            ctx,
            best_logodds: std::sync::Mutex::new(f64::NEG_INFINITY),
            winner: std::sync::Mutex::new(None),
            cancel: None,
        }
    }

    /// Publishes this capture's winner to the later attempts of its chunk (see
    /// [`CancelBoard`]); used by the concurrent sweep only.
    fn with_cancel<'b: 'a>(mut self, board: &'b CancelBoard, slot: usize) -> Self {
        self.cancel = Some((board, slot));
        self
    }

    /// Folds one verified hit into the capture: running `best_logodds` for every
    /// hit, then - only for a solved hit - refine + recentre (off-lock), the
    /// hint gate on the refined center, and the lexicographic replace. Always
    /// `Flow::Continue` (the production callback never stops the round).
    fn collect(&self, index: &Index, hit: &QuadHit, vr: VerifyResult) -> Flow {
        {
            let mut best = self.best_logodds.lock().unwrap();
            if vr.bestlogodds > *best {
                *best = vr.bestlogodds;
            }
        }
        if !vr.solved() {
            return Flow::Continue;
        }
        // Post-process on the worker thread, before any slot lock: a failed
        // refine keeps the raw WCS; then report in the field-center tangent
        // frame.
        let (w, h) = (self.ctx.width, self.ctx.height);
        let refined = refine_wcs(index, hit, self.stars, w, h, self.ctx.verbose).unwrap_or(hit.wcs);
        let report = refined.recentered_at([pixel_center_of_size(w), pixel_center_of_size(h)]);
        let (ra, dec) = (report.crval_deg[0], report.crval_deg[1]);
        if let Some(hint) = self.ctx.radecs {
            // The post-fit half of the hint gate: the finished solution, at
            // its refined center, must lie inside the hint circle as well (the
            // pre-fit half is the matched-star test in `solver.rs`).
            if !hint.contains(ra, dec) {
                if self.ctx.verbose {
                    eprintln!(
                        "[verify] solution at ({ra:.4},{dec:.4}) outside --ra/--dec circle, ignoring"
                    );
                }
                return Flow::Continue;
            }
        }
        let scale_deg = report.arcsec_per_pix() / 3600.0;
        let sol = Solution {
            ra,
            dec,
            width_deg: w * scale_deg,
            height_deg: h * scale_deg,
            orient_deg: report.orientation_deg(),
            parity_neg: report.parity_is_neg(),
            winner: Winner {
                indexid: self.ctx.indexid,
                healpix: self.ctx.healpix,
                quad: hit.quad,
                log_odds: vr.bestlogodds,
                obj_start: self.ctx.obj_start,
                max_objs: self.ctx.max_objs,
            },
        };
        // Poison-safe compare-and-replace under the slot lock: keep the
        // lexicographic-best (log-odds, quad) solved hit of this round.
        let mut slot = self
            .winner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if better_hit(
            (vr.bestlogodds, hit.quad),
            slot.as_ref().map(|(l, q, _)| (*l, *q)),
        ) {
            *slot = Some((vr.bestlogodds, hit.quad, sol));
            // Still under the winner lock: publishing in this same critical section
            // keeps "captured" and "siblings told" from ever disagreeing.
            if let Some((board, own)) = self.cancel {
                board.cancel_after(own);
            }
        }
        Flow::Continue
    }

    /// Post-round take: `(best_logodds, Option<(winner quad, Solution)>)`. The
    /// `[index N] ... SOLVED (quad Q) ... best logodds X` verbose line and the
    /// timeout precedence are rebuilt from exactly this.
    fn into_parts(self) -> (f64, Option<(usize, Solution)>) {
        let best_logodds = *self.best_logodds.lock().unwrap();
        let winner = self
            .winner
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (best_logodds, winner.map(|(_, quad, s)| (quad, s)))
    }
}

/// The cancel flags of one chunk of concurrent index attempts: one per slot,
/// raised when a strictly EARLIER slot of the same round has captured a
/// solution. An earlier attempt is never cancelled - it could still be the one
/// the sweep reports - so cancellation can only ever discard work whose result
/// the fold of [`solve_field`] would drop anyway.
///
/// The board is the sweep-order rule "cancel iff some earlier position solved",
/// written so the hot enumeration test is one relaxed load.
struct CancelBoard {
    flags: Vec<AtomicBool>,
}

impl CancelBoard {
    fn new(n: usize) -> Self {
        CancelBoard {
            flags: (0..n).map(|_| AtomicBool::new(false)).collect(),
        }
    }

    /// The flag of one slot, handed to that attempt's `FieldSpec::cancel`.
    fn flag(&self, slot: usize) -> &AtomicBool {
        &self.flags[slot]
    }

    /// Raises the flags of the slots after `slot` (the chunk is issued in sweep
    /// order, so a slot is a sweep position relative to its siblings).
    fn cancel_after(&self, slot: usize) {
        for flag in &self.flags[slot + 1..] {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Whether an earlier slot has already solved (the check an attempt makes
    /// before it does anything expensive).
    fn cancelled(&self, slot: usize) -> bool {
        self.flags[slot].load(Ordering::Relaxed)
    }
}

/// What one index attempt of a chunk produced, in the terms the fold of
/// [`solve_field`] needs to replay the sequential sweep's precedence exactly.
#[derive(Debug)]
enum Attempt {
    /// The index did not load, with the deadline verdict of the moment it tried
    /// (see [`fold_attempts`]). An error at an earlier sweep position still masks
    /// a solution at a later one (a corrupt index stays exit 2).
    Err(anyhow::Error, bool),
    /// The `--cpulimit` deadline tripped inside the attempt; the captured
    /// solution, if there is one, still outranks the timeout report.
    Timeout(Option<Solution>),
    /// A solved hit was captured and passed the hint gate.
    Solved(Solution),
    /// An earlier slot of the chunk solved, so this attempt stopped early.
    /// Anything it had captured sits at a later position, which the fold never
    /// reaches once it has reported that earlier one.
    Cancelled,
    /// The attempt ran to its end and captured nothing.
    Nothing,
}

/// One index of one depth round: the body the sequential sweep ran inline per
/// index (cancelled? -> load -> enumerate -> capture -> verbose line), returned
/// as a positional [`Attempt`]. A cancelled slot stops before the load, so it
/// yields [`Attempt::Cancelled`] and never opens a file.
///
/// Runs on a pool worker alongside the chunk's other attempts, each with its
/// own index open. Its `on_hit` callback must never spawn rayon work:
/// `verify_hit` holds its thread-local `Scratch` `RefCell` borrow for the whole
/// call, and a rayon join can move the rest of the task to another worker.
#[allow(clippy::too_many_arguments)]
fn attempt_index(
    opts: &SolveOptions,
    stars: &[Star],
    pre: &FieldPre,
    cands: &ProbeStars,
    meta: &IndexMeta,
    board: &CancelBoard,
    slot: usize,
    lo: usize,
    hi: usize,
    deadline: Instant,
    hints: Option<&HintCache>,
    // Whether this attempt may keep a compact hint codes tree, per
    // `hint_tree_slots`; ignored without a hint cache.
    tree_allowed: bool,
) -> Attempt {
    // Cancelled before the file is opened: the sequential sweep would never have
    // reached this index, so neither should its reader (this only narrows when
    // a file the sequential sweep would never open gets read at all). The real
    // `panic = "abort"` safety comes from `Index::load` returning `Err` rather
    // than panicking; the fold discards a later position's `Err` anyway.
    if board.cancelled(slot) {
        return Attempt::Cancelled;
    }
    if opts.verbose {
        eprintln!("[index] trying {}", meta.describe());
    }
    // The deadline verdict is taken HERE, at the point where the sequential loop
    // tested it, and travels with the `Err`: re-testing the clock at fold time
    // would let a sibling that is still enumerating when the deadline expires
    // downgrade a corrupt index's exit 2 to the timeout's exit 1.
    let past_deadline = Instant::now() >= deadline;
    // Loading once per round is still intentional: what the chunk holds open at
    // most is the chunk's `fanout` indices, never the whole selected list.
    let index = match Index::load(&meta.path) {
        Ok(index) => index,
        Err(e) => return Attempt::Err(e, past_deadline),
    };
    // What the hint can reach in this file belongs to `(this file, the hint)`
    // alone, so the first round that reaches the file pays for it and every later
    // round reads it back from the cache. It is computed outside the lock - the
    // pass faults the star and quad tables, and the chunk's attempts must keep
    // doing that concurrently - and it only ever runs on a file that has just
    // loaded, which is what lets a later round drop it without hiding a load error.
    if let (Some(hints), Some(hint)) = (hints, opts.radecs)
        && hints.get(&meta.path).is_none()
    {
        hints.put(
            &meta.path,
            index.hint_index(&hint.precompute(), tree_allowed),
        );
    }
    // The compact codes tree this attempt searches, when the cache has one; held
    // for the whole attempt so the reference in `spec` stays alive. `None` in the
    // first round (no cache yet) and in a blind sweep.
    let hint_index = hints.and_then(|h| h.get(&meta.path));
    let spec = FieldSpec {
        stars,
        width: opts.width,
        height: opts.height,
        app_min: opts.app_min,
        app_max: opts.app_max,
        radecs: opts.radecs,
        hint_codes: hint_index.as_ref().and_then(|h| h.codes.as_ref()),
        max_objs: hi,
        obj_start: lo,
        verbose: opts.verbose,
        deadline: Some(deadline),
        cancel: Some(board.flag(slot)),
    };
    // `solve_index` runs on the rayon pool: the callback is `Sync` and guards its
    // mutable state itself (`RoundCapture`: refine + recentre, the hint gate, the
    // atomic winner replace; `verify_hit` stays in this glue, it needs `&index`).
    let capture = RoundCapture::new(
        stars,
        CaptureCtx {
            indexid: meta.indexid,
            healpix: meta.healpix,
            obj_start: lo,
            max_objs: hi,
            width: opts.width,
            height: opts.height,
            verbose: opts.verbose,
            radecs: opts.radecs,
        },
    )
    .with_cancel(board, slot);
    let stats = solve_index(&index, &spec, &|hit| {
        // Cheap sanity check first: most code hits are fitted garbage whose odds
        // scan would run to the end of the star list for zero matches. The probe is
        // pure in `(index, hit, stars, cands, frame)`, so which hits it drops cannot
        // depend on how the pool scheduled this attempt, and a dropped hit never
        // reaches `RoundCapture::collect`. It can move stderr only - this hit's
        // `[verify]` line is not printed, so `best logodds` covers the hits actually
        // verified - never the `output.rs` block on stdout.
        if !verify_probe(&index, hit, stars, cands, opts.width, opts.height) {
            return Flow::Continue;
        }
        let vr = verify_hit(&index, hit, stars, pre, opts.width, opts.height);
        if opts.verbose {
            eprintln!(
                "[verify] index {} quad {}: logodds {:.3}, {} match, {} ref, {} test{}",
                meta.indexid,
                hit.quad,
                vr.bestlogodds,
                vr.nmatch,
                vr.nr,
                vr.nt,
                if vr.bailed { ", BAILED" } else { "" }
            );
        }
        capture.collect(&index, hit, vr)
    });
    let (best_logodds, winner) = capture.into_parts();
    if opts.verbose {
        eprintln!(
            "[index {}] {}: {} queries, {} code hits, {} verified hits{}",
            meta.indexid,
            match &winner {
                Some((quad, _)) => format!("SOLVED (quad {quad})"),
                None if stats.timed_out => "timeout".to_string(),
                None if stats.cancelled => "cancelled".to_string(),
                None => "no solution".to_string(),
            },
            stats.queries,
            stats.code_hits,
            stats.hits,
            if best_logodds.is_finite() {
                format!(", best logodds {best_logodds:.3}")
            } else {
                String::new()
            }
        );
    }
    let solution = winner.map(|(_, s)| s);
    if stats.timed_out {
        // With parallel workers the deadline can trip while another worker has
        // just captured a solution: the fold reports that solution, not the
        // timeout, and only the fold prints (a sibling that solved must not
        // leave a stray timeout line behind).
        return Attempt::Timeout(solution);
    }
    // A cancel outranks this attempt's own capture: a raised flag means a
    // strictly earlier slot solved, and the fold reports that one first.
    if stats.cancelled {
        return Attempt::Cancelled;
    }
    match solution {
        Some(sol) => Attempt::Solved(sol),
        None => Attempt::Nothing,
    }
}

/// What folding one chunk's positional [`Attempt`]s decided about the sweep.
enum Folded {
    /// Report this solution; the sweep stops here.
    Solved(Solution),
    /// Stop the sweep with no solution; the timeout line was printed.
    Stop,
    /// No position in this chunk terminated the sweep; try the next chunk.
    Continue,
}

/// Folds one chunk's positional attempts into the sweep's next move, replaying
/// the sequential loop's precedence exactly: the FIRST terminating position
/// wins - `Err` (a corrupt index stays exit 2), then `Timeout` (a captured
/// solution still outranks the timeout report), then `Solved`; `Cancelled` and
/// `Nothing` fall through to the next position. It reads no clock: every
/// verdict a position needed was taken when that attempt ran, so the fold of a
/// given chunk is a pure function of the positions.
///
/// The `Err` arm honours the deadline verdict the attempt recorded at its load:
/// the sequential loop tested `--cpulimit` at the top of EVERY index before
/// opening it, so an index past the deadline printed the timeout and exited 1
/// without reaching its (possibly corrupt) file. Hence the timeout line prints
/// through `on_timeout` here while the top-of-chunk check prints directly: the
/// seam lets the precedence rules be tested without a sweep.
fn fold_attempts(
    attempts: Vec<Attempt>,
    cpulimit: f64,
    on_timeout: &mut dyn FnMut(&str),
) -> Result<Folded> {
    for attempt in attempts {
        match attempt {
            Attempt::Err(e, past_deadline) => {
                if past_deadline {
                    on_timeout(&timeout_msg(cpulimit));
                    return Ok(Folded::Stop);
                }
                return Err(e);
            }
            Attempt::Timeout(sol) => {
                if sol.is_none() {
                    on_timeout(&timeout_msg(cpulimit));
                }
                return Ok(match sol {
                    Some(sol) => Folded::Solved(sol),
                    None => Folded::Stop,
                });
            }
            Attempt::Solved(sol) => return Ok(Folded::Solved(sol)),
            Attempt::Cancelled | Attempt::Nothing => {}
        }
    }
    Ok(Folded::Continue)
}

/// The index sweep: rounds of field depth outer; inside a round the selected
/// indices issued in chunks of `fanout` concurrent attempts
/// (`params::INDEX_FANOUT` on the CLI path). `selected` is swept as given, so
/// the caller must hand it over in sweep order ([`select_indices`]: largest
/// index quad size first).
///
/// What is reported is the first round + index (in that order) with a solved
/// hit, and that winning ATTEMPT runs its enumeration to its end: every hit is
/// verified and the reported solution is the lexicographic-best (log-odds,
/// quad) solved hit of its capture, so the printed WCS does not depend on
/// rayon scheduling - unless `--cpulimit` truncates that attempt, which is
/// timing-dependent by design. Attempts at later positions of the winning
/// chunk run until cancelled and later chunks are never issued, so only the
/// winning attempt's hit set is schedule-independent; a cancelled attempt's
/// depends on when the cancel lands, and the fold discards its result.
///
/// Chunking does not change what is reported: the chunk's attempts are folded
/// in sweep order and only the FIRST terminating position is used
/// ([`fold_attempts`] reproduces the sequential loop's precedence); everything
/// later is speculative - cancelled (only strictly later attempts are, and one
/// cancelled after capturing could not have been the reported one) or
/// discarded by the fold. Per-index results are schedule-independent
/// (`solver::tests::hit_multiset_is_schedule_independent`); the winner inside
/// an index is still [`better_hit`].
///
/// Two consequences of chunk issuance: the `--cpulimit` check moves from the
/// top of an attempt to the top of a chunk (still before any index is opened;
/// inside a chunk the deadline is tripped by `solve_index` through
/// `FieldSpec::deadline`), and the chunk's verbose stderr lines interleave,
/// which they may - stderr is not a contract, the `output.rs` block on stdout
/// is. Chunks do NOT cancel across their boundary: a solution in chunk `k`
/// means every earlier chunk found nothing.
pub(crate) fn solve_field(
    opts: &SolveOptions,
    stars: &[Star],
    selected: &[IndexMeta],
    deadline: Instant,
) -> Result<Option<Solution>> {
    solve_field_with_fanout(opts, stars, selected, deadline, INDEX_FANOUT)
}

/// [`solve_field`] with the chunk width given explicitly. The width is a solver
/// tunable and stays a `params` constant on the CLI path; the parameter exists
/// so a test can compare a chunked sweep with `fanout = 1`, the sequential one.
pub(crate) fn solve_field_with_fanout(
    opts: &SolveOptions,
    stars: &[Star],
    selected: &[IndexMeta],
    deadline: Instant,
    fanout: usize,
) -> Result<Option<Solution>> {
    // What a hinted sweep knows per index file; `None` for a blind one, which
    // therefore neither walks the star and quad tables nor filters an index out of
    // its sweep (that sweep stays exactly the list `select_indices` handed over).
    let hints = opts.radecs.map(|_| HintCache::default());
    solve_field_swept(opts, stars, selected, deadline, fanout, hints.as_ref())
}

/// The same sweep over a hint cache the caller owns, so a test can read the
/// activation counters of the run it just made. `hints` is used only under a
/// hint; one cache per run keeps the counters a property of that run.
#[cfg(test)]
fn solve_field_with_fanout_counted(
    opts: &SolveOptions,
    stars: &[Star],
    selected: &[IndexMeta],
    deadline: Instant,
    fanout: usize,
    hints: &HintCache,
) -> Result<Option<Solution>> {
    solve_field_swept(
        opts,
        stars,
        selected,
        deadline,
        fanout,
        opts.radecs.is_some().then_some(hints),
    )
}

fn solve_field_swept(
    opts: &SolveOptions,
    stars: &[Star],
    selected: &[IndexMeta],
    deadline: Instant,
    fanout: usize,
    hints: Option<&HintCache>,
) -> Result<Option<Solution>> {
    let pre = FieldPre::new(stars);
    // The probe's candidate detections depend on the star list only, never on an
    // index or a hit, so they are ordered once per field next to `pre`; the
    // per-hit path only drops that hit's quad stars from the list.
    let cands = probe_stars(stars);
    let depth_cap = usize::min(stars.len(), MAX_OBJS);
    // One worker per concurrent attempt, and never a zero-width chunk. Clamping
    // to the pool makes a narrow machine sweep narrower, which is legal for
    // stdout: the fold reports the same position at any width.
    let fanout = usize::max(usize::min(fanout, rayon::current_num_threads()), 1);
    // The indices in play this round. Round 1 sweeps `selected` as given; an
    // attempt of a later round fills the hint cache, and from then on an index
    // whose hint reaches no quad leaves the list (see [`hint_kept`]). Filtering
    // rebuilds the list from `selected` every round, so the order and the
    // chunk boundaries of what remains are schedule-independent.
    let mut active: Vec<&IndexMeta> = selected.iter().collect();
    let mut lo = 0usize;
    let mut round = 0usize;
    while lo < depth_cap {
        let hi = usize::min(lo + OBJ_ROUND_STEP, depth_cap);
        if opts.verbose {
            eprintln!("[round] field objects {}-{}", lo + 1, hi);
        }
        if round > 0
            && let Some(hints) = &hints
        {
            let kept: Vec<&IndexMeta> = selected.iter().filter(|m| hint_kept(m, hints)).collect();
            // `kept` is a subset of `active` - the rule is the same every round
            // and a dropped file never returns - so the difference is exactly the
            // number of files this round eliminated.
            hints
                .files_dropped
                .fetch_add(active.len() - kept.len(), Ordering::Relaxed);
            if opts.verbose {
                for dropped in newly_dropped(&active, &kept) {
                    eprintln!(
                        "[index] drop {}: no quad has all its stars inside the \n\
                         [index]        --ra/--dec circle, index skipped for this hint",
                        dropped.describe()
                    );
                }
            }
            // A dropped index is never opened again, so a file that turns
            // unreadable after its verdict is cached is no longer an input error
            // at all: the run continues past its position and can solve (exit 0)
            // where reading it would have ended the run with exit 2. Accepted: the
            // drop only happens for a file that loaded and was swept in an earlier
            // round, and a hint that excludes it means its content was already
            // irrelevant.
            active = kept;
        }
        for (chunk_no, chunk) in active.chunks(fanout).enumerate() {
            if Instant::now() >= deadline {
                eprintln!("{}", timeout_msg(opts.cpulimit));
                return Ok(None);
            }
            let board = CancelBoard::new(chunk.len());
            let tree_slots = hint_tree_slots(chunk_no, fanout);
            // Nested rayon scopes are fine: the pool work-steals across the two
            // levels, so the chunk's `K` attempts and the enumeration each of
            // them runs share the same workers.
            let attempts: Vec<Attempt> = chunk
                .par_iter()
                .enumerate()
                .map(|(slot, meta)| {
                    attempt_index(
                        opts,
                        stars,
                        &pre,
                        &cands,
                        meta,
                        &board,
                        slot,
                        lo,
                        hi,
                        deadline,
                        // Walking the star and quad tables is worth its cost only
                        // from the second round on: the first round is the cheap
                        // one, and a field that solves there must not pay for a
                        // pass over tables it never needed. A blind sweep has no
                        // cache at all.
                        if round > 0 { hints } else { None },
                        slot < tree_slots,
                    )
                })
                .collect();
            match fold_attempts(attempts, opts.cpulimit, &mut |m| eprintln!("{m}"))? {
                Folded::Solved(sol) => return Ok(Some(sol)),
                Folded::Stop => return Ok(None),
                Folded::Continue => {}
            }
        }
        lo = hi;
        round += 1;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ScaleUnits, parse_options};
    use crate::coords::deg2arcsec;
    use crate::genfield;
    use crate::index;
    use crate::params::{
        self, CPU_LIMIT_SECS, DEFAULT_RADECS_RADIUS_DEG, MAX_WIDTH_DEG, MIN_WIDTH_DEG,
    };
    use crate::testutil::{FIELD2_TRUTH, NARROW1_TRUTH, NARROW2_TRUTH, PLATESOLVE1_TRUTH};
    use crate::wcs::Wcs;
    use std::path::Path;
    use std::time::Duration;

    /// Invariant: given the frame size and no angular size, the scale half of
    /// index selection keeps every installed index: the blind 0.1-180 deg
    /// field-width range is a 27.2 arcsec to 225.7 deg backbone quad band on
    /// this 4656x3522 frame (`IndexMeta::fits_scale`), so an index would need
    /// its whole quad band outside that span to be dropped. The installed set
    /// is not ours to enumerate, so the assertion is an invariant, never a
    /// count: nothing dropped and the `sweep_cmp` order survives the filter.
    #[test]
    fn blind_scale_range_keeps_every_installed_index() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        // The blind scale range of a 4656x3522 field: no --scale-low/--scale-high.
        let (w, h) = (4656.0, 3522.0);
        let app_min = deg2arcsec(MIN_WIDTH_DEG) / w;
        let app_max = deg2arcsec(MAX_WIDTH_DEG) / w;
        let mut selected: Vec<&IndexMeta> = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        for meta in metas {
            if meta.fits_scale(app_min, app_max, w, h) {
                selected.push(meta);
            } else {
                dropped.push(meta.describe());
            }
        }
        assert_eq!(
            selected.len(),
            metas.len(),
            "the blind range dropped indices: {dropped:?}"
        );
        for pair in selected.windows(2) {
            assert_ne!(
                index::sweep_cmp(pair[0], pair[1]),
                std::cmp::Ordering::Greater,
                "selection broke the sweep order: {} then {}",
                pair[0].describe(),
                pair[1].describe()
            );
        }
    }

    /// A deadline that has already passed must make the sweep report "not
    /// solved" before any index is loaded (the `--cpulimit` deadline check).
    /// Hermetic by design: the metas are header stubs, so no index file is read
    /// at all, and the field file only has to supply a non-empty star list - an
    /// empty one leaves the round loop without ever reaching the deadline check.
    #[test]
    fn solve_field_honors_a_past_deadline() {
        let Some(stars) = crate::testutil::load_field("4656x3522-1.xyls") else {
            return;
        };
        let metas = [
            crate::testutil::meta_stub(4213, -1, 0),
            crate::testutil::meta_stub(4206, 2, 1),
        ];
        let opt = parse_options(&[]).unwrap();
        let solution = solve_field(&opt, &stars, &metas, Instant::now()).unwrap();
        assert!(solution.is_none(), "past deadline must solve nothing");
    }

    // ---- Deterministic round winner -----------------------------------------

    /// The winner of a round is the lexicographic-best `(log-odds, quad)`
    /// solved hit, independent of arrival order; exact float ties resolve to
    /// the larger quad id.
    #[test]
    fn better_hit_is_lexicographic_and_order_independent() {
        // Differing log-odds: the higher one must win in either arrival order.
        let a = (342.720f64, 88080usize);
        let b = (321.500, 69298);
        for order in [[a, b], [b, a]] {
            let mut best = None;
            for c in order {
                if better_hit(c, best) {
                    best = Some(c);
                }
            }
            assert_eq!(best, Some(a), "arrival order {order:?}");
        }
        let t1 = (100.0f64, 5usize);
        let t2 = (100.0, 9);
        for order in [[t1, t2], [t2, t1]] {
            let mut best = None;
            for c in order {
                if better_hit(c, best) {
                    best = Some(c);
                }
            }
            assert_eq!(best, Some(t2), "exact tie, arrival order {order:?}");
        }
    }

    // ---- RoundCapture: the winner slot, exercised directly ------------------

    /// A synthetic TAN WCS whose field-center sky position is near (10, 0) deg.
    /// Its refine outcome is irrelevant to these assertions: the winner slot
    /// keys on `(log-odds, quad)`, and the far-hint gate is far enough that no
    /// similarity tweak can land the center inside it either way.
    fn synthetic_wcs() -> Wcs {
        let scale = 1.0 / 3600.0; // 1 arcsec/px, deg/px
        Wcs {
            crpix: [50.0, 50.0],
            crval_deg: [10.0, 0.0],
            cd: [[-scale, 0.0], [0.0, scale]],
        }
    }

    fn capture_ctx(radecs: Option<PositionHint>) -> CaptureCtx {
        CaptureCtx {
            indexid: 1,
            healpix: -1,
            obj_start: 0,
            max_objs: 10,
            width: 100.0,
            height: 100.0,
            verbose: false,
            radecs,
        }
    }

    /// A hit whose `field_stars` index the 4-star field below and whose `quad`
    /// is a valid record of the loaded index (the caller keeps `quad < nquads`).
    fn synth_hit(quad: usize) -> QuadHit {
        QuadHit {
            quad,
            code_dist2: 0.0,
            field_stars: [0, 1, 2, 0],
            wcs: synthetic_wcs(),
        }
    }

    /// A `VerifyResult` with the given log-odds; `solved()` keys on the log-odds
    /// alone, so the match counts are placeholders here.
    fn vr_with(logodds: f64) -> VerifyResult {
        VerifyResult {
            bestlogodds: logodds,
            nmatch: 10,
            nr: 100,
            nt: 100,
            bailed: false,
        }
    }

    /// The 4-star field every synthetic hit indexes (its `field_stars` values
    /// are all valid indices into it).
    fn synth_field() -> Vec<Star> {
        [(10.0, 10.0), (50.0, 20.0), (80.0, 60.0), (30.0, 90.0)]
            .iter()
            .map(|&(x, y)| Star { x, y, flux: 1.0 })
            .collect()
    }

    /// (a) The capture's winner is the lexicographic-best `(log-odds, quad)`
    /// solved hit independent of `collect` arrival order (the property
    /// `solve_field` relies on). No hint: the winner is the pure argmax.
    #[test]
    fn round_capture_winner_is_order_independent() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        let hi = params::ODDS_TO_SOLVE_LN + 20.0;
        let lo = params::ODDS_TO_SOLVE_LN + 5.0;
        for order in [[(hi, 0usize), (lo, 1usize)], [(lo, 1), (hi, 0)]] {
            let capture = RoundCapture::new(&stars, capture_ctx(None));
            for &(logodds, quad) in &order {
                assert_eq!(
                    capture.collect(&index, &synth_hit(quad), vr_with(logodds)),
                    Flow::Continue
                );
            }
            let (_best, winner) = capture.into_parts();
            let (wquad, sol) = winner.expect("a solved hit must be captured");
            assert_eq!(
                wquad, 0,
                "the higher-log-odds quad must win, arrival order {order:?}"
            );
            assert_eq!(sol.winner.log_odds, hi);
        }
    }

    /// (b) An exact float tie resolves to the larger quad id (the second half
    /// of the lexicographic key), regardless of arrival order.
    #[test]
    fn round_capture_breaks_float_ties_by_quad() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        let tie = params::ODDS_TO_SOLVE_LN + 10.0;
        for order in [[(tie, 0usize), (tie, 1usize)], [(tie, 1), (tie, 0)]] {
            let capture = RoundCapture::new(&stars, capture_ctx(None));
            for &(logodds, quad) in &order {
                assert_eq!(
                    capture.collect(&index, &synth_hit(quad), vr_with(logodds)),
                    Flow::Continue
                );
            }
            let (wquad, _sol) = capture
                .into_parts()
                .1
                .expect("a solved hit must win on an exact tie");
            assert_eq!(
                wquad, 1,
                "an exact log-odds tie resolves to the larger quad, order {order:?}"
            );
        }
    }

    /// (c) A solved hit whose (refined) center falls outside the hint circle
    /// never enters the winner slot and never stops the round; the identical
    /// hits with no hint do get captured, so the drop is the gate's doing.
    #[test]
    fn round_capture_hint_gate_drops_far_solutions() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        // The synthetic center is near (10, 0) deg; a 1 deg circle at (180, -80)
        // cannot contain it whether refine keeps or replaces the WCS.
        let far = Some(PositionHint {
            ra_deg: 180.0,
            dec_deg: -80.0,
            radius_deg: 1.0,
        });
        let hits = [synth_hit(0), synth_hit(1)];
        let capture = RoundCapture::new(&stars, capture_ctx(far));
        for hit in &hits {
            assert_eq!(
                capture.collect(&index, hit, vr_with(params::ODDS_TO_SOLVE_LN + 50.0)),
                Flow::Continue,
                "a gated hit must not stop the round"
            );
        }
        assert!(
            capture.into_parts().1.is_none(),
            "a solution outside the hint circle must never enter the slot"
        );

        // Control: the same solved hits DO enter with no hint.
        let open = RoundCapture::new(&stars, capture_ctx(None));
        for hit in &hits {
            assert_eq!(
                open.collect(&index, hit, vr_with(params::ODDS_TO_SOLVE_LN + 50.0)),
                Flow::Continue
            );
        }
        assert!(
            open.into_parts().1.is_some(),
            "control: the identical solved hits capture with no hint"
        );
    }

    /// (d) `best_logodds` is a running max over EVERY hit, reached before the
    /// solved check: hits below the solve gate update it (and drive the verbose
    /// `best logodds` line) yet never enter the winner slot.
    #[test]
    fn round_capture_tracks_best_logodds_of_unsolved_hits() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        assert!(index.meta.nquads >= 2, "need two quads to compare");
        let stars = synth_field();
        let capture = RoundCapture::new(&stars, capture_ctx(None));
        let a = params::ODDS_TO_SOLVE_LN - 1.0;
        let b = params::ODDS_TO_SOLVE_LN - 50.0;
        for (logodds, quad) in [(b, 0usize), (a, 1usize)] {
            assert_eq!(
                capture.collect(&index, &synth_hit(quad), vr_with(logodds)),
                Flow::Continue
            );
        }
        let (best, winner) = capture.into_parts();
        assert_eq!(best, a, "best_logodds must reflect unsolved hits");
        assert!(
            winner.is_none(),
            "an unsolved hit must not enter the winner slot"
        );
    }

    /// The round reports the lexicographic-best `(log-odds, quad)` solved hit:
    /// the callback always continues, so the round runs to its end and its hit
    /// set does not depend on scheduling. Replaying the round that wins
    /// `4656x3522-2` twice must give the same winner twice. No quad id or
    /// log-odds value is pinned here on purpose: which quad wins is a property
    /// of the installed index set; the rule that picks it is ours.
    #[test]
    fn round_winner_is_the_best_solved_hit_of_the_round() {
        let Some(run) = crate::testutil::field_run(FIELD2, 4656.0, 3522.0, None) else {
            return;
        };
        let reported = (run.log_odds, run.quad);
        // Two replays of the winning round: each must contain the reported
        // winner again (same parameters, so losing it would be a real
        // regression) and pick the same argmax, which is also the reported one.
        let mut argmaxes: Vec<(f64, usize)> = Vec::new();
        for _ in 0..2 {
            let hits = crate::testutil::solved_hits(run);
            assert!(
                !hits.is_empty(),
                "the winning round of {} replayed no solved hit",
                run.field
            );
            let winner = hits
                .iter()
                .find(|h| h.hit.quad == run.quad)
                .unwrap_or_else(|| {
                    panic!(
                        "quad {} is not a solved hit when its own round is replayed",
                        run.quad
                    )
                });
            assert_eq!(
                (winner.verify.bestlogodds, winner.hit.quad),
                reported,
                "the winner is not the same hit when its own round is replayed"
            );
            let mut best: Option<(f64, usize)> = None;
            for h in &hits {
                let candidate = (h.verify.bestlogodds, h.hit.quad);
                if better_hit(candidate, best) {
                    best = Some(candidate);
                }
            }
            argmaxes.push(best.unwrap());
        }
        assert_eq!(
            argmaxes[0], argmaxes[1],
            "two replays of the same round picked different winners"
        );
        assert_eq!(
            reported, argmaxes[0],
            "the sweep did not report the better_hit argmax of its round"
        );
        assert!(
            reported.0 >= params::ODDS_TO_SOLVE_LN,
            "the reported winner is below the solve gate: {reported:?}"
        );
    }

    // ---- End-to-end prints: the sky truth ---------------------------------

    /// Field files of the print gates. Their sky truth lives in
    /// `testutil` (`FIELD2_TRUTH`, `NARROW1_TRUTH`, `NARROW2_TRUTH`);
    /// `testutil::Truth::check` gates the print against it with the budgets
    /// `TOL_CENTER_PX`, `TOL_ORIENT_DEG`, `TOL_SCALE_FRAC`, `TOL_SIZE_FRAC`.
    const FIELD2: &str = "4656x3522-2.xyls";
    const NARROW1: &str = "1920x1080-1.xyls";
    const NARROW2: &str = "1920x1080-2.xyls";
    /// The gate's `platesolve-1`: a wide field whose ~3 deg hint run reaches the
    /// fifth depth round, which is what makes it a leg over the hint structures a
    /// sweep builds from round 2 on. Its frame comes from the `IMAGEW`/`IMAGEH`
    /// keys in the file, and its sky truth is `testutil::PLATESOLVE1_TRUTH`.
    const PLATE1: &str = "platesolve_13932859987089881536.xyls";
    const PLATE1_FRAME: (f64, f64) = (6224.0, 4168.0);

    /// The tight `--radius` of the hint legs, degrees - the same circle the
    /// end-to-end gate's `hint_tight` variant draws around a field's center, and
    /// the radius `scripts/bench.py` uses for its wrong-prior leg.
    const TIGHT_HINT_RADIUS_DEG: f64 = 3.0;

    /// The narrow fields' plate scale, as the `--scale-units arcsecperpix
    /// --scale-low 0.95 --scale-high 1.05` hint these runs are swept with: a
    /// band around the 0.9966 and 0.9971 arcsec/px the two truth entries
    /// record.
    const NARROW_SCALE: (f64, f64) = (0.95, 1.05);

    /// Depth cap of the far-hint legs: 40 stars = 4 rounds of the production
    /// ramp ([`OBJ_ROUND_STEP`]). Uncapped, an unsolvable hint sends the ramp
    /// to `MAX_OBJS` over every all-sky index - a batch-job cost, not a
    /// unit-test one.
    const SHALLOW_OBJS: usize = 40;

    /// Offset of the far-hint legs, in degrees of declination from the field's
    /// published center. Even the widest gated field (3.5 x 2.7 deg) then
    /// has its nearest frame corner 18.7 deg from the hint center: clear of the
    /// default 10 deg `--radius` circle by 8.7 deg, and of the near radius
    /// below by even more.
    const FAR_HINT_OFFSET_DEG: f64 = 20.0;

    /// Near-hint radius for field 2: its farthest frame corner is 2.20 deg from
    /// the field center, so a 2.5 deg circle covers the whole 3.5 x 2.7 deg
    /// field and neither the pre-fit matched-star gate in
    /// `solver.rs::process_hit` nor the post-fit center gate in
    /// `RoundCapture::collect` can drop the solution.
    const NEAR_HINT_RADIUS_DEG: f64 = 2.5;

    /// `SolveOptions` for a field file and frame: `scale` is the
    /// `--scale-units arcsecperpix` low/high pair (`None` is the blind 0.1-180
    /// deg field-width range), `hint` the `--ra/--dec/--radius` position hint.
    /// The file is a `testdata/` entry; [`opts_at`] is the same for a star list
    /// that has no file behind it.
    fn solve_opts(
        field: &str,
        width: f64,
        height: f64,
        scale: Option<(f64, f64)>,
        hint: Option<PositionHint>,
    ) -> SolveOptions {
        opts_at(
            &crate::testutil::testdata_path(field),
            width,
            height,
            scale,
            hint,
        )
    }

    /// `SolveOptions` for a star list read from `file`. `solve_field` never opens
    /// it (the caller hands it the stars), so a run over a list that exists only
    /// in memory can name it whatever it likes.
    fn opts_at(
        file: &Path,
        width: f64,
        height: f64,
        scale: Option<(f64, f64)>,
        hint: Option<PositionHint>,
    ) -> SolveOptions {
        let (app_min, app_max) = match scale {
            Some((lo, hi)) => (lo, hi),
            None => (
                deg2arcsec(MIN_WIDTH_DEG) / width,
                deg2arcsec(MAX_WIDTH_DEG) / width,
            ),
        };
        SolveOptions {
            file: file.to_path_buf(),
            width,
            height,
            app_min,
            app_max,
            radecs: hint,
            cpulimit: CPU_LIMIT_SECS,
            verbose: false,
            units: ScaleUnits::DegWidth,
        }
    }

    /// Wall-clock budget of the end-to-end gates: generous against the
    /// seconds these fields take on an installed set.
    fn test_deadline() -> Instant {
        Instant::now() + Duration::from_secs(240)
    }

    /// Guards the fanout legs that need a pool wider than one chunk: at
    /// `rayon::current_num_threads() < INDEX_FANOUT + 1` the injected fanouts
    /// collapse to the same effective width and the `K = 1` leg is identical to
    /// the chunked one, so the comparison would be vacuous. Follows the
    /// `testutil` skip rule (a loud failure under `ASTROMETRY_REQUIRE_TEST_DATA`).
    fn pool_wider_than_a_chunk() -> bool {
        if rayon::current_num_threads() > usize::max(params::INDEX_FANOUT, 1) {
            return true;
        }
        crate::testutil::skip(&format!(
            "need > {} rayon threads to widen past one chunk (pool has {})",
            params::INDEX_FANOUT,
            rayon::current_num_threads()
        ));
        false
    }

    /// A `Solution` with a distinctive `ra` for the [`fold_attempts`] unit tests,
    /// which check only identity (which `Attempt` the fold reports), never the
    /// sky values.
    fn fold_solution(tag: f64) -> Solution {
        Solution {
            ra: tag,
            dec: -tag,
            width_deg: tag,
            height_deg: tag,
            orient_deg: tag,
            parity_neg: false,
            winner: Winner {
                indexid: tag as i64,
                healpix: -1,
                quad: tag as usize,
                log_odds: 1000.0,
                obj_start: 0,
                max_objs: 10,
            },
        }
    }

    /// A `--ra/--dec` circle that does not contain the field must report
    /// nothing. The control leg (no hint) sweeps the installed set and must
    /// solve, so the far leg's `None` proves something; it then runs on
    /// exactly the index the control won - same index, same depth, only the
    /// hint moved - so the gate is what is under test, at the cost of one
    /// index. Both legs are depth-capped (see [`SHALLOW_OBJS`]): uncapped, an
    /// unsolvable hint sends the ramp to `MAX_OBJS` over every all-sky index -
    /// a batch-job cost, not a unit-test one.
    fn far_hint_reports_nothing(
        metas: &[IndexMeta],
        stars: &[crate::xyls::Star],
        field: &Path,
        width: f64,
        height: f64,
        scale: Option<(f64, f64)>,
        center: (f64, f64),
    ) {
        let shallow = &stars[..usize::min(stars.len(), SHALLOW_OBJS)];
        let control = solve_field(
            &opts_at(field, width, height, scale, None),
            shallow,
            metas,
            test_deadline(),
        )
        .unwrap();
        let Some(control) = control else {
            panic!(
                "{} must still solve at depth {} without a hint, or the far-hint \
                 `None` below proves nothing",
                field.display(),
                shallow.len()
            );
        };
        // `FAR_HINT_OFFSET_DEG` of declination away with the default radius:
        // the field lies clear of the circle.
        let far = PositionHint {
            ra_deg: center.0,
            dec_deg: center.1 + FAR_HINT_OFFSET_DEG,
            radius_deg: DEFAULT_RADECS_RADIUS_DEG,
        };
        let won: Vec<IndexMeta> = metas
            .iter()
            .filter(|m| m.indexid == control.winner.indexid && m.healpix == control.winner.healpix)
            .cloned()
            .collect();
        assert!(
            !won.is_empty(),
            "the index the control leg won is not in the sweep set: {:?}",
            control.winner
        );
        let sol = solve_field(
            &opts_at(field, width, height, scale, Some(far)),
            shallow,
            &won,
            test_deadline(),
        )
        .unwrap();
        assert!(
            sol.is_none(),
            "index {} solved {} under a hint at {far:?}: winner {:?}",
            control.winner.indexid,
            field.display(),
            sol.map(|s| s.winner)
        );
    }

    /// End-to-end gate, anchored on the sky: the blind production sweep over
    /// the whole installed set must print field 2's published center/size/
    /// rotation/parity within `testutil::Truth::check`'s budgets. Which index
    /// wins is whatever the installed set offers - the print is a property of
    /// the sky, not of one index file: no index id, quad id or per-index fixed
    /// point is pinned here.
    #[test]
    fn blind_field2_print_matches_sky_truth() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(FIELD2) else {
            return;
        };
        let opts = solve_opts(FIELD2, 4656.0, 3522.0, None, None);
        let sol = solve_field(&opts, &stars, metas, test_deadline())
            .unwrap()
            .unwrap_or_else(|| panic!("{FIELD2} must solve against the installed set"));
        FIELD2_TRUTH.check(&sol, 4656.0);
        assert!(
            sol.winner.log_odds >= params::ODDS_TO_SOLVE_LN,
            "the reported winner is below the solve gate: {:?}",
            sol.winner
        );
    }

    /// The hint gate end to end on field 2, with the hint built at runtime from
    /// the field's published center: a circle around it ([`NEAR_HINT_RADIUS_DEG`],
    /// covering the whole field) still reports the solution; a circle
    /// [`FAR_HINT_OFFSET_DEG`] deg away reports nothing.
    #[test]
    fn hint_on_the_true_center_reports_and_a_far_hint_does_not() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(FIELD2) else {
            return;
        };
        let center = (FIELD2_TRUTH.ra_deg, FIELD2_TRUTH.dec_deg);
        let near = Some(PositionHint {
            ra_deg: center.0,
            dec_deg: center.1,
            radius_deg: NEAR_HINT_RADIUS_DEG,
        });
        let sol = solve_field(
            &solve_opts(FIELD2, 4656.0, 3522.0, None, near),
            &stars,
            metas,
            test_deadline(),
        )
        .unwrap()
        .expect("a hint at the true center must keep the solution (final gate)");
        FIELD2_TRUTH.check(&sol, 4656.0);

        far_hint_reports_nothing(
            metas,
            &stars,
            &crate::testutil::testdata_path(FIELD2),
            4656.0,
            3522.0,
            None,
            center,
        );
    }

    // ---- Chunked concurrent index attempts ---------------------------------

    /// The chunk fold has to reproduce the sequential sweep's answer bit for
    /// bit, so the comparison is on raw bits: a float tolerance would happily
    /// accept a different winning hit.
    fn solution_bits(
        s: &Solution,
    ) -> (
        u64,
        u64,
        u64,
        u64,
        u64,
        bool,
        i64,
        i64,
        usize,
        u64,
        usize,
        usize,
    ) {
        (
            s.ra.to_bits(),
            s.dec.to_bits(),
            s.width_deg.to_bits(),
            s.height_deg.to_bits(),
            s.orient_deg.to_bits(),
            s.parity_neg,
            s.winner.indexid,
            s.winner.healpix,
            s.winner.quad,
            s.winner.log_odds.to_bits(),
            s.winner.obj_start,
            s.winner.max_objs,
        )
    }

    /// `K = 1` is the sequential sweep; every wider chunking must report the
    /// same solution. The `K = 1` leg comes from the injected chunk width,
    /// never a one-thread pool, so what is compared is the issue order and the
    /// fold, not the parallelism inside one index (both enumerate on the pool).
    #[test]
    fn fanout_matches_sequential_output() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        if !pool_wider_than_a_chunk() {
            return;
        }
        // `4656x3522-2`'s winning index sits where a chunk boundary can move
        // it - the case `-1` does not cover.
        for field in ["4656x3522-1.xyls", "4656x3522-2.xyls"] {
            let Some(stars) = crate::testutil::load_field(field) else {
                return;
            };
            let opts = solve_opts(field, 4656.0, 3522.0, None, None);
            let mut want: Option<Solution> = None;
            // Around the shipped constant in both directions, so a boundary on
            // the winning index is covered on either side of it.
            for fanout in [1usize, 2, params::INDEX_FANOUT, params::INDEX_FANOUT + 1] {
                let sol = solve_field_with_fanout(&opts, &stars, metas, test_deadline(), fanout)
                    .unwrap()
                    .unwrap_or_else(|| panic!("{field} must solve at fanout {fanout}"));
                match &want {
                    None => want = Some(sol),
                    Some(seq) => assert_eq!(
                        solution_bits(seq),
                        solution_bits(&sol),
                        "{field}: fanout {fanout} reported a different answer than the sequential sweep"
                    ),
                }
            }
            let want = want.expect("the sequential leg ran");
            assert!(
                want.winner.log_odds >= params::ODDS_TO_SOLVE_LN,
                "the shared winner is below the solve gate: {:?}",
                want.winner
            );
        }
    }

    /// The hinted sweep obeys the same rule as the blind one: the same
    /// solution bits at every chunk width, and the published sky truth.
    ///
    /// It runs on [`PLATE1`] under a [`TIGHT_HINT_RADIUS_DEG`] circle at the
    /// field's center because that reaches the fifth depth round: compact hint
    /// trees are built only from round 2 on, so a field that solves in round 1
    /// would exercise nothing the blind legs above do not. The print is NOT
    /// compared against a blind run (a hint-narrowed sweep may reach the field
    /// through another winner), only against the `fanout = 1` leg and the sky.
    ///
    /// The chunk width also decides which files get a tree, because
    /// [`hint_tree_slots`] spends the budget by position: the legs differ in
    /// which records a query walks and in the order equal-distance hits arrive,
    /// yet must agree on the winner - including the `quad`, which under a tree
    /// is the product of the record-to-quad remap of
    /// `solver.rs::Searcher::query`. `trees_built > 0` says the leg really
    /// searched a filtered tree; without it the equality could be won by a
    /// sweep where the mechanism never fired.
    #[test]
    fn hinted_fanout_matches_sequential_output() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        if !pool_wider_than_a_chunk() {
            return;
        }
        let Some(stars) = crate::testutil::load_field(PLATE1) else {
            return;
        };
        let hint = PositionHint {
            ra_deg: PLATESOLVE1_TRUTH.ra_deg,
            dec_deg: PLATESOLVE1_TRUTH.dec_deg,
            radius_deg: TIGHT_HINT_RADIUS_DEG,
        };
        let opts = solve_opts(PLATE1, PLATE1_FRAME.0, PLATE1_FRAME.1, None, Some(hint));
        let mut want: Option<Solution> = None;
        for fanout in [1usize, 2, params::INDEX_FANOUT] {
            // One cache per leg, so its counters count that run alone.
            let hints = HintCache::default();
            let sol = solve_field_with_fanout_counted(
                &opts,
                &stars,
                metas,
                test_deadline(),
                fanout,
                &hints,
            )
            .unwrap()
            .unwrap_or_else(|| {
                panic!("{PLATE1} must solve at fanout {fanout} under the hint {hint:?}")
            });
            assert!(
                hints.trees_built() > 0,
                "fanout {fanout}: the hinted sweep built no compact codes tree, so it \
                 never searched the hint-eligible records"
            );
            PLATESOLVE1_TRUTH.check(&sol, PLATE1_FRAME.0);
            match &want {
                None => want = Some(sol),
                Some(seq) => assert_eq!(
                    solution_bits(seq),
                    solution_bits(&sol),
                    "{PLATE1}: fanout {fanout} reported a different answer than the sequential sweep"
                ),
            }
        }
        let want = want.expect("the sequential leg ran");
        assert!(
            want.winner.log_odds >= params::ODDS_TO_SOLVE_LN,
            "the shared winner is below the solve gate: {:?}",
            want.winner
        );
    }

    /// A wrong hint must leave nothing to report, and report it through the
    /// new mechanism: the counters are the only witness of the second half,
    /// since a file searched through its own codes tree answers the same
    /// queries as through a filtered tree, so a sweep with the mechanism off
    /// prints exactly the same "not solved".
    ///
    /// [`NARROW1`] is the smallest bench field and its antipode the farthest
    /// circle from it: the sweep walks every round with the field outside the
    /// circle, files whose quads are nowhere near it must leave the list, and
    /// those the circle reaches must be searched through their compact trees.
    #[test]
    fn wrong_hint_drops_files_and_builds_trees() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(NARROW1) else {
            return;
        };
        // The antipode of the field's published center, drawn the way
        // `scripts/bench.py` draws its wrong-prior leg.
        let away = PositionHint {
            ra_deg: (NARROW1_TRUTH.ra_deg + 180.0) % 360.0,
            dec_deg: -NARROW1_TRUTH.dec_deg,
            radius_deg: TIGHT_HINT_RADIUS_DEG,
        };
        let opts = solve_opts(NARROW1, 1920.0, 1080.0, None, Some(away));
        let hints = HintCache::default();
        let sol = solve_field_with_fanout_counted(
            &opts,
            &stars,
            metas,
            test_deadline(),
            params::INDEX_FANOUT,
            &hints,
        )
        .unwrap();
        assert!(
            sol.is_none(),
            "a hint at the antipode of {NARROW1} solved it: {:?}",
            sol.map(|s| s.winner)
        );
        assert!(
            hints.files_dropped() > 0,
            "no index file left the sweep, so the elimination is not running"
        );
        assert!(
            hints.trees_built() > 0,
            "no index file got a compact hint codes tree, so the filtered search is \
             not running"
        );
    }

    /// The same field solved three times over a chunked sweep must render the
    /// identical stdout block, identical to the sequential sweep's: the print
    /// is a property of the field, not of issue order or worker scheduling.
    #[test]
    fn fanout_stdout_is_reproducible() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        if !pool_wider_than_a_chunk() {
            return;
        }
        let field = "4656x3522-1.xyls";
        let Some(stars) = crate::testutil::load_field(field) else {
            return;
        };
        let opts = solve_opts(field, 4656.0, 3522.0, None, None);
        let render = |fanout: usize| {
            let sol = solve_field_with_fanout(&opts, &stars, metas, test_deadline(), fanout)
                .unwrap()
                .unwrap_or_else(|| panic!("{field} must solve at fanout {fanout}"));
            let mut out = Vec::new();
            crate::output::write_solution(&sol, &mut out).expect("render the success block");
            String::from_utf8(out).expect("the success block is utf-8")
        };
        let seq = render(1);
        for leg in 0..3 {
            assert_eq!(
                render(params::INDEX_FANOUT),
                seq,
                "chunked run {leg} printed something else than the sequential sweep"
            );
        }
    }

    /// A chunk in which one index fails to load still fails the run when the
    /// failing position is the earlier one, and still solves when it is the
    /// later one: the fold takes the FIRST terminating position in sweep order,
    /// so an error keeps masking a later solution (a corrupt index is exit 2)
    /// and a solution is never spoiled by a later unreadable file. Both legs
    /// also run at `K = 1`, where the sequential loop gives the same answer.
    #[test]
    fn fanout_error_at_an_earlier_position_beats_a_later_solution() {
        let Some(run) = crate::testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        // A truncated copy of a real index file: the header parses, the tables
        // are not there, so `Index::load` returns a clean `Err`.
        let whole = std::fs::read(&run.meta.path).expect("read the winning index file");
        let path =
            std::env::temp_dir().join(format!("ansolve_sweep_trunc_{}.fits", std::process::id()));
        let prefix = usize::min(whole.len() / 4, 64 * 1024);
        std::fs::write(&path, &whole[..prefix]).expect("write the truncated index");
        assert!(
            Index::load(&path).is_err(),
            "the fixture must be a file `Index::load` rejects: {}",
            path.display()
        );
        let mut broken = crate::testutil::meta_stub(9999, -1, 0);
        broken.path = path.clone();

        let opts = solve_opts("4656x3522-1.xyls", 4656.0, 3522.0, None, None);
        // Both entries in one chunk at the widest leg, in different orders.
        for fanout in [1usize, params::INDEX_FANOUT] {
            let corrupt_first = [broken.clone(), run.meta.clone()];
            let err =
                solve_field_with_fanout(&opts, &run.stars, &corrupt_first, test_deadline(), fanout)
                    .expect_err("a corrupt index at the earlier position must fail the run");
            assert!(
                err.to_string().contains(&path.display().to_string()),
                "the error does not name the unreadable file: {err:#}"
            );
            let winner_first = [run.meta.clone(), broken.clone()];
            let sol =
                solve_field_with_fanout(&opts, &run.stars, &winner_first, test_deadline(), fanout)
                    .expect("a corrupt index at the later position must not fail the run")
                    .expect("the winning index must still solve the field");
            assert_eq!(
                (sol.winner.indexid, sol.winner.healpix),
                (run.meta.indexid, run.meta.healpix),
                "the run reported a different winner than the fixture's"
            );
        }
        std::fs::remove_file(&path).ok();
    }

    // ---- fold_attempts: the sequential precedence, exercised directly --------

    /// While the attempt tried to load before the deadline, an `Err` at an
    /// earlier position masks a later solution (a corrupt index is exit 2), and a
    /// solution at an earlier position beats a later `Err`.
    #[test]
    fn fold_precedence_err_beats_later_solution_while_alive() {
        let mut printed = Vec::new();
        let out = match fold_attempts(
            vec![
                Attempt::Err(anyhow::anyhow!("corrupt"), false),
                Attempt::Solved(fold_solution(7.0)),
            ],
            10.0,
            &mut |m| printed.push(m.to_string()),
        ) {
            Err(e) => e,
            Ok(_) => panic!("an Err the deadline had not overtaken must surface"),
        };
        assert!(out.to_string().contains("corrupt"));
        assert!(
            printed.is_empty(),
            "an Err must not print a timeout: {printed:?}"
        );

        let mut printed = Vec::new();
        let folded = fold_attempts(
            vec![
                Attempt::Solved(fold_solution(7.0)),
                Attempt::Err(anyhow::anyhow!("corrupt"), false),
            ],
            10.0,
            &mut |m| printed.push(m.to_string()),
        )
        .unwrap();
        assert!(
            matches!(folded, Folded::Solved(s) if f64::abs(s.ra - 7.0) < 1e-9),
            "an earlier solution must win"
        );
        assert!(printed.is_empty(), "a solution must not print a timeout");
    }

    /// C1 regression: an attempt already past the deadline when it opened its
    /// file reports the timeout (exit 1), not its `Err` (exit 2) - the
    /// sequential loop would have hit the deadline before ever opening the
    /// corrupt file. The verdict travels with the attempt, so a sibling still
    /// running at the deadline cannot downgrade a live `Err` (above) to this.
    #[test]
    fn fold_reports_timeout_not_a_late_err_once_past_the_deadline() {
        let mut printed = Vec::new();
        let folded = fold_attempts(
            vec![
                Attempt::Nothing,
                Attempt::Err(anyhow::anyhow!("corrupt"), true),
            ],
            10.0,
            &mut |m| printed.push(m.to_string()),
        )
        .expect("a past-deadline Err is a timeout, never a hard error");
        assert!(
            matches!(folded, Folded::Stop),
            "a past-deadline Err must Stop"
        );
        assert_eq!(
            printed.len(),
            1,
            "the timeout must print exactly once: {printed:?}"
        );
    }

    /// T2: a timeout at an earlier position ends the sweep before a later
    /// solution and prints the timeout line exactly once (the fold never
    /// reaches the later position). A timeout that carried a solution reports
    /// it without printing.
    #[test]
    fn fold_timeout_beats_a_later_solution_and_prints_once() {
        let mut printed = Vec::new();
        let folded = fold_attempts(
            vec![Attempt::Timeout(None), Attempt::Solved(fold_solution(9.0))],
            10.0,
            &mut |m| printed.push(m.to_string()),
        )
        .unwrap();
        assert!(
            matches!(folded, Folded::Stop),
            "an earlier timeout must stop the sweep"
        );
        assert_eq!(
            printed.len(),
            1,
            "the timeout must print exactly once: {printed:?}"
        );

        let mut printed = Vec::new();
        let folded = fold_attempts(
            vec![Attempt::Timeout(Some(fold_solution(9.0))), Attempt::Nothing],
            10.0,
            &mut |m| printed.push(m.to_string()),
        )
        .unwrap();
        assert!(
            matches!(folded, Folded::Solved(s) if f64::abs(s.ra - 9.0) < 1e-9),
            "a captured solution outranks the timeout report"
        );
        assert!(
            printed.is_empty(),
            "a captured solution must not print a timeout: {printed:?}"
        );
    }

    /// A chunk whose every position is `Cancelled`/`Nothing` continues to the
    /// next chunk rather than stopping the sweep.
    #[test]
    fn fold_all_nothing_continues() {
        let folded = fold_attempts(
            vec![Attempt::Cancelled, Attempt::Nothing, Attempt::Cancelled],
            10.0,
            &mut |_| {},
        )
        .unwrap();
        assert!(
            matches!(folded, Folded::Continue),
            "no terminator must Continue"
        );
    }

    // ---- cancellation: the three rules the chunked sweep leans on ------------

    /// The board rule itself: a slot tells only the slots AFTER it, never itself
    /// and never an earlier one (an earlier sibling could still be the position
    /// the fold reports, and a winner that cancelled itself would be discarded as
    /// `Attempt::Cancelled`). Telling after the last slot is a no-op, not a
    /// slice out of `flags`.
    #[test]
    fn cancel_board_raises_only_strictly_later_slots() {
        let board = CancelBoard::new(4);
        for slot in 0..4 {
            assert!(!board.cancelled(slot), "a fresh board cancels nothing");
        }
        board.cancel_after(1);
        assert!(!board.cancelled(0), "an earlier slot is never cancelled");
        assert!(
            !board.cancelled(1),
            "the capturing slot must not cancel itself"
        );
        assert!(board.cancelled(2));
        assert!(board.cancelled(3));
        board.cancel_after(3);
        assert!(board.cancelled(3), "the last slot tells nobody");
    }

    /// The publish side of the same rule: only a captured winner raises the later
    /// slots, and it does so inside the winner lock, while an unsolved hit and an
    /// out-of-hint solution leave the chunk alone.
    #[test]
    fn capturing_a_winner_cancels_the_later_slots_of_its_chunk() {
        let Some(meta) = crate::testutil::smallest_index(0) else {
            return;
        };
        let index = crate::testutil::load(meta);
        let stars = synth_field();
        let board = CancelBoard::new(3);
        let capture = RoundCapture::new(&stars, capture_ctx(None)).with_cancel(&board, 1);
        assert_eq!(
            capture.collect(
                &index,
                &synth_hit(0),
                vr_with(params::ODDS_TO_SOLVE_LN - 1.0),
            ),
            Flow::Continue,
            "the production callback never stops the enumeration"
        );
        assert!(
            !board.cancelled(2),
            "an unsolved hit must not cancel the chunk"
        );
        assert_eq!(
            capture.collect(
                &index,
                &synth_hit(0),
                vr_with(params::ODDS_TO_SOLVE_LN + 5.0),
            ),
            Flow::Continue,
            "even the capturing hit must not stop the enumeration"
        );
        assert!(
            board.cancelled(2),
            "a captured winner must cancel the later slots"
        );
        assert!(
            !board.cancelled(0),
            "a captured winner must leave the earlier slots running"
        );
    }

    /// The pre-load short circuit, which is what keeps a speculative attempt from
    /// ever reading a file the sequential sweep would not open (with
    /// `panic = "abort"`, reading is the thing that must not happen). A path that
    /// does not exist makes "it did not load" observable: without the check the
    /// attempt would report `Err`, not `Cancelled`.
    #[test]
    fn a_cancelled_attempt_never_opens_its_index_file() {
        let mut meta = crate::testutil::meta_stub(9999, -1, 0);
        meta.path = std::env::temp_dir().join("ansolve_sweep_must_not_be_opened.fits");
        std::fs::remove_file(&meta.path).ok();
        let opts = solve_opts("none.xyls", 100.0, 100.0, None, None);
        let stars = synth_field();
        let pre = FieldPre::new(&stars);
        let cands = probe_stars(&stars);

        let board = CancelBoard::new(2);
        let attempt = attempt_index(
            &opts,
            &stars,
            &pre,
            &cands,
            &meta,
            &board,
            1,
            0,
            10,
            test_deadline(),
            // No hint, so no hint cache: the attempt must still reach its load.
            // A tree budget of `false` is inert without one.
            None,
            false,
        );
        assert!(
            matches!(attempt, Attempt::Err(..)),
            "an uncancelled attempt must reach the load (and fail on a missing file): {attempt:?}"
        );

        let board = CancelBoard::new(2);
        board.cancel_after(0);
        let attempt = attempt_index(
            &opts,
            &stars,
            &pre,
            &cands,
            &meta,
            &board,
            1,
            0,
            10,
            test_deadline(),
            None,
            false,
        );
        assert!(
            matches!(attempt, Attempt::Cancelled),
            "a cancelled attempt must stop before loading, not report a load error: {attempt:?}"
        );
    }

    // ---- Healpix tile selection: the hint gate ------------------------------

    /// All-sky metas are never gated; a tile is tried blind, kept when its
    /// cell overlaps the hint circle, pruned by a far hint. The pixel numbers
    /// are the xy scheme (`healpix.rs`): cell 2 of nside 1 - the north cap
    /// centered at RA 225 deg, dec +41.8 deg - contains both narrow field
    /// centers, and `near` is `1920x1080-1`'s published center.
    #[test]
    fn tile_in_hint_gates_tiles_not_allsky() {
        let allsky = crate::testutil::meta_stub(4213, -1, 0);
        let tile = crate::testutil::meta_stub(4206, 2, 1);
        let far = Some(PositionHint {
            ra_deg: 20.0,
            dec_deg: -30.0,
            radius_deg: 10.0,
        });
        let near = Some(PositionHint {
            ra_deg: 202.484,
            dec_deg: 47.232,
            radius_deg: 10.0,
        });
        assert!(tile_in_hint(&allsky, far), "all-sky is always in range");
        assert!(tile_in_hint(&allsky, near));
        assert!(tile_in_hint(&tile, None), "blind mode tries every tile");
        assert!(tile_in_hint(&tile, near), "the tile containing the hint");
        assert!(!tile_in_hint(&tile, far), "a far hint prunes the tile");
    }

    // ---- Hint knowledge: eliminating indices with no reachable quad ---------

    /// The cache decides per index FILE: tiles of one installed series share
    /// their `INDEXID`, so an id-keyed cache would let the verdict of one tile
    /// drop dozens of others. A file the cache has never seen is always kept -
    /// its hint only exists once the file has loaded, which is what keeps a file
    /// that cannot load on its `Err` (exit 2) path.
    #[test]
    fn hint_cache_is_keyed_by_path_not_indexid() {
        // Two healpix tiles of one installed series: the same `INDEXID`, one file
        // each (which is why the paths are spelled out here rather than taken from
        // `meta_stub`, whose stub path is named after the id).
        let mut with_quads = crate::testutil::meta_stub(4206, 8, 2);
        with_quads.path = std::path::PathBuf::from("index-4206-8.fits");
        let mut empty = crate::testutil::meta_stub(4206, 9, 2);
        empty.path = std::path::PathBuf::from("index-4206-9.fits");
        assert_eq!(
            with_quads.indexid, empty.indexid,
            "the stubs must be two tiles of one id"
        );
        let hints = HintCache::default();
        hints.put(&empty.path, HintIndex::for_test(false));
        hints.put(&with_quads.path, HintIndex::for_test(true));
        assert!(
            hint_kept(&with_quads, &hints),
            "a hint with a reachable quad keeps its index"
        );
        assert!(
            !hint_kept(&empty, &hints),
            "a hint with no reachable quad drops only its own file"
        );
        let mut never_loaded = crate::testutil::meta_stub(4206, 10, 2);
        never_loaded.path = std::env::temp_dir().join("ansolve_sweep_never_loaded.fits");
        std::fs::remove_file(&never_loaded.path).ok();
        assert!(
            hint_kept(&never_loaded, &hints),
            "an index with no cached hint is still opened, so a corrupt file is still exit 2"
        );
    }

    /// The list the next round sweeps keeps `selected` order, and a dropped index
    /// is reported once, not in every later round.
    #[test]
    fn hint_filtering_preserves_order_and_reports_a_drop_once() {
        let metas: Vec<IndexMeta> = (0..4)
            .map(|i| crate::testutil::meta_stub(4200 + i, -1, 0))
            .collect();
        let hints = HintCache::default();
        hints.put(&metas[1].path, HintIndex::for_test(false));
        let active: Vec<&IndexMeta> = metas.iter().collect();
        let kept: Vec<&IndexMeta> = metas.iter().filter(|m| hint_kept(m, &hints)).collect();
        assert_eq!(
            kept.iter().map(|m| m.indexid).collect::<Vec<_>>(),
            vec![4200, 4202, 4203],
            "filtering must keep the sweep order"
        );
        assert_eq!(
            newly_dropped(&active, &kept)
                .iter()
                .map(|m| m.indexid)
                .collect::<Vec<_>>(),
            vec![4201]
        );
        // The next round sees the same cache, so nothing is newly dropped again.
        assert!(newly_dropped(&kept, &kept).is_empty());
    }

    /// The hint-tree budget is spent in sweep order, never overspent and never
    /// over-promised: the chunks of a sweep get exactly
    /// `params::HINT_TREE_MAX_FILES` slots between them however the list is
    /// chunked, and a slot goes to an attempt exactly when it holds one of the
    /// first `HINT_TREE_MAX_FILES` list positions - which is what makes the
    /// budget a position rule, immune to the pool schedule.
    #[test]
    fn hint_tree_slots_spend_the_budget_in_sweep_order() {
        let cap = crate::params::HINT_TREE_MAX_FILES;
        for fanout in [1usize, 2, 3, 7, 40] {
            let nchunks = usize::div_ceil(cap, fanout) + 2;
            let mut spent = 0usize;
            for chunk_no in 0..nchunks {
                let slots = hint_tree_slots(chunk_no, fanout);
                assert!(
                    slots <= fanout,
                    "fanout {fanout}: chunk {chunk_no} got {slots} slots for {fanout} attempts"
                );
                spent += slots;
                for slot in 0..fanout {
                    assert_eq!(
                        slot < slots,
                        chunk_no * fanout + slot < cap,
                        "fanout {fanout}: sweep position {}",
                        chunk_no * fanout + slot
                    );
                }
            }
            assert_eq!(
                spent, cap,
                "fanout {fanout}: the chunks spent {spent} of {cap} slots"
            );
        }
    }

    /// Gate: both `1920x1080-*` narrow fields solve end to end over the
    /// installed set at their own ~1 arcsec/px scale, and the print lands on
    /// the published sky truth within `testutil::Truth::check`'s budgets.
    /// Those two truth entries trade the pixel center budget for an absolute
    /// 2.0 arcsec one, looser than 1.5 px at their scale: the published center
    /// is a plate fit that carries a distortion term, and our refine is
    /// similarity-only, so it cannot follow that term.
    ///
    /// Run with the scale hint ([`NARROW_SCALE`]), which keeps selection on the
    /// fine-scale indices - and what a consumer of a narrow field actually
    /// knows, so it is the honest input. Self-skips when the installed set has
    /// no `dimquads = 4` index (`testutil::dim4_index`): which indices a
    /// machine has is not ours to decide, so an install without the 4-star code
    /// path skips instead of failing.
    #[test]
    fn narrow_fields_solve_with_the_scale_hint() {
        if crate::testutil::dim4_index().is_none() {
            return;
        }
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        for (field, truth) in [(NARROW1, &NARROW1_TRUTH), (NARROW2, &NARROW2_TRUTH)] {
            let Some(stars) = crate::testutil::load_field(field) else {
                return;
            };
            let opts = solve_opts(field, 1920.0, 1080.0, Some(NARROW_SCALE), None);
            let sol = solve_field(&opts, &stars, metas, test_deadline())
                .unwrap()
                .unwrap_or_else(|| panic!("{field} must solve at its own scale"));
            truth.check(&sol, 1920.0);
            assert!(
                sol.winner.log_odds >= params::ODDS_TO_SOLVE_LN,
                "{field}: winner below the solve gate: {:?}",
                sol.winner
            );
        }
    }

    /// The hint end to end on a narrow field: a hint at the published center
    /// keeps the tile that contains the field and still solves (print on
    /// truth); a circle [`FAR_HINT_OFFSET_DEG`] deg away leaves it unsolved.
    /// Same scale hint as [`narrow_fields_solve_with_the_scale_hint`].
    #[test]
    fn hint_gate_end_to_end_on_the_narrow_field() {
        if crate::testutil::dim4_index().is_none() {
            return;
        }
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(stars) = crate::testutil::load_field(NARROW1) else {
            return;
        };
        let center = (NARROW1_TRUTH.ra_deg, NARROW1_TRUTH.dec_deg);
        let near = Some(PositionHint {
            ra_deg: center.0,
            dec_deg: center.1,
            radius_deg: DEFAULT_RADECS_RADIUS_DEG,
        });
        let opts = solve_opts(NARROW1, 1920.0, 1080.0, Some(NARROW_SCALE), near);
        let sol = solve_field(&opts, &stars, metas, test_deadline())
            .unwrap()
            .expect("a hint at the true center must still solve");
        NARROW1_TRUTH.check(&sol, 1920.0);

        far_hint_reports_nothing(
            metas,
            &stars,
            &crate::testutil::testdata_path(NARROW1),
            1920.0,
            1080.0,
            Some(NARROW_SCALE),
            center,
        );
    }

    // ---- Generated fields: synthetic stars whose sky truth is exact ---------
    //
    // `genfield` projects reference stars of an installed index through a WCS
    // chosen by the test, so the four numbers the answer is compared against are
    // the four that went into the star list. That allows the tighter budgets of
    // `genfield::TOL_*` in place of the shipped fields' `testutil::Truth::check`,
    // whose truth is a reference run with real-observation noise in it.

    /// Frame of every generated-field leg.
    const GEN_WIDTH: f64 = 1920.0;
    const GEN_HEIGHT: f64 = 1080.0;

    /// Name of a generated run: `solve_field` never opens `SolveOptions.file`
    /// and this one has no file behind it - so it is deliberately not a
    /// `testdata/` path.
    const GEN_PSEUDO_FILE: &str = "generated.xyls";

    /// Seeds of the clean legs. Orientation and parity come from the seed (and
    /// from the index file's star count), so covering both CD branches is the
    /// separate `parity`/`half turn` legs below, which set those two by hand.
    const GEN_SEEDS: [u64; 6] = [1, 2, 3, 5, 8, 13];

    /// Star count of the distractor-only field. Small on purpose: the depth ramp
    /// is a function of the star count, so a long noise list is a long run, not a
    /// stronger gate.
    const NOISE_STARS: usize = 30;

    /// Knobs of the noisy leg: centroid sigma in pixels, one distractor per
    /// `NOISY_EXTRA_DIVISOR` source stars, and how many `sigma/sqrt(n_source)`
    /// of the averaged-down noise the center budget allows.
    const NOISY_SIGMA_PX: f64 = 0.25;
    const NOISY_EXTRA_DIVISOR: usize = 2;
    const NOISY_BUDGET_SIGMAS: f64 = 4.0;

    /// `SolveOptions` of a generated field at `scale`: the shared opts builder
    /// with the frame of these legs and the generator's own scale window.
    fn gen_opts(scale: f64, hint: Option<PositionHint>) -> SolveOptions {
        opts_at(
            Path::new(GEN_PSEUDO_FILE),
            GEN_WIDTH,
            GEN_HEIGHT,
            Some(genfield::scale_window(scale)),
            hint,
        )
    }

    /// `SolveOptions` of a generated field swept **blind**: no scale bound and no
    /// position hint, so the sweep has to pick the index itself - the way the
    /// binary runs for a user who knows nothing about the camera.
    fn gen_opts_blind() -> SolveOptions {
        opts_at(
            Path::new(GEN_PSEUDO_FILE),
            GEN_WIDTH,
            GEN_HEIGHT,
            None,
            None,
        )
    }

    /// One blind solve of a generated star list over the already-selected
    /// indices.
    fn solve_blind(stars: &[Star], selected: &[IndexMeta]) -> Option<Solution> {
        solve_field(&gen_opts_blind(), stars, selected, test_deadline()).unwrap()
    }

    /// Center offset of two reported centers, in px of `scale`.
    fn center_off_px(a: &Solution, b: &Solution, scale: f64) -> f64 {
        crate::testutil::center_off_px(a.ra, a.dec, b.ra, b.dec, scale)
    }

    /// The generated-field gate: center within `center_px` (pixels of the truth
    /// scale), rotation, both printed sizes, parity exact. The same checks
    /// `testutil::Truth::check` makes, at the `genfield::TOL_*` budgets an exact
    /// truth allows (shared with the integration target). The printed sizes also
    /// pin the scale, since the frame size is fixed here.
    fn check_generated(sol: &Solution, truth: &genfield::Truth, center_px: f64) {
        let off_px = crate::testutil::center_off_px(
            sol.ra,
            sol.dec,
            truth.ra_deg,
            truth.dec_deg,
            truth.scale_arcsec,
        );
        assert!(
            off_px <= center_px,
            "center off by {off_px} px (budget {center_px}): truth ({}, {}), got ({}, {})",
            truth.ra_deg,
            truth.dec_deg,
            sol.ra,
            sol.dec
        );
        let drot = crate::testutil::wrap_deg(sol.orient_deg - truth.orient_deg);
        assert!(
            f64::abs(drot) <= genfield::TOL_ORIENT_DEG,
            "rotation off by {drot} deg: got {}, truth {}",
            sol.orient_deg,
            truth.orient_deg
        );
        for (name, got, want) in [
            ("width", sol.width_deg, truth.width_deg),
            ("height", sol.height_deg, truth.height_deg),
        ] {
            assert!(
                f64::abs(got / want - 1.0) <= genfield::TOL_SIZE_FRAC,
                "{name} {got} deg vs truth {want} deg"
            );
        }
        assert_eq!(sol.parity_neg, truth.parity_neg, "parity");
    }

    /// Center budget of a noisy solve: the centroid jitter of an `n`-star fit
    /// averages down as `sigma/sqrt(n)`, so the budget is a few of those with the
    /// clean budget as its floor. `matched_divisor` shrinks `n` to the set the
    /// verifying index can actually pair: 1.0 when the field was cut from that
    /// file, more when it is whatever file won (see [`BLIND_NOISY_N_DIVISOR`]).
    fn noisy_center_budget(n_source: usize, matched_divisor: f64) -> f64 {
        assert!(
            n_source > 0,
            "a field with no source stars has no jitter to average down"
        );
        f64::max(
            genfield::TOL_CENTER_PX,
            NOISY_BUDGET_SIGMAS * NOISY_SIGMA_PX / f64::sqrt(n_source as f64 / matched_divisor),
        )
    }

    /// Gate: clean generated fields land on their own truth. Six seeds, each
    /// swept over the installed set through the generator's scale window.
    #[test]
    fn generated_clean_fields_match_their_truth() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        for seed in GEN_SEEDS {
            let Some(p) = crate::testutil::generated_params(seed, GEN_WIDTH, GEN_HEIGHT) else {
                return;
            };
            let (stars, truth) = genfield::generate_stars(&p).expect("generate");
            let sol = solve_field(
                &gen_opts(truth.scale_arcsec, None),
                &stars,
                metas,
                test_deadline(),
            )
            .unwrap()
            .unwrap_or_else(|| {
                panic!(
                    "generated field at seed {seed} ({} stars, {} arcsec/px) did not solve",
                    stars.len(),
                    truth.scale_arcsec
                )
            });
            check_generated(&sol, &truth, genfield::TOL_CENTER_PX);
        }
    }

    /// Gate: the same patch mirrored in parity is a different image but the
    /// same point on the sky. Each leg lands on its own truth (which differs
    /// in rotation, not center), so the two reports must agree within the sum
    /// of their budgets - the reflection branch of the fit may not bias the
    /// center. The mirrored placement is a different set of index stars, so
    /// neither leg solving is a data limitation; only one solving is a failure.
    #[test]
    fn generated_field_parity_flip_keeps_the_center() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(base) = crate::testutil::generated_params(21, GEN_WIDTH, GEN_HEIGHT) else {
            return;
        };
        let mut legs = Vec::new();
        for parity_neg in [true, false] {
            let p = genfield::Params {
                parity_neg,
                ..base.clone()
            };
            let (stars, truth) = genfield::generate_stars(&p).expect("generate");
            let sol = solve_field(
                &gen_opts(truth.scale_arcsec, None),
                &stars,
                metas,
                test_deadline(),
            )
            .unwrap();
            let Some(sol) = sol else { continue };
            check_generated(&sol, &truth, genfield::TOL_CENTER_PX);
            legs.push(sol);
        }
        if legs.is_empty() {
            crate::testutil::skip("neither parity of the generated patch solved");
            return;
        }
        assert_eq!(
            legs.len(),
            2,
            "only one parity of one patch solved: the other must solve too"
        );
        let off = center_off_px(&legs[0], &legs[1], base.scale_arcsec);
        assert!(
            off <= 2.0 * genfield::TOL_CENTER_PX,
            "the two parities of one patch disagree by {off} px of center"
        );
    }

    /// Gate: rotating the patch leaves center, scale and parity alone and moves
    /// the printed rotation by exactly the angle it was rotated by, wrapped into
    /// the reported range. Both legs are gated on their own truth, so this is a
    /// check of the rotation bookkeeping, not of the pattern.
    #[test]
    fn generated_field_half_turn_reports_the_wrapped_rotation() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(base) = crate::testutil::generated_params(22, GEN_WIDTH, GEN_HEIGHT) else {
            return;
        };
        let mut legs = Vec::new();
        for turn in [0.0, 180.0] {
            let p = genfield::Params {
                orient_deg: base.orient_deg + turn,
                ..base.clone()
            };
            let (stars, truth) = genfield::generate_stars(&p).expect("generate");
            let sol = solve_field(
                &gen_opts(truth.scale_arcsec, None),
                &stars,
                metas,
                test_deadline(),
            )
            .unwrap()
            .unwrap_or_else(|| panic!("the patch rotated by {turn} deg did not solve"));
            check_generated(&sol, &truth, genfield::TOL_CENTER_PX);
            legs.push(sol);
        }
        let drot = crate::testutil::wrap_deg(legs[1].orient_deg - legs[0].orient_deg);
        // The outer wrap turns "180 +/- the two legs' slack" into a small number
        // again: each report may land on either side of the reported range.
        assert!(
            f64::abs(crate::testutil::wrap_deg(drot - 180.0)) <= 2.0 * genfield::TOL_ORIENT_DEG,
            "a half turn moved the rotation by {drot} deg"
        );
        assert!(
            center_off_px(&legs[0], &legs[1], base.scale_arcsec) <= 2.0 * genfield::TOL_CENTER_PX,
            "a half turn moved the center"
        );
        assert_eq!(legs[0].parity_neg, legs[1].parity_neg);
    }

    /// Gate: the `--ra/--dec` hint on a field with an exact center. A circle
    /// at the truth center keeps the solution; the far-hint legs reuse
    /// `far_hint_reports_nothing`, proven on the very index the control won.
    #[test]
    fn generated_hint_at_the_center_solves_and_a_far_one_does_not() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(p) = crate::testutil::generated_params(31, GEN_WIDTH, GEN_HEIGHT) else {
            return;
        };
        let (stars, truth) = genfield::generate_stars(&p).expect("generate");
        let near = PositionHint {
            ra_deg: truth.ra_deg,
            dec_deg: truth.dec_deg,
            radius_deg: DEFAULT_RADECS_RADIUS_DEG,
        };
        let sol = solve_field(
            &gen_opts(truth.scale_arcsec, Some(near)),
            &stars,
            metas,
            test_deadline(),
        )
        .unwrap()
        .expect("a hint at the true center must keep the solution");
        check_generated(&sol, &truth, genfield::TOL_CENTER_PX);

        far_hint_reports_nothing(
            metas,
            &stars,
            Path::new(GEN_PSEUDO_FILE),
            GEN_WIDTH,
            GEN_HEIGHT,
            Some(genfield::scale_window(truth.scale_arcsec)),
            (truth.ra_deg, truth.dec_deg),
        );
    }

    /// Gate against false positives: a frame of uniform-random stars - the
    /// bad-pixel and diffraction-spike pattern, with no real pattern under it
    /// - must not talk the odds scan into a solution.
    #[test]
    fn generated_distractor_only_field_is_not_solved() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        // Only the plate scale of a real field is reused, so the sweep is pointed
        // at a band that exists instead of the blind 0.1-180 deg range.
        let Some(p) = crate::testutil::generated_params(41, GEN_WIDTH, GEN_HEIGHT) else {
            return;
        };
        let (stars, _noise) =
            genfield::noise_stars(GEN_WIDTH, GEN_HEIGHT, p.scale_arcsec, NOISE_STARS, 1);
        // The gate asserts a negative, so the list has to reach the sweep
        // non-empty; that a sweep of this frame does work is the clean legs'.
        assert_eq!(stars.len(), NOISE_STARS, "the noise list reaches the sweep");
        let sol = solve_field(
            &gen_opts(p.scale_arcsec, None),
            &stars,
            metas,
            test_deadline(),
        )
        .unwrap();
        assert!(
            sol.is_none(),
            "{NOISE_STARS} random stars solved: {:?}",
            sol.map(|s| s.winner)
        );
    }

    /// Gate: a field with centroid noise and distractor stars still lands on the
    /// truth, within the budget the noise itself implies (see
    /// [`noisy_center_budget`]); a solver that drifted with noise quality would
    /// show up as a leg over it.
    ///
    /// The same field without noise is solved first as a control: a frame this
    /// install cannot solve at all is a skip, while one that solves clean and
    /// then fails under a fraction of a pixel of jitter is a failure.
    #[test]
    fn generated_noisy_field_stays_within_its_statistical_budget() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        let Some(base) = crate::testutil::generated_params(51, GEN_WIDTH, GEN_HEIGHT) else {
            return;
        };
        let (clean_stars, clean_truth) = genfield::generate_stars(&base).expect("generate");
        let clean = solve_field(
            &gen_opts(clean_truth.scale_arcsec, None),
            &clean_stars,
            metas,
            test_deadline(),
        )
        .unwrap();
        let Some(clean) = clean else {
            crate::testutil::skip("the clean control of the noisy generated field did not solve");
            return;
        };
        check_generated(&clean, &clean_truth, genfield::TOL_CENTER_PX);

        // A few distractors per source star: enough to move the verifier's
        // first-come matching, not enough to make the run a batch job.
        let p = genfield::Params {
            jitter_px: NOISY_SIGMA_PX,
            extra: clean_truth.n_source / NOISY_EXTRA_DIVISOR,
            ..base.clone()
        };
        let (stars, truth) = genfield::generate_stars(&p).expect("generate");
        assert_eq!(truth.n_source, clean_truth.n_source);
        let sol = solve_field(
            &gen_opts(truth.scale_arcsec, None),
            &stars,
            metas,
            test_deadline(),
        )
        .unwrap()
        .unwrap_or_else(|| {
            panic!(
                "a noisy field ({} stars + {} distractors, {NOISY_SIGMA_PX} px) did not solve, \
                 while the same field clean did",
                truth.n_source, truth.n_extra
            )
        });
        check_generated(&sol, &truth, noisy_center_budget(truth.n_source, 1.0));
    }

    /// Draws of the blind gate, each solved twice (clean and noisy). A blind
    /// sweep is the slow path (every installed index whose band overlaps the
    /// blind range is tried, largest quad first), so the draws run on the
    /// shared rayon pool: each holds one index resident, and the pool overlaps
    /// one sweep's serial phases (index load, verify) with another's parallel
    /// enumeration (the draw-collection loop below stays serial).
    const BLIND_DRAWS: usize = 10;
    /// Index picks spent to collect [`BLIND_DRAWS`] fields. An index whose quad
    /// band does not fit this frame hosts no field at all; such a pick is cheap,
    /// but it must not be chased forever.
    const BLIND_PICKS: usize = 60;
    /// Fewest fields the installed set must host for this gate to run.
    const BLIND_MIN_FIELDS: usize = 3;
    /// First seed of the blind draws: draw `n` is driven by `BLIND_SEED + n`.
    const BLIND_SEED: u64 = 7000;
    /// The noisy leg's center budget divides the field's star count by this
    /// before taking the statistical rule. The windowed noisy leg verifies against
    /// the very file the field was cut from, where nearly every source star finds
    /// its partner and `sigma/sqrt(n_source)` is the right scale; here whichever
    /// file won does the verifying, and its reference list can hold only a
    /// fraction of the field's stars, so the jitter averages down over that
    /// smaller matched set. Six is that safety factor.
    const BLIND_NOISY_N_DIVISOR: f64 = 6.0;

    /// Gate: fields generated from pseudo-randomly chosen index files of the
    /// installed set, each solved blind twice - once clean, once with centroid
    /// noise and distractor stars. The windowed legs point the sweep at the
    /// generator's own scale window; here the run gets nothing but 0.1-180 deg
    /// of field width, so a selection or scale-filtering mistake a windowed
    /// leg cannot see shows up as a field that does not solve - and the
    /// winning index need not be the one the stars came from.
    ///
    /// The noisy leg is the closest this suite comes to a real observation: an
    /// index nobody points at, no hints, centroids off by [`NOISY_SIGMA_PX`],
    /// stars not on the sky. It keeps the knobs of
    /// [`generated_noisy_field_stays_within_its_statistical_budget`] and the
    /// clean solve of the same draw as control (noise alone may not unsolve a
    /// field the clean list solved), but its center budget is looser - see
    /// [`BLIND_NOISY_N_DIVISOR`] - with the median of the draws as the tight
    /// assertion: one draw on a sparser winner is not a failure, a systematic
    /// drift is.
    ///
    /// Which files host a field at this frame is a property of the install (a
    /// coarse all-sky-only set hosts none), so a set below [`BLIND_MIN_FIELDS`]
    /// skips, one between that and [`BLIND_DRAWS`] runs with what it has.
    #[test]
    fn generated_fields_solve_blind() {
        let Some(metas) = crate::testutil::metas() else {
            return;
        };
        // The draw list is fixed before any solving starts, so the set of fields
        // cannot depend on how the pool schedules them.
        let mut draws = Vec::new();
        for n in 0..BLIND_PICKS as u64 {
            if draws.len() == BLIND_DRAWS {
                break;
            }
            let seed = BLIND_SEED + n;
            let Some(meta) = genfield::random_index_from(metas, seed) else {
                break;
            };
            if let Ok(p) = genfield::random_params(&meta.path, seed, GEN_WIDTH, GEN_HEIGHT) {
                draws.push(p);
            }
        }
        if draws.len() < BLIND_MIN_FIELDS {
            crate::testutil::skip("the installed index set hosts no narrow field to solve blind");
            return;
        }
        // One selection for all of them: the blind opts are the same for every
        // draw, and this is the gate the binary applies before the sweep.
        let selected = select_indices(metas, &gen_opts_blind());
        assert!(
            !selected.is_empty(),
            "the blind scale range pruned every one of {} index files",
            metas.len()
        );
        /// Both blind answers of one draw, each with the truth it is judged
        /// against: the noisy field keeps the source stars but adds its own
        /// distractors, so its truth is not the clean one.
        struct Answers {
            truth: genfield::Truth,
            clean: Option<Solution>,
            noisy_truth: genfield::Truth,
            noisy: Option<Solution>,
        }
        let answers: Vec<Answers> = draws
            .par_iter()
            .map(|p| {
                let (stars, truth) = genfield::generate_stars(p).expect("generate the clean field");
                let clean = solve_blind(&stars, &selected);
                let noisy_p = genfield::Params {
                    jitter_px: NOISY_SIGMA_PX,
                    extra: truth.n_source / NOISY_EXTRA_DIVISOR,
                    ..p.clone()
                };
                let (nstars, noisy_truth) =
                    genfield::generate_stars(&noisy_p).expect("generate the noisy field");
                let noisy = solve_blind(&nstars, &selected);
                Answers {
                    truth,
                    clean,
                    noisy_truth,
                    noisy,
                }
            })
            .collect();
        // Noisy center residuals of every draw, for the median below.
        let mut residuals = Vec::new();
        for (p, a) in draws.iter().zip(answers) {
            assert_eq!(
                a.noisy_truth.n_source,
                a.truth.n_source,
                "jitter and distractors changed the source stars of {}",
                p.index.display()
            );
            let clean = a.clean.unwrap_or_else(|| {
                panic!(
                    "{} did not solve blind at {} arcsec/pixel ({} stars)",
                    p.index.display(),
                    a.truth.scale_arcsec,
                    a.truth.n_source
                )
            });
            check_generated(&clean, &a.truth, genfield::TOL_CENTER_PX);
            let noisy = a.noisy.unwrap_or_else(|| {
                panic!(
                    "{} went unsolved blind once {NOISY_SIGMA_PX} px of jitter and {} \
                     distractors were added, although the same field solved clean",
                    p.index.display(),
                    a.noisy_truth.n_extra
                )
            });
            let budget_px = noisy_center_budget(a.noisy_truth.n_source, BLIND_NOISY_N_DIVISOR);
            check_generated(&noisy, &a.noisy_truth, budget_px);
            residuals.push(crate::testutil::center_off_px(
                noisy.ra,
                noisy.dec,
                a.noisy_truth.ra_deg,
                a.noisy_truth.dec_deg,
                a.noisy_truth.scale_arcsec,
            ));
        }
        // The per-draw budget is a cap; the quality statement is the median.
        residuals.sort_by(f64::total_cmp);
        let mid = residuals.len() / 2;
        let median_px = if residuals.len() % 2 == 1 {
            residuals[mid]
        } else {
            0.5 * (residuals[mid - 1] + residuals[mid])
        };
        assert!(
            median_px <= genfield::TOL_CENTER_PX,
            "the median noisy blind center residual is {median_px} px over {} draws (budget {} px)",
            residuals.len(),
            genfield::TOL_CENTER_PX
        );
    }
}
