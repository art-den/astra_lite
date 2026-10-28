//! `starsolver`: a clone of a small subset of astrometry.net `solve-field` —
//! star pixel coordinates (a `.xyls` file) plus a directory of index files go
//! in, a plate solution (field center, field size, rotation, parity) comes out
//! on stdout.
//!
//! The pipeline (`run_into`, driven by `sweep::solve_field`):
//!
//! ```text
//! .xyls -> xyls::load + augment::preprocess (dense-line stars dropped, the rest
//!                                           binned on a grid over their own
//!                                           bounding box and emitted
//!                                           round-robin over the bins)
//!       -> sweep::select_indices            (index scale band, then the healpix
//!                                            tile gate of a --ra/--dec hint)
//!       -> sweep::solve_field               (object-count rounds of the star list;
//!                                            per index and round: field quad codes
//!                                            -> codes kd-tree -> WCS fit -> verify
//!                                            -> refine -> field-center recentring)
//!       -> output::write_solution           (the success block, into `out`)
//! ```
//!
//! Two facts the sweep needs are settled before the star list is loaded: the
//! frame size - `-w`/`-e` as a pair, else the `IMAGEW`/`IMAGEH` cards of the
//! file (`SolveOptions::from_args`) - and the index directories to scan
//! (`IndexDirs::resolve`).
//!
//! The public API is [`Args`], [`run`], [`run_into`] and the [`Solution`] data
//! [`run_into`] returns, nothing else: every module below stays private, so the
//! crate makes no library-API promises (frozen scope). The one exception is
//! [`genfield`], the synthetic-field generator the test suite shares between its
//! unit and integration targets; it exists only under the non-default
//! `test-util` feature (or `cfg(test)`), so a normal build - and the shipped
//! binary - still exposes exactly the four items above. `main.rs` is a thin
//! binary that parses [`Args`], calls [`run`] and maps its result onto the exit
//! codes (0 solved, 1 not solved, 2 error); [`run`] is [`run_into`] bound to the
//! process stdout/stderr, while an embedding host gets the solution as data and
//! chooses its own sinks.
//!
//! Minimal-injection scope: only the glue's own lines honor the `progress` sink
//! (the verbose `[xy]`/`[scale]`/`[index]` output and the warnings this file
//! prints). Messages emitted deeper down still go to the process stderr: the
//! index-directory and index-scan warnings, the out-of-frame and dense-line
//! star warnings, and the verbose round/index/hit/refine logs of the solve loop.

// The modules the pipeline is built from; all stay private, and the re-exports
// plus `run`/`run_into` below are the crate's whole public surface. Where a
// module carries items the shipped pipeline never calls - a reference formula
// the tests check an inlined copy against, a tunable kept only to document a
// frozen value - the `dead_code` lint is silenced on that `mod`.
mod augment;
mod cli;
#[allow(dead_code)]
mod codes;
mod coords;
mod fits;
// Test-only surface; see the module docs for why it is public and the pipeline
// modules are not.
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub mod genfield;
mod healpix;
mod index;
mod indexdir;
mod kdtree;
mod output;
// Several of the frozen tunables are documentation only: nothing in the pipeline
// reads them (each constant says so where the value that is compared lives).
#[allow(dead_code)]
mod params;
mod refine;
mod solver;
mod sweep;
#[cfg(test)]
mod testutil;
mod verify;
mod wcs;
mod xyls;

use anyhow::{Result, ensure};
use std::io::Write;
use std::time::{Duration, Instant};

use cli::{SolveOptions, given_scale_missing};
use index::Index;
use output::write_solution;
use sweep::{MIN_CPULIMIT_S, select_indices, solve_field};

// `Args` comes into scope through this re-export; adding it to the `use cli::`
// above would be E0252 (the same name imported twice).
pub use cli::Args;
pub use sweep::Solution;

/// The full pipeline with injectable sinks: the five-line success block (and the
/// `--dump-index` table dump) goes to `out`, the glue's progress lines and
/// warnings to `progress`.
///
/// The tri-state result carries the data: `Ok(Some(_))` on a solution,
/// `Ok(None)` when the field was not solved (or `--dump-index` ran), `Err` on
/// bad input or a format we cannot read.
pub fn run_into(
    args: &Args,
    out: &mut dyn Write,
    progress: &mut dyn Write,
) -> Result<Option<Solution>> {
    // Hidden debug aid (`--dump-index`): show one index file's header keys,
    // table counts, kd-tree layout and a few dequantized stars, then stop. The
    // `Ok(None)` is why `run` needs its extra exit-code term.
    if let Some(path) = &args.dump_index {
        let index = Index::load(path)?;
        write!(out, "{}", index.summary())?;
        return Ok(None);
    }

    let opts = SolveOptions::from_args(args)?;
    if opts.verbose && given_scale_missing(opts.units, args) {
        writeln!(
            progress,
            "[scale] --scale-units was given without --scale-low/--scale-high; ignored"
        )?;
    }

    // Resolved before the star table is read, so a system that offers no index
    // directory at all (no readable config `add_path`, no built-in install dir)
    // fails without parsing the star list. `describe()` - the directories plus
    // where they came from - is what the `[index]` line and the "no index
    // matches the scale range" warning print.
    let index_dirs = indexdir::IndexDirs::resolve(args.index_dir.as_deref())?;

    let stars = xyls::load(&opts.file)?;
    ensure!(
        !stars.is_empty(),
        "{} contains no stars",
        opts.file.display()
    );
    // Warning only, never a filter: dropping a star here would quietly change
    // the list the depth ramp and verification run on. The frame is the CLI
    // `-w`/`-e` pair, or the file's IMAGEW/IMAGEH when that pair was not given.
    xyls::warn_out_of_frame(&stars, opts.width, opts.height);
    if opts.verbose {
        writeln!(
            progress,
            "[xy] {} stars from {} ({}x{})",
            stars.len(),
            opts.file.display(),
            opts.width,
            opts.height
        )?;
        writeln!(
            progress,
            "[scale] search range {:.6} - {:.6} arcsec/pixel",
            opts.app_min, opts.app_max
        )?;
    }

    if opts.verbose {
        writeln!(
            progress,
            "[index] index files searched in {}",
            index_dirs.describe()
        )?;
    }
    let metas = index::load_metas_in(&index_dirs.dirs, index_dirs.unusable)?;
    let selected = select_indices(&metas, &opts);
    if opts.verbose {
        for meta in &metas {
            // Match by (indexid, healpix): a dropped tile must not make the
            // surviving tiles of the same indexid read as "skip".
            let used = selected
                .iter()
                .any(|s| s.indexid == meta.indexid && s.healpix == meta.healpix);
            let mark = if used { "using" } else { "skip " };
            writeln!(progress, "[index] {mark} {}", meta.describe())?;
            // A tile that fits the scale but is still skipped can only have been
            // dropped by the position gate (an all-sky index is never gated);
            // name the hint circle it failed to overlap.
            if !used
                && meta.healpix >= 0
                && meta.fits_scale(opts.app_min, opts.app_max, opts.width, opts.height)
                && let Some(hint) = opts.radecs
            {
                writeln!(
                    progress,
                    "[index]   not within {} degrees of (RA,Dec) = ({},{})",
                    hint.radius_deg, hint.ra_deg, hint.dec_deg
                )?;
            }
        }
    }
    if selected.is_empty() {
        writeln!(
            progress,
            "Warning: no index in {} matches the scale range {:.6} - {:.6} arcsec/pixel",
            index_dirs.describe(),
            opts.app_min,
            opts.app_max
        )?;
        return Ok(None);
    }

    // `selected` is already in sweep order: `load_metas_in` sorts by descending
    // index quad scale, so the indices that cover the widest fields go first.
    // Ordering by the indices rather than by their file names is a project
    // decision. The sweep reports the first index that yields a verified solution
    // and stops there; the attempts behind it in the same chunk run until
    // cancelled and later chunks are never issued (see `solve_field`).
    //
    // The solve budget `--cpulimit` (seconds) becomes an absolute wall-clock
    // deadline; it is floored so a 0 budget cannot expire before the first check.
    let deadline =
        Instant::now() + Duration::from_secs_f64(f64::max(opts.cpulimit, MIN_CPULIMIT_S));

    let solution = solve_field(&opts, &stars, &selected, deadline)?;
    match solution {
        Some(sol) => {
            write_solution(&sol, out)?;
            Ok(Some(sol))
        }
        None => Ok(None),
    }
}

/// [`run_into`] bound to the process stdout/stderr - the binary's entry point
/// behind the exit-code contract (0 solved, 1 not solved, 2 error). A
/// `--dump-index` run solves nothing yet still exits 0, hence the extra term.
pub fn run(args: &Args) -> Result<bool> {
    // Deliberately NOT `stdout().lock()`: it would stay held across the whole
    // solve for no benefit - nothing on the solve path writes to `out`, and the
    // verbose lines the rayon workers emit (solver hit, sweep verify, refine)
    // go to stderr, whose stdio lock is a separate one. A plain
    // `Stdout`/`Stderr` locks per write, the same way `println!`/`eprintln!`
    // always did.
    let mut out = std::io::stdout();
    let mut progress = std::io::stderr();
    let solved = run_into(args, &mut out, &mut progress)?;
    Ok(solved.is_some() || args.dump_index.is_some())
}
