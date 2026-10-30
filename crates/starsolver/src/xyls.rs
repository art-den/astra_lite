//! Reading `.xyls` — the input star list.
//!
//! The star table is a BINTABLE whose first column (`TTYPE1`) is `X`, then `Y`
//! and optionally `FLUX`; HDU0 is the plain IMAGE header, which may carry the
//! frame-size cards. Positions are read as doubles with no re-basing or scaling,
//! and any single-element numeric `TFORM` reads (`1E` and integers as well as
//! `1D`).
//!
//! A `.xyls` need not state the frame size (then the CLI has to supply it); it
//! may carry `IMAGEW`/`IMAGEH` keys, which [`frame_size`] reads for callers that
//! omit `-w`/`-e`.
//!
//! Rows with non-finite X/Y are dropped and non-finite FLUX clamped
//! (`stars_from`); out-of-frame stars are only counted and warned about, never
//! removed (`warn_out_of_frame`). The survivors go through the position
//! reordering in [`crate::augment`] - the caller receives that order, not the
//! stored row order - and [`brightest_first`] is a separate step `run_into`
//! applies on top, so the solver does not work in the [`crate::augment`] order.

use anyhow::{Context, Result, bail, ensure};
use std::path::Path;

use crate::augment;
use crate::fits::{Fits, FitsHeaders, Header, Headers, Table};

/// One detected source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    pub x: f64,
    pub y: f64,
    /// The `FLUX` column, unchanged apart from the non-finite clamp in
    /// `stars_from`. Quad enumeration never reads it: brightness there is the
    /// star's POSITION in the list (see [`crate::augment`]). Only
    /// `verify::probe_stars` and [`brightest_first`] read it; with no FLUX column
    /// every flux is 0.0 and both degenerate to list order.
    pub flux: f64,
}

/// Project decision, not a format limit: reject absurdly large xylists before
/// the derived allocations happen.
///
/// The peak at the cap is an analytic bound over what this reader derives (its
/// `Vec<Star>` plus the `augment` transients it feeds), inside the 200 MB RSS
/// budget; the file bytes are already mmap'd by `Fits::open`, so the cap bounds
/// the derived allocations, not the raw read. It bounds the read, not the solve:
/// `run_into` keeps the brightest [`crate::params::TOP_OBJS`] stars afterwards, so
/// an over-cap file stays an error rather than a silent truncation.
const MAX_XYLS_ROWS: usize = 1_000_000;

/// Opens the star table: the first BINTABLE whose first column (`TTYPE1`) is `X`.
/// A table carrying `X` further right cannot be opened, and is reported by name,
/// not by the misleading "no BINTABLE column named X" the fallback below gives.
fn xy_table(fits: &Fits) -> Result<Table<'_>> {
    if let Some(i) = fits.find_table("X") {
        return fits.table(i);
    }
    if let Some((_hdu, col)) = fits.find_column("X") {
        bail!("found X in column {col}: only a first-column X is supported");
    }
    fits.table_by_ttype("X")
}

/// How many BINTABLEs start with an `X` column (their `TTYPE1` is `X`).
fn count_x_tables(fits: &Fits) -> usize {
    fits.table_names()
        .iter()
        .filter(|name| **name == "X")
        .count()
}

/// Reads the star list from an already parsed FITS file. Every returned star has
/// finite `x`/`y` and finite `flux`: a non-finite coordinate can never match and
/// the position reordering expects finite X/Y, so such a row is dropped instead
/// of turning the file into an error; a non-finite `FLUX` is no reason to lose a
/// detection, so that star is kept at the dimmest finite value. All-unusable rows
/// come back empty, which the caller reports as an input error.
///
/// The returned order is not the file's: rows are read in file order, then run
/// through [`crate::augment::preprocess`].
fn stars_from(fits: &Fits) -> Result<Vec<Star>> {
    let ntables = count_x_tables(fits);
    if ntables > 1 {
        eprintln!("Warning: {ntables} X/Y tables found; only the first is used");
    }
    let table = xy_table(fits)?;
    // Bound the allocations below before any of them happens.
    ensure!(
        table.nrows <= MAX_XYLS_ROWS,
        "xylist has {} rows, more than the supported maximum {MAX_XYLS_ROWS}",
        table.nrows
    );
    let data = table.f64_columns(&["X", "Y"])?;
    // FLUX is optional; a list without it reads as equally bright stars (0.0).
    let fluxes = if table.column("FLUX").is_ok() {
        // Exactly one vector comes back per requested column name.
        Some(table.f64_columns(&["FLUX"])?.remove(0))
    } else {
        None
    };
    let (xs, ys) = (&data[0], &data[1]);

    let mut stars = Vec::with_capacity(xs.len());
    let (mut dropped, mut clamped) = (0usize, 0usize);
    for i in 0..xs.len() {
        let (x, y) = (xs[i], ys[i]);
        if !(f64::is_finite(x) && f64::is_finite(y)) {
            dropped += 1;
            continue;
        }
        // A column always holds `nrows` values, so the index is in range.
        let raw_flux = fluxes.as_ref().map_or(0.0, |f| f[i]);
        // Keep the star: only its brightness is corrupt. The clamp decides where
        // it lands in `brightest_first`, since f64::MIN sorts last.
        let flux = if f64::is_finite(raw_flux) {
            raw_flux
        } else {
            clamped += 1;
            f64::MIN
        };
        stars.push(Star { x, y, flux });
    }
    if dropped + clamped > 0 {
        eprintln!(
            "Warning: {dropped} stars with non-finite X/Y dropped, {clamped} non-finite \
             fluxes set to the dimmest value"
        );
    }
    // Free the column vectors before `preprocess` allocates its own transients:
    // at the row cap they are 24 MB of peak that would otherwise overlap.
    drop((data, fluxes));
    // No flux sort here: `preprocess` bins the stars over their bounding box and
    // emits them round-robin, and the depth ramp reads its windows from that.
    Ok(augment::preprocess(stars))
}

/// Slack, in px, for the out-of-frame warning (edge detections straddle the
/// boundary). Diagnostic only, it never affects the solver numerics.
const FRAME_MARGIN_PX: f64 = 5.0;

/// Is a (finite) star clearly outside the `width` x `height` frame?
fn out_of_frame(star: &Star, width: f64, height: f64) -> bool {
    star.x < -FRAME_MARGIN_PX
        || star.x > width + FRAME_MARGIN_PX
        || star.y < -FRAME_MARGIN_PX
        || star.y > height + FRAME_MARGIN_PX
}

/// Warns (stderr) about stars outside the image frame; returns their count.
///
/// Warning only, never a filter: dropping a star here would silently change the
/// star list the depth ramp and verification work on. The count is a diagnostic.
pub fn warn_out_of_frame(stars: &[Star], width: f64, height: f64) -> usize {
    let n = stars
        .iter()
        .filter(|s| out_of_frame(s, width, height))
        .count();
    if n > 0 {
        eprintln!(
            "Warning: {n}/{} stars are outside the {width}x{height} frame",
            stars.len()
        );
    }
    n
}

/// Reads the star list from disk in the order [`crate::augment`] delivers it:
/// dense source lines filtered out, survivors interleaved over their position
/// bins, not the order the rows are stored in.
pub fn load(path: &Path) -> Result<Vec<Star>> {
    let fits = Fits::open(path).with_context(|| format!("loading xylist {}", path.display()))?;
    stars_from(&fits).with_context(|| format!("reading xylist {}", path.display()))
}

/// Keeps the brightest `cap` stars, in descending-flux order.
///
/// The sort is stable, so equal fluxes - and a file with no FLUX column, where
/// every flux is 0.0 - keep the incoming [`crate::augment`] order, and a list at
/// or below `cap` is only sorted, never cut. Sorting still pays off there: the
/// depth ramp reads its star windows out of list position, so bright stars first
/// reach their quads in cheaper rounds. The cap trade-off is in
/// [`crate::params::TOP_OBJS`]; every flux must be finite, per [`stars_from`].
pub(crate) fn brightest_first(mut stars: Vec<Star>, cap: usize) -> Vec<Star> {
    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(cap);
    stars.shrink_to_fit(); // the read buffer of a 1M-row file is ~24 MB
    stars
}

/// Frame size recorded in the file: the `IMAGEW`/`IMAGEH` pair of the primary
/// header, else of the first extension header. The CLI `-w`/`-e` pair wins - the
/// file is consulted only when that pair is missing or incomplete, never mixed
/// with it (a lone `-w` never pairs with a file `IMAGEH`; see
/// `SolveOptions::from_args`). Only the headers are read, never the table data.
///
/// `Ok(None)` means "no usable frame size": no pair in either header, one card in
/// each (they must come as a pair out of one header), a non-integer card value (a
/// float-formatted `4656.0` reads as a float), a size below 1 or one that does not
/// fit `u32`. All collapse into the caller's single exit-2 "width and height must
/// be given or stored in the headers" error.
pub fn frame_size(path: &Path) -> Result<Option<(u32, u32)>> {
    let fits = FitsHeaders::open(path)
        .with_context(|| format!("reading headers of {}", path.display()))?;
    Ok(frame_size_from(&fits))
}

/// The lookup behind [`frame_size`]: both cards have to come from the *same*
/// header, and the headers are tried in order - primary, then first extension.
fn frame_size_from(fits: &FitsHeaders) -> Option<(u32, u32)> {
    let keys = |h: &Header| {
        let w = u32::try_from(h.get_i64("IMAGEW")?).ok()?;
        let h = u32::try_from(h.get_i64("IMAGEH")?).ok()?;
        (w >= 1 && h >= 1).then_some((w, h))
    };
    keys(fits.primary()).or_else(|| fits.header_at(1).and_then(keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits::{BLOCK_SIZE, CARD_SIZE};

    /// Cards + `END`: each card padded to `CARD_SIZE`, the header to a multiple of
    /// `BLOCK_SIZE`.
    fn header_block(cards: &[String]) -> Vec<u8> {
        let mut blocks: Vec<u8> = Vec::new();
        for c in cards {
            let mut card = c.as_bytes().to_vec();
            card.resize(CARD_SIZE, b' ');
            blocks.extend_from_slice(&card);
        }
        let mut end = b"END".to_vec();
        end.resize(CARD_SIZE, b' ');
        blocks.extend_from_slice(&end);
        pad_block(&mut blocks);
        blocks
    }

    fn pad_block(data: &mut Vec<u8>) {
        let len = data.len().div_ceil(BLOCK_SIZE) * BLOCK_SIZE;
        data.resize(len, 0);
    }

    /// The minimal primary IMAGE HDU (no data) that a FITS file starts with.
    fn primary_block() -> Vec<u8> {
        primary_block_with(&[])
    }

    /// The primary IMAGE HDU with extra cards appended (e.g. `IMAGEW`/`IMAGEH`).
    fn primary_block_with(extra: &[String]) -> Vec<u8> {
        let mut cards = vec![
            "SIMPLE  =                    T".to_string(),
            "BITPIX  =                    8".to_string(),
            "NAXIS   =                    0".to_string(),
        ];
        cards.extend_from_slice(extra);
        header_block(&cards)
    }

    /// BINTABLE header cards: columns named `ttypes`, all with `TFORM = tform`,
    /// `row_len` bytes per row and `nrows` rows.
    fn table_cards(ttypes: &[&str], tform: &str, row_len: usize, nrows: usize) -> Vec<String> {
        let mut cards = vec![
            "XTENSION= 'BINTABLE'           ".to_string(),
            "BITPIX  =                    8".to_string(),
            "NAXIS   =                    2".to_string(),
            format!("NAXIS1  = {row_len:>20}"),
            format!("NAXIS2  = {nrows:>20}"),
            "PCOUNT  =                    0".to_string(),
            "GCOUNT  =                    1".to_string(),
            format!("TFIELDS = {:>20}", ttypes.len()),
        ];
        for (i, name) in ttypes.iter().enumerate() {
            cards.push(format!("TTYPE{}  = '{name:<8}'", i + 1));
            cards.push(format!("TFORM{}  = '{tform:<8}'", i + 1));
        }
        cards
    }

    /// Appends a BINTABLE HDU whose data area is `data` (padded to a block).
    fn append_table(
        buf: &mut Vec<u8>,
        ttypes: &[&str],
        tform: &str,
        row_len: usize,
        nrows: usize,
        mut data: Vec<u8>,
    ) {
        append_table_with(buf, ttypes, tform, row_len, nrows, &mut data, &[]);
    }

    /// `append_table` with extra cards in the table header.
    fn append_table_with(
        buf: &mut Vec<u8>,
        ttypes: &[&str],
        tform: &str,
        row_len: usize,
        nrows: usize,
        data: &mut Vec<u8>,
        extra: &[String],
    ) {
        let expected = row_len * nrows;
        assert_eq!(data.len(), expected, "test bug: wrong data size");
        let mut cards = table_cards(ttypes, tform, row_len, nrows);
        cards.extend_from_slice(extra);
        buf.extend_from_slice(&header_block(&cards));
        pad_block(data);
        buf.extend_from_slice(data);
    }

    /// Cards naming the frame, in the fixed-column FITS form.
    fn frame_cards(w: i64, h: i64) -> Vec<String> {
        vec![format!("IMAGEW  = {w:>20}"), format!("IMAGEH  = {h:>20}")]
    }

    fn frame_of(buf: Vec<u8>) -> Option<(u32, u32)> {
        frame_size_from(&FitsHeaders::from_bytes(buf).unwrap())
    }

    /// Builds primary + BINTABLE in memory: big-endian `1D` columns named
    /// `ttypes`, holding `rows` laid out row-major.
    fn make_table(rows: &[&[f64]], ttypes: &[&str]) -> Vec<u8> {
        let mut data: Vec<u8> = Vec::new();
        for row in rows {
            assert_eq!(row.len(), ttypes.len(), "test bug: ragged row");
            for &v in row.iter() {
                data.extend_from_slice(&v.to_be_bytes());
            }
        }
        let mut buf = primary_block();
        append_table(&mut buf, ttypes, "1D", 8 * ttypes.len(), rows.len(), data);
        buf
    }

    /// `make_table` for the X/Y(/FLUX) shape. `x_name` lets a test produce a
    /// table whose first column is not named `X`.
    fn make_fits(rows: &[(f64, f64, f64)], with_flux: bool, x_name: &str) -> Vec<u8> {
        let mut ttypes = vec![x_name, "Y"];
        if with_flux {
            ttypes.push("FLUX");
        }
        let rows: Vec<Vec<f64>> = rows
            .iter()
            .map(|&(x, y, flux)| {
                if with_flux {
                    vec![x, y, flux]
                } else {
                    vec![x, y]
                }
            })
            .collect();
        let rows: Vec<&[f64]> = rows.iter().map(Vec::as_slice).collect();
        make_table(&rows, &ttypes)
    }

    fn make_xyls(rows: &[(f64, f64, f64)], with_flux: bool) -> Vec<u8> {
        make_fits(rows, with_flux, "X")
    }

    /// A valid `X, Y` xylist with `nrows` rows in 1-byte columns, so a row count
    /// beyond the cap costs kilobytes rather than gigabytes.
    fn make_many_rows(nrows: usize) -> Vec<u8> {
        let mut data: Vec<u8> = Vec::with_capacity(2 * nrows);
        for i in 0..nrows {
            data.push((i % 251) as u8);
            data.push((i % 257) as u8);
        }
        let mut buf = primary_block();
        append_table(&mut buf, &["X", "Y"], "1B", 2, nrows, data);
        buf
    }

    /// The reader must not sort by flux: rows on a zero-height bounding box cannot
    /// be gridded, so `uniformize` returns the list unchanged and file order
    /// survives despite the non-descending fluxes. Pins `stars_from` alone.
    #[test]
    fn reads_file_order_when_uniformize_is_a_no_op() {
        let buf = make_xyls(
            &[(10.5, 20.0, 100.0), (1.0, 20.0, 900.0), (3.0, 20.0, 400.0)],
            true,
        );
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert_eq!(stars.len(), 3);
        assert_eq!(
            stars.iter().map(|s| s.flux).collect::<Vec<_>>(),
            vec![100.0, 900.0, 400.0],
            "the reader must not reorder by flux"
        );
        assert_eq!(
            stars[0],
            Star {
                x: 10.5,
                y: 20.0,
                flux: 100.0
            }
        );
        assert_eq!(stars[2].x, 3.0);
    }

    /// End to end: the reader hands back the position-binned order of the file
    /// rows (the permutation itself is pinned in `augment`'s
    /// `uniformize_reproduces_the_fixture_order`); checked here is the reader's
    /// half - `load` only, before `run_into` reorders the list brightest-first.
    #[test]
    fn loads_the_fixture_in_the_augment_order() {
        if !crate::testutil::have_file("testdata", "1920x1080-2.xyls") {
            return;
        }
        let path = crate::testutil::testdata_path("1920x1080-2.xyls");
        // The file's own row order, straight from the FITS columns.
        let fits = crate::fits::Fits::open(&path).unwrap();
        let table = fits.table(fits.find_table("X").unwrap()).unwrap();
        let cols = table.f64_columns(&["X", "Y", "FLUX"]).unwrap();
        let (fx, fy, fflux) = (&cols[0], &cols[1], &cols[2]);

        let stars = crate::xyls::load(&path).unwrap();
        assert_eq!(stars.len(), fx.len());
        // The first output positions hold these file rows, in this order.
        for (i, &row) in [0usize, 2, 6, 7, 9, 29, 68, 140, 1, 3, 11, 21]
            .iter()
            .enumerate()
        {
            assert_eq!(
                (stars[i].x, stars[i].y, stars[i].flux),
                (fx[row], fy[row], fflux[row]),
                "position {i} must hold file row {row}"
            );
        }
        // Not a brightness order: one increasing adjacent flux pair rules it out.
        assert!(
            stars.windows(2).any(|w| w[0].flux < w[1].flux),
            "the loaded list still looks flux-sorted"
        );
    }

    /// A list without a FLUX column still loads (all fluxes equal). Two stars well
    /// apart land in separate bins, so they keep their file order.
    #[test]
    fn reads_without_flux_column() {
        let buf = make_xyls(&[(7.0, 8.0, 0.0), (9.0, 10.0, 0.0)], false);
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert_eq!(stars.len(), 2);
        assert_eq!(stars[0].x, 7.0);
        assert_eq!(stars[1].x, 9.0);
        assert_eq!(stars[0].flux, 0.0);
    }

    /// A FITS table without an `X` column is not an xylist.
    #[test]
    fn rejects_missing_columns() {
        let buf = make_fits(&[(1.0, 2.0, 3.0)], true, "FOO");
        assert!(stars_from(&Fits::from_bytes(buf).unwrap()).is_err());
    }

    /// `X` in a later column is unsupported, but saying "no column named X"
    /// would be wrong: the error must name the column it was found in.
    #[test]
    fn reports_x_in_a_later_column() {
        let buf = make_table(&[&[1.0, 2.0]], &["Y", "X"]);
        let err = stars_from(&Fits::from_bytes(buf).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("found X in column 2"), "got: {err}");
        assert!(
            !err.contains("no BINTABLE column named"),
            "misleading error: {err}"
        );
        // A column that is not X at all keeps the plain "no such column" error.
        let buf = make_table(&[&[1.0, 2.0]], &["FOO", "BAR"]);
        let err = stars_from(&Fits::from_bytes(buf).unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no BINTABLE column named"), "got: {err}");
    }

    /// More rows than the cap are rejected before the column vectors are
    /// allocated, so a hostile input cannot exhaust memory.
    #[test]
    fn rejects_more_rows_than_the_cap() {
        let buf = make_many_rows(MAX_XYLS_ROWS + 1);
        let err = stars_from(&Fits::from_bytes(buf).unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("more than the supported maximum"),
            "got: {err}"
        );
        assert!(err.contains(&MAX_XYLS_ROWS.to_string()), "got: {err}");
    }

    /// Just below the cap the same shape still loads (the cap bounds absurd input,
    /// not real). Rows are position-reordered like any other list: the box spans
    /// 250 x 255 px (`i % 251` reaches 250, `as u8` of `i % 257` reaches 255, 256
    /// wrapping to 0) and the bin budget grids it 3 x 3, so each round-robin pass
    /// emits the first star of each bin (file rows 0, 84, 85, 167, 170).
    #[test]
    fn loads_many_rows_under_the_cap() {
        let buf = make_many_rows(1000);
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert_eq!(stars.len(), 1000);
        assert_eq!(
            stars.iter().take(5).map(|s| s.x).collect::<Vec<_>>(),
            vec![0.0, 84.0, 85.0, 167.0, 170.0]
        );
        // Nothing lost or duplicated by the reordering.
        let mut xs = stars.iter().map(|s| s.x).collect::<Vec<_>>();
        xs.sort_by(f64::total_cmp);
        let mut want = (0..1000usize).map(|i| (i % 251) as f64).collect::<Vec<_>>();
        want.sort_by(f64::total_cmp);
        assert_eq!(xs, want, "the reordering must not lose rows");
    }

    /// Several `X` tables are counted for the stderr warning; reading uses the first.
    #[test]
    fn counts_extra_x_tables() {
        let mut buf = primary_block();
        for x in [1.0f64, 2.0] {
            let mut data: Vec<u8> = Vec::new();
            data.extend_from_slice(&x.to_be_bytes());
            data.extend_from_slice(&10.0f64.to_be_bytes());
            append_table(&mut buf, &["X", "Y"], "1D", 16, 1, data);
        }
        let fits = Fits::from_bytes(buf).unwrap();
        assert_eq!(count_x_tables(&fits), 2);
        let stars = stars_from(&fits).unwrap();
        assert_eq!(stars.len(), 1);
        assert_eq!(stars[0].x, 1.0);
        // A one-table file is not "extra".
        let one = Fits::from_bytes(make_xyls(&[(1.0, 2.0, 3.0)], true)).unwrap();
        assert_eq!(count_x_tables(&one), 1);
    }

    /// A row with non-finite X or Y cannot take part in a fit, so it is gone; the
    /// two survivors keep their order (separate bins, ascending input index).
    #[test]
    fn drops_rows_with_non_finite_positions() {
        let buf = make_xyls(
            &[
                (1.0, 2.0, 100.0),
                (f64::NAN, 3.0, 1e9),
                (4.0, f64::INFINITY, 1e9),
                (f64::NEG_INFINITY, f64::NEG_INFINITY, 1e9),
                (5.0, 6.0, 50.0),
            ],
            true,
        );
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert_eq!(stars.len(), 2);
        assert!(
            stars
                .iter()
                .all(|s| f64::is_finite(s.x) && f64::is_finite(s.y))
        );
        assert_eq!(
            stars[0],
            Star {
                x: 1.0,
                y: 2.0,
                flux: 100.0
            }
        );
        assert_eq!(
            stars[1],
            Star {
                x: 5.0,
                y: 6.0,
                flux: 50.0
            }
        );
    }

    /// A star with a non-finite flux survives as the dimmest one. Nothing here
    /// orders by flux, so the contract is the clamp plus the position order: these
    /// four stars land in three bins (the last two share one), and a bin emits its
    /// stars in input-index order, so `uniformize` leaves the list in file order.
    #[test]
    fn non_finite_flux_is_clamped() {
        let buf = make_xyls(
            &[
                (1.0, 2.0, f64::NAN),
                (3.0, 4.0, 1.0),
                (5.0, 6.0, f64::INFINITY),
                (7.0, 8.0, -1.0),
            ],
            true,
        );
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert_eq!(
            stars.iter().map(|s| s.flux).collect::<Vec<_>>(),
            vec![f64::MIN, 1.0, f64::MIN, -1.0]
        );
        assert_eq!(
            stars.iter().map(|s| s.x).collect::<Vec<_>>(),
            vec![1.0, 3.0, 5.0, 7.0],
            "the clamped stars must keep their coordinates"
        );
    }

    /// A list whose rows are all unusable reads as empty: the caller turns that
    /// into the exit-2 input error.
    #[test]
    fn all_rows_unusable_yields_empty_list() {
        let buf = make_xyls(&[(f64::NAN, 1.0, 5.0), (2.0, f64::NAN, 5.0)], true);
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert!(stars.is_empty());
    }

    /// The primary header's `IMAGEW`/`IMAGEH` win over a pair in the extension.
    #[test]
    fn frame_size_reads_primary_keys_first() {
        let mut buf = primary_block_with(&frame_cards(4656, 3522));
        append_table_with(
            &mut buf,
            &["X", "Y"],
            "1D",
            16,
            1,
            &mut vec![0u8; 16],
            &frame_cards(1, 1),
        );
        assert_eq!(frame_of(buf), Some((4656, 3522)));
    }

    /// Without primary keys the first extension header is consulted.
    #[test]
    fn frame_size_falls_back_to_the_extension_header() {
        let mut buf = primary_block();
        append_table_with(
            &mut buf,
            &["X", "Y"],
            "1D",
            16,
            1,
            &mut vec![0u8; 16],
            &frame_cards(1920, 1080),
        );
        assert_eq!(frame_of(buf), Some((1920, 1080)));
    }

    /// No keys, a half pair or a non-positive value: no usable frame size.
    #[test]
    fn frame_size_absent_or_non_positive_is_none() {
        assert_eq!(frame_of(make_xyls(&[(1.0, 2.0, 3.0)], true)), None);
        for (w, h) in [
            (0, 1080),
            (-10, 1080),
            (1920, 0),
            // A width beyond `u32` is not a frame size we can hand back.
            (i64::from(u32::MAX) + 1, 1080),
        ] {
            let mut buf = primary_block_with(&frame_cards(w, h));
            append_table(&mut buf, &["X", "Y"], "1D", 16, 1, vec![0u8; 16]);
            assert_eq!(frame_of(buf), None, "frame ({w},{h}) must read as absent");
        }
        // A lone IMAGEW is a half pair even with a valid IMAGEH in the table.
        let mut buf = primary_block_with(&[format!("IMAGEW  = {:>20}", 1920)]);
        append_table_with(
            &mut buf,
            &["X", "Y"],
            "1D",
            16,
            1,
            &mut vec![0u8; 16],
            &[format!("IMAGEH  = {:>20}", 1080)],
        );
        assert_eq!(frame_of(buf), None, "keys must come as a pair per header");
    }

    /// The public path reads the same values from a real file on disk.
    #[test]
    fn frame_size_from_disk() {
        let mut buf = primary_block_with(&frame_cards(123, 456));
        append_table(&mut buf, &["X", "Y"], "1D", 16, 1, vec![0u8; 16]);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "starsolver_frame_size_{}_{}.fits",
            std::process::id(),
            unique
        ));
        std::fs::write(&path, &buf).unwrap();
        let got = frame_size(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(got.unwrap(), Some((123, 456)));
    }

    /// The frame check has `FRAME_MARGIN_PX` px of slack on every side and counts
    /// the rest; a star sitting exactly on the slack boundary is still inside.
    #[test]
    fn counts_out_of_frame_stars() {
        let (w, h) = (40.0, 20.0);
        let star = |x, y| Star { x, y, flux: 1.0 };
        assert_eq!(
            warn_out_of_frame(
                &[
                    star(0.0, 0.0),
                    star(-FRAME_MARGIN_PX, 10.0),
                    star(45.0, 10.0),
                    star(20.0, 25.0),
                    star(-40.0, 10.0),
                    star(46.0, 10.0),
                    star(10.0, 26.0),
                    star(10.0, -30.0),
                ],
                w,
                h
            ),
            4
        );
        // An all-in-frame list produces no warning.
        assert_eq!(
            warn_out_of_frame(&[star(0.0, 0.0), star(39.0, 19.0)], w, h),
            0
        );
    }

    /// At or below the cap nothing is dropped, the list only gets sorted: flux
    /// descending, equal fluxes in their incoming order (stable sort).
    #[test]
    fn brightest_first_sorts_without_dropping() {
        let star = |x, flux| Star { x, y: 0.0, flux };
        let out = brightest_first(
            vec![
                star(1.0, 10.0),
                star(2.0, 30.0),
                star(3.0, 20.0),
                star(4.0, 30.0),
            ],
            10,
        );
        assert_eq!(out.len(), 4, "a list below the cap keeps every star");
        assert_eq!(
            out.iter().map(|s| s.flux).collect::<Vec<_>>(),
            vec![30.0, 30.0, 20.0, 10.0]
        );
        assert_eq!(
            out.iter().map(|s| s.x).collect::<Vec<_>>(),
            vec![2.0, 4.0, 3.0, 1.0],
            "the two equal fluxes must keep their input order"
        );
    }

    /// Above the cap exactly the `cap` brightest survive, in descending order, a
    /// tie across the boundary broken by input position (stable sort).
    #[test]
    fn brightest_first_keeps_the_cap_brightest() {
        let star = |x, flux| Star { x, y: 0.0, flux };
        let out = brightest_first(
            vec![
                star(1.0, 5.0),
                star(2.0, 9.0),
                star(3.0, 7.0),
                star(4.0, 9.0),
                star(5.0, 1.0),
            ],
            3,
        );
        assert_eq!(out.len(), 3);
        assert_eq!(
            out.iter().map(|s| s.flux).collect::<Vec<_>>(),
            vec![9.0, 9.0, 7.0],
            "the dimmer stars must be the ones dropped"
        );
        assert_eq!(
            out.iter().map(|s| s.x).collect::<Vec<_>>(),
            vec![2.0, 4.0, 3.0],
            "of equal fluxes the earlier input entry wins"
        );
    }

    /// A file with no FLUX column reads as all-zero fluxes, so the cap alone
    /// decides: the first `cap` entries, in the incoming order, unchanged.
    #[test]
    fn brightest_first_of_equal_fluxes_keeps_input_order() {
        let star = |x| Star {
            x,
            y: 0.0,
            flux: 0.0,
        };
        let out = brightest_first(vec![star(1.0), star(2.0), star(3.0)], 2);
        assert_eq!(
            out.iter().map(|s| s.x).collect::<Vec<_>>(),
            vec![1.0, 2.0],
            "a stable sort of equal keys must not permute the list"
        );
    }

    /// Nothing to sort: the empty list (the caller's exit-2 case) and a single
    /// star pass through the cap untouched.
    #[test]
    fn brightest_first_passes_short_lists_through() {
        assert!(brightest_first(Vec::new(), 100).is_empty());
        let one = Star {
            x: 1.0,
            y: 2.0,
            flux: 3.0,
        };
        assert_eq!(brightest_first(vec![one], 100), vec![one]);
        // A zero cap empties the list; only `params::TOP_OBJS` keeps that unreachable.
        assert!(brightest_first(vec![one; 5], 0).is_empty());
    }
}
