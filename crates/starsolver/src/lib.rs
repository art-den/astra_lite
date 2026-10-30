//! `starsolver`: a clone of a small subset of astrometry.net `solve-field`:
//! a `.xyls` star list plus a directory of index files in, a plate solution
//! (field center, field size, rotation, parity) printed to stdout.
//!
//! The pipeline (`run_into`, driven by `sweep::solve_field`):
//!
//! ```text
//! .xyls -> xyls::load + augment::preprocess (drops dense-line stars, bins the
//!           rest on a bounding-box grid, emits round-robin over the bins)
//!       -> xyls::brightest_first   (descending flux, stable, capped at
//!                                   params::TOP_OBJS)
//!       -> sweep::select_indices   (index scale band, then the healpix tile
//!                                   gate of a --ra/--dec hint)
//!       -> sweep::solve_field      (object-count rounds of the star list; per
//!                                   index and round: field quad codes -> codes
//!                                   kd-tree -> WCS fit -> verify -> refine ->
//!                                   field-center recentring)
//!       -> output::write_solution  (the success block, into `out`)
//! ```
//!
//! The frame size (`-w`/`-e` as a pair, else the file's `IMAGEW`/`IMAGEH`) and
//! the index directories are settled before the star list is read.
//!
//! The public API is [`Args`], [`run`], [`run_into`] and the [`Solution`] data
//! [`run_into`] returns, nothing else: every module below stays private, so the
//! crate makes no library-API promises (frozen scope). The sole exception is
//! `genfield`, the synthetic-field generator the test suite shares, built only
//! under the non-default `test-util` feature (or `cfg(test)`), so a normal build
//! exposes exactly the four items above. `main.rs` is a thin binary that parses
//! [`Args`], calls [`run`] and maps its result onto the exit codes (0 solved,
//! 1 not solved, 2 error). [`run`] is [`run_into`] bound to the process
//! stdout/stderr; an embedding host gets the solution as data, its own sinks.
//!
//! Minimal-injection scope: only the glue's own lines honor `progress` (verbose
//! `[xy]`/`[scale]`/`[index]` lines, this file's warnings); deeper messages
//! (index-dir and index-scan warnings, out-of-frame and dense-line star
//! warnings, solve-loop verbose round/index/hit/refine logs) go to stderr.

// All modules stay private; the re-exports and `run`/`run_into` below are the
// crate's whole public surface. A `mod` silences `dead_code` where the shipped
// pipeline never calls some of its items (a reference formula the tests mirror,
// a documentation-only frozen tunable).
mod augment;
mod cli;
#[allow(dead_code)]
mod codes;
mod coords;
mod fits;
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub mod genfield;
mod healpix;
mod index;
mod indexdir;
mod kdtree;
mod output;
// Several frozen tunables are documentation only: nothing reads them.
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

// Adding `Args` to the `use cli::` above would be E0252 (name imported twice).
pub use cli::Args;
pub use sweep::Solution;

/// The full pipeline with injectable sinks: the five-line success block (and the
/// `--dump-index` table dump) go to `out`, the glue's progress lines and
/// warnings to `progress`.
///
/// `Ok(Some(_))` on a solution, `Ok(None)` when the field was not solved (or
/// `--dump-index` ran), `Err` on bad input or an unreadable format.
pub fn run_into(
    args: &Args,
    out: &mut dyn Write,
    progress: &mut dyn Write,
) -> Result<Option<Solution>> {
    // Hidden debug aid (`--dump-index`): dump one index file, then stop. The
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

    // No index directory at all (no readable config `add_path`, no built-in
    // install dir) fails here, before the star list is parsed.
    let index_dirs = indexdir::IndexDirs::resolve(args.index_dir.as_deref())?;

    let stars = xyls::load(&opts.file)?;
    ensure!(
        !stars.is_empty(),
        "{} contains no stars",
        opts.file.display()
    );
    // Warning only, never a filter: dropping a star here would change the list
    // the depth ramp and verification run on.
    xyls::warn_out_of_frame(&stars, opts.width, opts.height);
    // Cap after the warning so that warning still counts the full loaded list.
    let loaded = stars.len();
    let stars = xyls::brightest_first(stars, params::TOP_OBJS);
    if opts.verbose {
        // Keep the plain line byte-identical when the cap dropped nothing.
        let (counts, kept_note) = if stars.len() < loaded {
            (
                format!("{} of {} stars", stars.len(), loaded),
                ", brightest kept",
            )
        } else {
            (format!("{} stars", stars.len()), "")
        };
        writeln!(
            progress,
            "[xy] {} from {} ({}x{}){}",
            counts,
            opts.file.display(),
            opts.width,
            opts.height,
            kept_note
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
            // A skipped tile that fits the scale was dropped by the position
            // gate (all-sky is never gated): name the hint circle it missed.
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
    // index quad scale, so the widest-field indices go first. The sweep reports
    // the first index that yields a verified solution and stops there; attempts
    // behind it in the same chunk run until cancelled, later chunks are never
    // issued (see `solve_field`).
    //
    // `--cpulimit` (seconds) becomes an absolute wall-clock deadline, floored so
    // a 0 budget cannot expire before the first check.
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
    // Deliberately NOT `stdout().lock()`: it would be held across the whole
    // solve for no benefit - nothing on the solve path writes to `out`, and the
    // verbose lines the rayon workers emit go to stderr, whose stdio lock is a
    // separate one. A plain `Stdout`/`Stderr` locks per write, as `println!`
    // always did.
    let mut out = std::io::stdout();
    let mut progress = std::io::stderr();
    let solved = run_into(args, &mut out, &mut progress)?;
    Ok(solved.is_some() || args.dump_index.is_some())
}
