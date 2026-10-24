//! The `.xyls` preprocessing astrometry.net's `solve-field` applies before
//! solving: drop the stars that sit in a dense source line, then reorder what is
//! left so every group of stars is spread over the whole field.
//!
//! A `.xyls` list is never brightness-sorted, even though it may carry a `FLUX`
//! column: the chain starts from the order the file lists the stars in, filters
//! the dense lines, then bins the survivors on a grid over their bounding box
//! and emits them round-robin over the bins. Both steps always run; their
//! tunables (the bin budget, the line-filter significance) are frozen in
//! [`crate::params`] and our CLI exposes no switch for either.
//!
//! The reordering is a performance property, not a correctness one. The depth
//! ramp ([`crate::sweep::solve_field`]'s round loop) feeds the solver stars
//! 1-10, then 11-20, ...; `uniformize` spreads every round over the whole
//! field instead of handing it the N brightest detections, which is what lets it
//! verify a real match in the first rounds.
//!
//! Bit-exactness notes (the ordering is only useful if it reproduces numpy):
//! - rounding is half-to-even (numpy's mode), applied to BOTH bin counts
//!   (`nx` and `ny`); see [`uniformize_grid`].
//! - all arithmetic is f64 over the double-precision X/Y columns.
//! - the caller guarantees finite X/Y: [`crate::xyls::load`] already drops
//!   non-finite positions, so nothing here re-drops them.

use crate::params;
use crate::xyls::Star;

/// Runs the default `.xyls` preprocessing chain: the dense line filter, then the
/// position uniformization into [`crate::params::UNIFORMIZE_BINS`] bins. Both
/// keep the input order except for the reordering the bins impose.
pub(crate) fn preprocess(stars: Vec<Star>) -> Vec<Star> {
    uniformize(removelines(stars))
}

// ---- dense-line filter ------------------------------------------------------

/// Bin width of the line-detection histogram, in pixels.
const LINE_BIN_WIDTH: f64 = 1.0;
/// Bin offset: it moves every edge off the integers, so with the frozen width
/// bin `j` spans `[j - 0.5, j + 0.5)`.
const LINE_BIN_OFFSET: f64 = 0.5;

/// Drops stars that sit in a dense line of sources sharing a 1-pixel bin of X or
/// of Y (see [`hist_remove_lines`]); a star is kept iff it is kept in both
/// coordinates. The input order is never changed.
///
/// The bins form a contiguous run from the first edge up past the largest
/// coordinate, but only the bins holding stars are counted: that is equivalent
/// to materialising one bin per edge and keeps the memory O(stars) rather than
/// O(max coordinate) — a star at a large coordinate must not cost a vector as
/// long as the coordinate range.
fn removelines(stars: Vec<Star>) -> Vec<Star> {
    let xs: Vec<f64> = stars.iter().map(|s| s.x).collect();
    let ys: Vec<f64> = stars.iter().map(|s| s.y).collect();
    let keep_x = hist_remove_lines(&xs);
    let keep_y = hist_remove_lines(&ys);
    let keep: Vec<bool> = keep_x
        .iter()
        .zip(keep_y.iter())
        .map(|(a, b)| *a && *b)
        .collect();
    let dropped = keep.iter().filter(|&&k| !k).count();
    if dropped == 0 {
        return stars;
    }
    eprintln!("Warning: {dropped} stars in a dense source line dropped (removelines)");
    let mut out = Vec::with_capacity(stars.len() - dropped);
    for (i, star) in stars.into_iter().enumerate() {
        if keep[i] {
            out.push(star);
        }
    }
    out
}

/// Line filter for one coordinate: bin the values with the frozen binning
/// ([`LINE_BIN_WIDTH`], [`LINE_BIN_OFFSET`]), find the bins whose occupancy is
/// impossibly high for the mean occupancy across the occupied bins (a Poisson
/// tail below [`crate::params::REMOVELINES_LOGCUT`]), and drop the stars that
/// fall in such a bin. Returns one `false` per dropped value.
///
/// `v` must be finite: a NaN or infinite value has no bin (it lands outside the
/// range and is never counted), and the reader guarantees that case cannot reach
/// here — see [`crate::xyls::load`].
fn hist_remove_lines(v: &[f64]) -> Vec<bool> {
    let count = v.len();
    let mut keep = vec![true; count];
    if count == 0 {
        return keep;
    }
    let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // The binning we reproduce puts an edge every `LINE_BIN_WIDTH` from 0 up to
    // `max + LINE_BIN_WIDTH + 1`: `ceil((max + width + 1) / width)` edges, i.e.
    // one fewer bins. The literal `+1` in the stop puts the last edge at or
    // above `max + (1 - offset)`, which is above `max` for any offset < 1 (we
    // use `0.5`); so the right-inclusive last bin can never apply and bin `j` is
    // `[j*width - offset, (j+1)*width - offset)`.
    let bins = f64::ceil((max + LINE_BIN_WIDTH + 1.0) / LINE_BIN_WIDTH) - 1.0;
    // Fewer than two edges means no bins at all: `ceil(max + 2) - 1 < 1` is
    // `max <= -1` with the frozen binning (an all-negative list), so nothing is
    // occupied and every star is kept — the same answer numpy gives for an empty
    // histogram (numpy 2.3.5 does NOT raise here; see
    // `removelines_keeps_a_line_below_the_histogram_range`).
    if f64::is_nan(bins) || bins < 1.0 {
        return keep;
    }
    // The bin of a value is `floor((v + LINE_BIN_OFFSET) / LINE_BIN_WIDTH)`; a
    // value below the first edge is outside the histogram range, is never
    // counted, and can never be marked (`-1` marks those here).
    let cell = |x: f64| -> i64 {
        let j = f64::floor((x + LINE_BIN_OFFSET) / LINE_BIN_WIDTH);
        if j >= 0.0 && j < bins { j as i64 } else { -1 }
    };
    let cells: Vec<i64> = v.iter().copied().map(cell).collect();

    // The occupied bins (those holding more than one star) in ascending order.
    let mut sorted = cells.clone();
    sorted.sort_unstable();
    let mut occupied: Vec<(i64, usize)> = Vec::new();
    let mut i = 0;
    while i < count {
        let mut j = i + 1;
        while j < count && sorted[j] == sorted[i] {
            j += 1;
        }
        if sorted[i] >= 0 && j - i > 1 {
            occupied.push((sorted[i], j - i));
        }
        i = j;
    }
    if occupied.is_empty() {
        return keep;
    }
    // The mean occupancy is `sum(count - 1)` over the occupied bins divided by
    // their number; the sum is an exact integer, and far below 2^53 for any list
    // we can load, so the f64 division below is exact.
    let sum_k: i64 = occupied.iter().map(|&(_, c)| (c - 1) as i64).sum();
    let mean = sum_k as f64 / occupied.len() as f64;
    let log_mean = f64::ln(mean);
    // The log-Poisson tail of an occupancy is `k*ln(mean) - mean - k*(k-1)/2`,
    // where `k*(k-1)/2` is the exact integer sum `0 + 1 + ... + (k-1)`. The
    // evaluation order below matches numpy's left-to-right.
    let mut bad: Vec<i64> = Vec::new();
    for &(bin, c) in &occupied {
        let k = (c - 1) as i64;
        let log_fac = (k * (k - 1) / 2) as f64;
        let log_poisson = ((k as f64 * log_mean) - mean) - log_fac;
        if log_poisson < params::REMOVELINES_LOGCUT {
            bad.push(bin);
        }
    }
    if bad.is_empty() {
        return keep;
    }
    // Marking by bin index is the same as marking by the bin's value range
    // `[edge, edge + width)`: the two agree because the edges are the exact
    // half-integers `j - 0.5`.
    for (i, &bin) in cells.iter().enumerate() {
        if bin >= 0 && bad.binary_search(&bin).is_ok() {
            keep[i] = false;
        }
    }
    keep
}

// ---- uniformize -------------------------------------------------------------

/// Cap on a bin count: above it the field is degenerate (a bounding box whose
/// aspect ratio is astronomically far from 1, so the grid says nothing about the
/// field); the list is then emitted unchanged instead of binned. Real fields
/// land at `nx = 4` (`ny = 2`).
const MAX_UNIFORMIZE_GRID: f64 = 1e9;

/// Reorders `stars` by position: bin them on an `nx x ny` grid over the
/// bounding box and emit the stars round-robin over the bins; the bin budget `n`
/// is [`crate::params::UNIFORMIZE_BINS`].
fn uniformize(stars: Vec<Star>) -> Vec<Star> {
    let xs: Vec<f64> = stars.iter().map(|s| s.x).collect();
    let ys: Vec<f64> = stars.iter().map(|s| s.y).collect();
    let Some(order) = uniformize_perm(&xs, &ys, params::UNIFORMIZE_BINS) else {
        return stars;
    };
    // `Star` is `Copy`, and `uniformize_perm` returns a permutation of the input
    // indices, so the reordering is a plain gather.
    order.iter().map(|&i| stars[i]).collect()
}

/// The permutation [`uniformize`] applies, as input indices in output order, or
/// `None` when the list must be emitted unchanged: fewer than two stars, `x` and
/// `y` of unequal length, a bounding box of zero width or height, or a degenerate
/// grid (see [`MAX_UNIFORMIZE_GRID`]).
///
/// Always a permutation of `0..x.len()`; `x` and `y` must carry only finite
/// values.
fn uniformize_perm(x: &[f64], y: &[f64], n: f64) -> Option<Vec<usize>> {
    let count = x.len();
    if count < 2 || x.len() != y.len() {
        return None;
    }
    let (min_x, max_x) = min_max(x);
    let (min_y, max_y) = min_max(y);
    let (width, height) = (max_x - min_x, max_y - min_y);
    let (nx, ny) = uniformize_grid(width, height, n)?;

    // Sorting the (cell, index) pairs groups the bins and puts every bin's
    // indices in ascending order, exactly the order a single pass over the input
    // that appends each index to its bin produces. Numbering the bins
    // `iy*nx + ix` instead of this `(ix, iy)` one would not change the result
    // either: every output row is sorted by index before it is emitted (see
    // below), which makes the order the bins are walked in irrelevant.
    let mut cells: Vec<((u64, u64), usize)> = (0..count)
        .map(|j| {
            (
                (
                    bin_index(x[j], min_x, width, nx),
                    bin_index(y[j], min_y, height, ny),
                ),
                j,
            )
        })
        .collect();
    cells.sort_unstable();

    // The round-robin: for `i = 0..length of the longest bin`, emit the `i`-th
    // star of every bin with that row sorted by index. Sorting by
    // `(rank within the bin, index)` therefore yields the identical sequence, in
    // O(count log count) and with a single allocation instead of one `Vec` per
    // row (at the `MAX_XYLS_ROWS` cap a crowded bin would mean up to a million
    // one-element vectors).
    let mut keyed: Vec<(usize, usize)> = Vec::with_capacity(count);
    for bin in cells.chunk_by(|a, b| a.0 == b.0) {
        keyed.extend(bin.iter().enumerate().map(|(rank, &(_, j))| (rank, j)));
    }
    drop(cells);
    keyed.sort_unstable();
    Some(keyed.into_iter().map(|(_, j)| j).collect())
}

/// The grid dimensions of [`uniformize`]: `nx` follows from the bounding-box
/// aspect ratio, `ny` then spends the remaining budget `n / nx`. Split out so
/// the rounding of `ny` can be pinned without field data (`nx` is `4` on every
/// shipped fixture, where `round(10/4) = 2` equals the truncated `10/4`, so the
/// fixtures cannot tell half-to-even from truncation).
///
/// `None` means "emit the list unchanged": zero-width or zero-height bounding
/// box, or a degenerate grid (see [`MAX_UNIFORMIZE_GRID`]).
fn uniformize_grid(width: f64, height: f64, n: f64) -> Option<(u64, u64)> {
    if width == 0.0 || height == 0.0 {
        return None;
    }
    // Half-to-even round (numpy's rounding mode), floored at 1, then truncated
    // toward zero (it is already a whole number).
    let nx = width / f64::sqrt(width * height / n);
    let nx = f64::max(1.0, f64::round_ties_even(nx));
    if !f64::is_finite(nx) || nx > MAX_UNIFORMIZE_GRID {
        return None;
    }
    let ny = f64::max(1.0, f64::round_ties_even(n / f64::floor(nx)));
    if !f64::is_finite(ny) || ny > MAX_UNIFORMIZE_GRID {
        return None;
    }
    Some((nx as u64, ny as u64))
}

/// The bin of a value: `floor((v - min) / span * bins)` clamped into
/// `0..bins - 1`, truncated to an integer. A star at `max` lands on `bins` and
/// clamps to `bins - 1`.
fn bin_index(v: f64, min: f64, span: f64, bins: u64) -> u64 {
    let bins = bins as f64;
    let j = f64::floor((v - min) / span * bins);
    f64::clamp(j, 0.0, bins - 1.0) as u64
}

/// The minimum and the maximum of a list, in one pass (the values are finite).
fn min_max(v: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &x in v {
        if x < min {
            min = x;
        }
        if x > max {
            max = x;
        }
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn star(x: f64, y: f64) -> Star {
        Star { x, y, flux: 1.0 }
    }

    fn xs(stars: &[Star]) -> Vec<f64> {
        stars.iter().map(|s| s.x).collect()
    }

    fn ys(stars: &[Star]) -> Vec<f64> {
        stars.iter().map(|s| s.y).collect()
    }

    /// The bit-exactness guard: our order matches the augmented star list that
    /// astrometry.net's `solve-field` produces for
    /// `testdata/1920x1080-2.xyls`.
    #[test]
    fn uniformize_reproduces_the_fixture_order() {
        if !crate::testutil::have_file("testdata", "1920x1080-2.xyls") {
            return;
        }
        let path = crate::testutil::testdata_path("1920x1080-2.xyls");
        let fits = crate::fits::Fits::open(&path).unwrap();
        let table = fits.table(fits.find_table("X").unwrap()).unwrap();
        let cols = table.f64_columns(&["X", "Y"]).unwrap();
        let (x, y) = (&cols[0], &cols[1]);
        assert_eq!(x.len(), 285, "the fixture has 285 rows");

        let (min_x, max_x) = min_max(x);
        let (min_y, max_y) = min_max(y);
        let (nx, ny) = uniformize_grid(max_x - min_x, max_y - min_y, params::UNIFORMIZE_BINS)
            .expect("the fixture encloses an area");
        assert_eq!((nx, ny), (4, 2), "the fixture uniformizes into 4 x 2 bins");

        let order = uniformize_perm(x, y, params::UNIFORMIZE_BINS).unwrap();
        assert_eq!(
            &order[..12],
            &[0, 2, 6, 7, 9, 29, 68, 140, 1, 3, 11, 21],
            "the round-robin order diverged from the reference solver's order"
        );
        // The FULL permutation, pinned compactly as FNV-1a-64 over every index
        // (8 little-endian bytes each), computed by running astrometry.net's
        // solve-field preprocessing verbatim on this fixture. The 12 positions
        // above cannot catch a divergence in the later rounds.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for i in &order {
            for byte in (*i as u64).to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
        assert_eq!(
            hash, 0x74a4_5d51_61a7_8a1a,
            "the permutation diverged from the reference solver's order"
        );
        // A permutation of every input row, once each.
        assert_eq!(order.len(), x.len());
        let mut seen = vec![false; order.len()];
        for &i in &order {
            assert!(i < order.len() && !seen[i], "not a permutation: {i}");
            seen[i] = true;
        }
    }

    /// `ny` must be rounded half-to-even, not truncated. With the frozen `n = 10`
    /// no integer `nx` can put `n / nx` on an odd `.5` (the only divisors of 20
    /// with an odd half are `nx = 4` -> `2.5` and `nx = 20` -> `0.5`, where
    /// both rules agree), so no fixture or realistic field can catch a missing
    /// `round` on `ny`; this pins it directly through a synthetic `n`.
    #[test]
    fn grid_rounds_both_counts_half_to_even() {
        // ny: n=14, w=40, h=35 -> nx = round(40/sqrt(100)) = 4 exactly, so
        // n/nx = 3.5, and round(3.5) = 4 half-to-even where truncation gives 3.
        assert_eq!(uniformize_grid(40.0, 35.0, 14.0), Some((4, 4)));
        // nx: w=49, h=40, n=10 -> 49/sqrt(196) = 3.5 -> round(3.5) = 4, not 3;
        // ny is then round(10/4) = round(2.5) = 2, not 3.
        assert_eq!(uniformize_grid(49.0, 40.0, 10.0), Some((4, 2)));
        // A tie that rounds DOWN: 10/sqrt(160/10) = 2.5 -> nx = 2, ny = 5.
        assert_eq!(uniformize_grid(10.0, 16.0, 10.0), Some((2, 5)));
        // Half-to-even is not "round to the lower even": 9/sqrt(36) = 1.5 -> 2.
        assert_eq!(uniformize_grid(9.0, 40.0, 10.0), Some((2, 5)));
        // 1/sqrt(4) = 0.5 -> 0, then the `max(1, ...)` floor lifts it to 1.
        assert_eq!(uniformize_grid(1.0, 40.0, 10.0), Some((1, 10)));
    }

    /// A zero-extent bounding box is emitted unchanged, which is what makes a
    /// column of stars a no-op for `uniformize`.
    #[test]
    fn grid_of_a_degenerate_box_is_identity() {
        assert_eq!(uniformize_grid(0.0, 12.0, 10.0), None);
        assert_eq!(uniformize_grid(12.0, 0.0, 10.0), None);
        let column = [star(5.0, 1.0), star(5.0, 2.0), star(5.0, 3.0)];
        assert!(uniformize_perm(&xs(&column), &ys(&column), 10.0).is_none());
        // ... and the reordering really leaves it alone.
        let out = uniformize(column.to_vec());
        assert_eq!(out, column.to_vec());
    }

    /// Round-robin over bins: the first output row is one star per occupied bin
    /// (ascending input index), then the second star of each bin, and so on.
    #[test]
    fn uniformize_interleaves_the_bins() {
        // Four bins on a 2x2 grid, three stars packed in the lower-left one.
        let stars = vec![
            star(0.0, 0.0),
            star(0.1, 0.1),
            star(0.0, 0.2),
            star(9.0, 0.0),
            star(9.0, 9.0),
            star(0.0, 9.0),
        ];
        let order = uniformize_perm(&xs(&stars), &ys(&stars), 4.0).unwrap();
        // Every star exactly once, and the sparse bins lead (they hold the i=0
        // star of their bin) while the crowded bin contributes its 2nd and 3rd.
        assert_eq!(order.len(), stars.len());
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(order[..2], vec![0, 3]);
        // Applying the permutation to the list gives the same order back.
        assert_eq!(order, vec![0, 3, 4, 5, 1, 2]);
        let out = uniformize(stars.clone());
        assert_eq!(xs(&out), vec![0.0, 9.0, 9.0, 0.0, 0.1, 0.0]);
        assert_eq!(ys(&out), vec![0.0, 0.0, 9.0, 9.0, 0.1, 0.2]);
    }

    /// [`hist_remove_lines`] is a no-op while no bin is anomalously crowded:
    /// distinct integer coordinates put one star per bin, and a bin holding a
    /// single star is not even counted as occupied; a modest crowd is well within
    /// the Poisson tail.
    #[test]
    fn removelines_keeps_ordinary_fields() {
        let spread: Vec<Star> = (0..40)
            .map(|i| star(i as f64, (i * 3) as f64 % 40.0))
            .collect();
        assert_eq!(removelines(spread.clone()), spread);
        // A dozen stars sharing one coordinate are still ordinary: the Poisson
        // tail is 11*ln(11) - 11 - 55 = -39.6, far above the -100 cut.
        let mut crowded: Vec<Star> = (0..12).map(|_| star(7.0, 1.0)).collect();
        crowded.extend((20..40).map(|i| star(i as f64, (i * 7) as f64 % 20.0)));
        assert_eq!(removelines(crowded.clone()), crowded);
    }

    /// A line of dozens of sources on one integer coordinate is what the dense
    /// line filter exists for: they are dropped and the surviving order is
    /// untouched.
    #[test]
    fn removelines_drops_a_dense_line() {
        let mut stars: Vec<Star> = (0..30).map(|_| star(500.4, 700.0)).collect();
        stars.extend((0..60).map(|i| star(i as f64, i as f64)));
        let out = removelines(stars);
        assert_eq!(out.len(), 60, "the dense line must be dropped");
        assert_eq!(xs(&out), ys(&out), "survivors must be the diagonal");
        // Order preserved among the survivors.
        for pair in out.windows(2) {
            assert!(pair[0].x < pair[1].x);
        }
    }

    /// A star is dropped if EITHER coordinate is bad (the two coordinates' keeps
    /// are intersected), so a dense run in X and a dense run in Y both go.
    #[test]
    fn removelines_marks_either_coordinate() {
        let mut stars: Vec<Star> = (0..30).map(|i| star(500.4, i as f64)).collect();
        stars.extend((0..60).map(|i| star(1000.0 + i as f64, 2000.0 + i as f64)));
        stars.extend((0..30).map(|i| star(3000.0 + i as f64, 3000.4)));
        let out = removelines(stars);
        assert_eq!(
            out.len(),
            60,
            "both dense runs must go, the spread must stay"
        );
        assert_eq!(xs(&out).first().copied(), Some(1000.0));
        assert_eq!(xs(&out).last().copied(), Some(1059.0));
    }

    /// The bins start at the first histogram edge, `-LINE_BIN_OFFSET`: a crowd
    /// inside the range is dropped, a crowd below the edge is outside the
    /// histogram (never counted) and is kept rather than dropped.
    #[test]
    fn removelines_ignores_values_below_the_first_edge() {
        let spread_y = |i: usize| (i as f64) * 2.0;
        // Bin 0 spans [-0.5, 0.5), so x = -0.4 is inside the range: dropped.
        let mut stars: Vec<Star> = (0..30).map(|i| star(-0.4, spread_y(i))).collect();
        stars.extend((30..90).map(|i| star(i as f64 - 29.0, spread_y(i))));
        assert_eq!(removelines(stars).len(), 60);
        // x = -0.6 is below the first edge: never counted, never removed.
        let mut stars: Vec<Star> = (0..30).map(|i| star(-0.6, spread_y(i))).collect();
        stars.extend((30..90).map(|i| star(i as f64 - 29.0, spread_y(i))));
        assert_eq!(removelines(stars).len(), 90);
    }

    /// A value sitting exactly on a bin edge belongs to the bin that STARTS
    /// there: the edges are the half-integers `j - LINE_BIN_OFFSET`, and the
    /// `floor` in [`hist_remove_lines`] puts such a value in the upper bin.
    #[test]
    fn removelines_bin_edge_belongs_to_the_upper_bin() {
        // Half the crowd at 500.4, half at 500.5: 500.5 is the edge between bins
        // 500 and 501, so they land in DIFFERENT bins and neither occupancy is
        // anomalous (k = 14, log-Poisson -68 > -100) -> all kept. Merged into
        // one bin they would be k = 29, log-Poisson -337 -> all dropped.
        let mut stars: Vec<Star> = (0..15).map(|i| star(500.4, i as f64)).collect();
        stars.extend((0..15).map(|i| star(500.5, i as f64)));
        assert_eq!(removelines(stars.clone()), stars);
        let mut merged = stars;
        for s in merged.iter_mut() {
            s.x = 500.5;
        }
        assert_eq!(removelines(merged).len(), 0, "30 in one bin is a line");
    }

    /// An all-negative list leaves the histogram with fewer than two edges, so
    /// there are no bins at all, no bin is occupied, and every star is kept (the
    /// same answer numpy 2.3.5 gives, which does NOT raise here). Thus a dense
    /// line at a negative coordinate survives.
    #[test]
    fn removelines_keeps_a_line_below_the_histogram_range() {
        let stars: Vec<Star> = (0..30).map(|i| star(-500.5, -400.5 - i as f64)).collect();
        assert_eq!(removelines(stars.clone()), stars);
    }

    /// The two "emit the list unchanged" branches of `uniformize`: fewer than
    /// two stars, and a grid too degenerate to bin.
    #[test]
    fn uniformize_degenerate_inputs() {
        assert!(uniformize_perm(&[1.0], &[2.0], 10.0).is_none());
        // A sane extreme aspect ratio still grids (nx ~3.16e6, ny clamped to 1).
        assert_eq!(uniformize_grid(1e12, 1.0, 10.0), Some((3_162_278, 1)));
        // `nx` overflowing to infinity is the [`MAX_UNIFORMIZE_GRID`] arm (here
        // `w*h` itself is a plain 1.0; the division is what overflows).
        assert_eq!(uniformize_grid(1e308, 1e-308, 10.0), None);
    }

    /// Preprocessing keeps the list length and never reorders a single star.
    #[test]
    fn preprocess_is_a_permutation() {
        let stars: Vec<Star> = (0..50)
            .map(|i| star((i * 37) as f64 % 100.0, (i * 53) as f64 % 100.0))
            .collect();
        let out = preprocess(stars.clone());
        assert_eq!(out.len(), stars.len());
        let mut got: Vec<[f64; 2]> = out.iter().map(|s| [s.x, s.y]).collect();
        let mut want: Vec<[f64; 2]> = stars.iter().map(|s| [s.x, s.y]).collect();
        let by_pos = |a: &[f64; 2], b: &[f64; 2]| {
            f64::total_cmp(&a[0], &b[0]).then(f64::total_cmp(&a[1], &b[1]))
        };
        got.sort_by(by_pos);
        want.sort_by(by_pos);
        assert_eq!(got, want);
    }
}
