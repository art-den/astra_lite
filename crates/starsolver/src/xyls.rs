//! Reading `.xyls` — the input star list.
//!
//! The star table is a BINTABLE whose first column (`TTYPE1`) is `X`, followed
//! by `Y` and optionally `FLUX`; HDU0 is the plain IMAGE header a FITS file
//! starts with, which may carry the frame-size cards. Positions are read out of
//! the columns as doubles with no re-basing or scaling applied, and any
//! single-element numeric `TFORM` is accepted, so a list stored as `1E` floats
//! or as integers reads as well as one stored as `1D`.
//!
//! A `.xyls` need not state the frame size (then the CLI has to supply it), but
//! it may carry `IMAGEW`/`IMAGEH` header keys, which [`frame_size`] reads for
//! callers that omit `-w`/`-e`.
//!
//! Rows with non-finite X/Y are dropped and non-finite FLUX values are clamped
//! (see `stars_from`); stars outside the frame (CLI or file) are only counted
//! and warned about, never removed (see `warn_out_of_frame`). The surviving list
//! is then run through the position reordering in [`crate::augment`], so the
//! caller receives that order rather than the order the rows are stored in.

use anyhow::{Context, Result, bail, ensure};
use std::path::Path;

use crate::augment;
use crate::fits::{Fits, FitsHeaders, Header, Headers, Table};

/// One detected source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    pub x: f64,
    pub y: f64,
    /// The `FLUX` column, carried through unchanged apart from the non-finite
    /// clamp in `stars_from`. The quad enumeration never reads it: what stands
    /// in for brightness there is the star's POSITION in the list (see
    /// [`crate::augment`]). The only reader is `verify::probe_stars`, which
    /// picks the pre-verification probe's detections; with no FLUX column every
    /// flux is 0.0 and that pick degenerates to list order.
    pub flux: f64,
}

/// Project decision, not a format limit: reject absurdly large xylists before
/// the derived allocations happen.
///
/// The peak at the cap is an analytic bound, not a measurement, and counts only
/// what this reader derives: the `Vec<Star>` (24 B per row, so 24 MB) plus the
/// `augment` transients it feeds - two `Vec<f64>` coordinate copies (16 MB) and
/// the two bin-sort vectors (24 B and 16 B per row, 40 MB) - about 80 MB in
/// all. The column buffers add another 24 MB but are released before
/// `preprocess` runs (see `stars_from`). Either way this stays inside the 200 MB
/// RSS budget, and the file bytes themselves are mmap'd by `Fits::open` before
/// this check, so the cap bounds the derived allocations, not the raw read.
/// Verification consumes the whole list, so an over-cap file is an error rather
/// than a silent truncation.
///
/// Future optimization: iterate the rows in a single pass instead of
/// materializing one column vector per column, which would cut the peak again.
const MAX_XYLS_ROWS: usize = 1_000_000;

/// Opens the star table: the first BINTABLE whose first column is `X`.
///
/// The table is picked by `TTYPE1` alone, the column a `.xyls` keeps `X` in. A
/// table carrying `X` further right is not something this reader can open, and
/// it gets a message saying so instead of the misleading "no BINTABLE column
/// named X" that the fallback below would produce.
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

/// Reads the star list from an already parsed FITS file.
///
/// Guarantees that every returned star has finite `x`/`y` and finite `flux`. A
/// non-finite coordinate can never match anything in the solver and the position
/// reordering expects finite X/Y, so such a row is dropped here instead of
/// turning the whole file into an error. A non-finite `FLUX` is no reason to
/// lose a detection, so that star is kept with the dimmest finite value.
///
/// A list whose rows are all unusable comes back empty; the caller reports that
/// as an input error rather than solving nothing.
///
/// The returned order is not the file's: the rows are read in file order and
/// then run through [`crate::augment::preprocess`].
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
        // Keep the star, but record a dim flux: dropping it would lose a real
        // detection whose brightness measurement alone is corrupt. Nothing orders
        // by flux, so the clamped value only has to keep the guarantee that a
        // returned flux is finite.
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
    // at the row cap they are 24 MB of otherwise overlapping peak.
    drop((data, fluxes));
    // No flux sort here: `preprocess` bins the stars on a grid over their
    // bounding box and emits them round-robin over the bins, and the depth ramp
    // reads its star windows out of that ordering.
    Ok(augment::preprocess(stars))
}

/// Slack, in px, for the out-of-frame warning. Edge detections straddle the
/// frame boundary, and a slightly mis-measured centroid is not worth an alarm;
/// diagnostic only, it never affects the solver numerics.
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
/// star list the depth ramp and verification work on, so a star that is merely
/// far from the frame stays in the input. The count itself is a diagnostic, and
/// `FRAME_MARGIN_PX` keeps ordinary edge detections from triggering the warning.
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
/// the file rows with their dense source lines filtered out and the survivors
/// interleaved over their position bins, not the order they are stored in.
pub fn load(path: &Path) -> Result<Vec<Star>> {
    let fits = Fits::open(path).with_context(|| format!("loading xylist {}", path.display()))?;
    stars_from(&fits).with_context(|| format!("reading xylist {}", path.display()))
}

/// Frame size recorded in the file: the `IMAGEW`/`IMAGEH` pair of the primary
/// header, else of the first extension header. The CLI `-w`/`-e` pair wins over
/// this - the file is consulted only when that pair is missing or incomplete,
/// never mixed with it (a lone `-w` never pairs with a file `IMAGEH`; see
/// `SolveOptions::from_args`).
///
/// Only the headers are read, never the table data, so the cost does not depend
/// on how many stars the list holds.
///
/// `Ok(None)` means "this file carries no usable frame size": no pair in either
/// header, one card in each header (they must come as a pair out of one header),
/// a value that is not an integer card value (a float-formatted `4656.0` is
/// read as a float), a size below 1, or one that does not fit `u32`. All of them
/// collapse into the caller's single "width and height must be given or stored
/// in the headers" error, which is one exit-2 message rather than one per case.
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

    /// Cards + `END`, each card space-padded to `CARD_SIZE` and the whole header
    /// padded out to a multiple of `BLOCK_SIZE`.
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

    /// A valid `X, Y` xylist with `nrows` rows in 1-byte columns, so a
    /// row count beyond the cap costs kilobytes rather than gigabytes.
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

    /// No flux sort: a row set with a zero-height bounding box cannot be put on
    /// a grid, so `uniformize` hands the list back unchanged and the file order
    /// must survive even though the fluxes are not descending.
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

    /// End to end on a sample list: the reader hands back the position-binned
    /// order of the file rows. The permutation itself is pinned in `augment`'s
    /// `uniformize_reproduces_the_fixture_order`, so what is checked here is the
    /// reader's half of that handoff.
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
        // Explicitly not a brightness order: a flux-sorted list is
        // non-increasing, so one increasing adjacent pair rules it out.
        assert!(
            stars.windows(2).any(|w| w[0].flux < w[1].flux),
            "the loaded list still looks flux-sorted"
        );
    }

    /// A list without a FLUX column still loads (all fluxes equal). Two stars
    /// well apart land in separate bins, so they keep their file order.
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

    /// The same shape just below the cap still loads (the cap is a bound on
    /// absurd input, not on real ones). The rows are position-reordered like any
    /// other list: `x = i % 251` reaches 250 and the `as u8` cast of `i % 257`
    /// reaches 255 (the value 256 wraps to 0), so the box spans 250 x 255 px and
    /// the bin budget grids it 3 x 3. Each pass of the round-robin takes one star
    /// per occupied bin, so the first positions out are the first star of each
    /// bin (file rows 0, 84, 85, 167, 170) rather than the first file rows.
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

    /// Several `X` tables are counted for the stderr warning, and reading
    /// still uses the first one.
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

    /// A row with non-finite X or Y cannot take part in a fit, so it is gone.
    /// The two survivors keep their order: they land in separate bins, and a
    /// round-robin over singleton bins emits them by ascending input index.
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

    /// A star with a non-finite flux survives as the dimmest one. Nothing orders
    /// by flux, so the only contract here is the clamp plus the position order:
    /// these four diagonal stars land in three bins (the last two share one), and
    /// a bin emits its stars in input-index order, so `uniformize` leaves the
    /// list in file order.
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
    /// into the exit-2 input error rather than solving nothing.
    #[test]
    fn all_rows_unusable_yields_empty_list() {
        let buf = make_xyls(&[(f64::NAN, 1.0, 5.0), (2.0, f64::NAN, 5.0)], true);
        let stars = stars_from(&Fits::from_bytes(buf).unwrap()).unwrap();
        assert!(stars.is_empty());
    }

    /// `IMAGEW`/`IMAGEH` in the primary header are the frame size, and the
    /// primary is looked at first: its pair wins over one in the extension.
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

    /// Without primary keys the first extension header is consulted, so a list
    /// that stores its size next to the table still works.
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

    /// No keys at all, a half pair, or a non-positive value: no frame size, so
    /// the caller's "must be specified" error fires.
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
}
