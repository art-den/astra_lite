//! Synthetic fields with exact sky truth, for the test suite.
//!
//! An installed index' reference stars are a real pattern whose sky positions the
//! solver can read back (`Index::star_xyz`), so projecting a subset through a WCS
//! *chosen here* makes the truth exact by construction: center, plate scale,
//! orientation and parity are the projection's inputs - unlike the shipped
//! `testdata/` fields, whose truth is a noisy reference run.
//!
//! Two traps this module exists to avoid, both measured:
//!
//! * TAN is defined only within 90 degrees of the tangent point; beyond it a star
//!   lands back in the frame as a *mirrored* pattern of the antipodal sky, so such
//!   a field really has a second solution half a sphere away. Hence
//!   `MAX_PROJECTION_ANGLE_DEG`, which keeps the truth unique.
//! * The source index must be one the sweep tries at the field's plate scale, or
//!   the field never solves: [`random_params`] takes the scale from the index'
//!   `SCALE_L`/`SCALE_U` band and checks it with `IndexMeta::fits_scale`.
//!
//! Plain data (`PathBuf`, `f64`, `Vec<[f64; 2]>`) throughout, so this module can
//! be published to `tests/` (non-default `test-util` feature) with every pipeline
//! module private; the in-memory star-list helpers (`generate_stars`,
//! `noise_stars`, `wcs_of`) are `cfg(test)` only - outside, go through
//! [`write_xyls`] and the public solve API.

use anyhow::{Result, bail, ensure};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::coords::{deg2rad, radec_to_xyz, xyz_to_radec};
use crate::fits::{BLOCK_SIZE, CARD_SIZE};
use crate::index::{Index, IndexMeta, Unusable, load_metas_in};
use crate::params::MAX_OBJS;
use crate::wcs::{Wcs, pixel_center_of_size};
// Only the in-memory star-list helpers need these; `xyls::load` reorders for the file path.
#[cfg(test)]
use crate::{augment, xyls::Star};

/// Gate budgets of a generated field, shared by the unit legs and the integration
/// target so the two cannot drift apart: center in pixels of the field's own
/// scale, rotation in degrees, sizes relative. A clean generated field's pattern
/// is the catalog itself, so its residual is solver noise only: these are a drift
/// alarm, not a precision claim - still far tighter than the shipped fields'.
pub const TOL_CENTER_PX: f64 = 0.2;
pub const TOL_ORIENT_DEG: f64 = 0.05;
pub const TOL_SIZE_FRAC: f64 = 0.001;

/// Ceiling on a field's angular span from the tangent point, degrees: inside it
/// TAN is injective and no real field is affected, so it only trims the antipodal
/// contamination of the module docs.
const MAX_PROJECTION_ANGLE_DEG: f64 = 60.0;
/// Floor on the sky radius the selection pass looks over, degrees, so a tiny
/// narrow field is trimmed by the frame rather than by the cut.
const MIN_SELECTION_RADIUS_DEG: f64 = 5.0;
/// Fewest index stars a field must contain for [`random_params`] to accept a
/// placement: below it the field is too sparse to verify.
const MIN_SOURCE_STARS: usize = 12;
/// Placements [`random_params`] tries before giving up on an index.
const MAX_PLACEMENT_TRIES: usize = 24;
/// Star count [`random_params`] caps a field at: below `MAX_OBJS` the depth ramp
/// reaches the whole list on direct test paths; above
/// [`crate::params::TOP_OBJS`] `run_into` cuts to the brightest on the binary path.
const DEFAULT_MAX_STARS: usize = 120;
/// Plate scale bounds, arcsec/pixel, that [`random_params`] will hand back.
const MIN_SCALE_ARCSEC: f64 = 0.05;
const MAX_SCALE_ARCSEC: f64 = 60.0;
/// The index quad size that sets the plate scale, as a fraction of the frame
/// diagonal: half of it, so the quads span much of the field, not a corner.
const QUAD_OVER_FRAME: f64 = 0.5;
/// Half-width of the [`scale_window`] window as a fraction of the scale: wide
/// enough that the generating index survives the sweep's scale selection
/// (`random_params` guarantees one exists), narrow enough to try few indices.
const SCALE_WINDOW_FRAC: f64 = 0.02;

/// What field to generate. Everything is explicit, so a hand-built `Params` gives
/// the same field from the same `index` file on any machine.
#[derive(Debug, Clone)]
pub struct Params {
    /// Index to take reference stars from; one the sweep will try at
    /// `scale_arcsec` (see [`random_params`]).
    pub index: PathBuf,
    /// Seed of every choice [`generate`] makes: subsample, jitter, distractors,
    /// list order. [`random_params`] draws center, orientation and parity itself
    /// and stores the derived seed, so changing this re-rolls the noise, not the sky.
    pub seed: u64,
    /// Truth field center, degrees; must lie inside the index' coverage, which
    /// [`random_params`] guarantees by picking it from one of the index' stars.
    pub ra_deg: f64,
    pub dec_deg: f64,
    /// Frame size, pixels.
    pub width_pix: f64,
    pub height_pix: f64,
    /// Truth plate scale, arcsec/pixel.
    pub scale_arcsec: f64,
    /// Truth `Field rotation angle` (the sky PA of pixel +y), degrees.
    pub orient_deg: f64,
    /// Truth `Field parity`: `true` prints `neg`.
    pub parity_neg: bool,
    /// Sigma of the centroid noise on every source star, pixels; `0.0` leaves the
    /// pattern exact, matching the index star for star.
    pub jitter_px: f64,
    /// Uniform-random stars added on top of the sources: the bad-pixel and spike
    /// model the verifier's distractor handling exists for. They join the list the
    /// depth ramp walks, so a large `extra` pushes its tail past `MAX_OBJS`.
    pub extra: usize,
    /// Cap on the source stars kept (random subsample); `0` keeps them all. Above
    /// `MAX_OBJS` the depth ramp would never reach the tail.
    pub max_stars: usize,
}

/// The sky truth a field was built from: the printed numbers plus the star-list counts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Truth {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub width_deg: f64,
    pub height_deg: f64,
    pub orient_deg: f64,
    pub parity_neg: bool,
    pub scale_arcsec: f64,
    /// Index stars kept, before jitter/reordering: the upper bound on what can match.
    pub n_source: usize,
    /// Uniform-random stars added after the sources.
    pub n_extra: usize,
}

/// A generated field: truth plus pixels in the generator's own order (a
/// projection is shuffled, a pure-distractor frame is not). The order the solver
/// sees comes later, from `augment::preprocess` or `xyls::load`.
#[derive(Debug, Clone)]
pub struct Field {
    pub truth: Truth,
    pub stars: Vec<[f64; 2]>,
}

/// Deterministic PRNG (splitmix64): the frozen dependency set leaves the suite
/// its own generator rather than `rand`; only reproducibility from a seed matters.
#[derive(Debug, Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in [0, 1) from the 53 high bits of a draw; splitmix64's low bits are
    /// the weakest.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }

    /// An index into `0..n` (`n` must not be zero); modulo bias is irrelevant here.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }

    /// Standard normal (Box-Muller); the second root is dropped on purpose, `max`
    /// keeps `ln` finite for a draw whose high 53 bits are all zero.
    fn normal(&mut self) -> f64 {
        let u = f64::max(self.unit(), f64::MIN_POSITIVE);
        let v = self.unit();
        f64::sqrt(-2.0 * f64::ln(u)) * f64::cos(2.0 * std::f64::consts::PI * v)
    }
}

/// CD matrix of a truth WCS with the requested printed orientation and parity.
/// Both branches are the inverse of what `Wcs` reports (`parity_is_neg` is
/// `det(CD) > 0`, `orientation_deg` folds the parity sign into its `atan2`); pinned by
/// `generated_truth_round_trips_through_the_wcs_conventions`.
fn truth_cd(scale_deg: f64, orient_deg: f64, parity_neg: bool) -> [[f64; 2]; 2] {
    if parity_neg {
        // det = +scale^2: the rotation branch of the fit.
        let a = deg2rad(-orient_deg);
        let (s, c) = (f64::sin(a), f64::cos(a));
        [
            [scale_deg * c, -scale_deg * s],
            [scale_deg * s, scale_deg * c],
        ]
    } else {
        // det = -scale^2: the reflection branch. The +180 keeps `orientation_deg`
        // equal to `orient_deg` for a mirrored CD.
        let a = deg2rad(180.0 - orient_deg);
        let (s, c) = (f64::sin(a), f64::cos(a));
        [
            [scale_deg * c, scale_deg * s],
            [scale_deg * s, -scale_deg * c],
        ]
    }
}

/// The truth WCS of `t`: tangent point at the printed field center, where the
/// success block reports from (`Wcs::recentered_at` of [`pixel_center_of_size`]).
#[cfg(test)]
#[must_use]
pub(crate) fn wcs_of(t: &Truth) -> Wcs {
    Wcs {
        crpix: [
            pixel_center_of_size(t.width_deg * 3600.0 / t.scale_arcsec),
            pixel_center_of_size(t.height_deg * 3600.0 / t.scale_arcsec),
        ],
        crval_deg: [t.ra_deg, t.dec_deg],
        cd: truth_cd(t.scale_arcsec / 3600.0, t.orient_deg, t.parity_neg),
    }
}

/// The truth WCS of a `Params` (frame size from the params, not a `Truth`).
fn truth_wcs(p: &Params) -> Wcs {
    let s = p.scale_arcsec / 3600.0;
    Wcs {
        crpix: [
            pixel_center_of_size(p.width_pix),
            pixel_center_of_size(p.height_pix),
        ],
        crval_deg: [p.ra_deg, p.dec_deg],
        cd: truth_cd(s, p.orient_deg, p.parity_neg),
    }
}

/// Validates hand-built parameters, so a bad one fails here rather than as a
/// field that mysteriously never solves.
fn check_params(p: &Params) -> Result<()> {
    ensure!(p.width_pix >= 1.0, "width_pix is {0}", p.width_pix);
    ensure!(p.height_pix >= 1.0, "height_pix is {0}", p.height_pix);
    ensure!(
        p.scale_arcsec.is_finite() && p.scale_arcsec > 0.0,
        "scale_arcsec must be finite and positive, got {}",
        p.scale_arcsec
    );
    ensure!(
        p.ra_deg.is_finite() && (-90.0..=90.0).contains(&p.dec_deg),
        "center (RA,Dec) = ({}, {}) is not usable",
        p.ra_deg,
        p.dec_deg
    );
    ensure!(
        p.jitter_px.is_finite() && p.jitter_px >= 0.0,
        "jitter_px must be finite and non-negative, got {}",
        p.jitter_px
    );
    ensure!(
        p.max_stars <= MAX_OBJS || p.max_stars == 0,
        "max_stars {} is above the depth ramp cap MAX_OBJS {MAX_OBJS}",
        p.max_stars
    );
    Ok(())
}

/// The field `p` describes from an already-loaded index: index stars inside the
/// projection cut that land in the frame, subsampled to `max_stars`, jittered,
/// extended with `extra` distractors, then shuffled (so list order means nothing).
fn project(index: &Index, p: &Params) -> Field {
    let wcs = truth_wcs(p);
    let half_diag_deg = f64::hypot(p.width_pix, p.height_pix) * p.scale_arcsec / 7200.0;
    // Look out to twice the frame's half-diagonal so the frame fills from a wider
    // pattern, clamped by the tiny-field floor and the antipodal ceiling.
    let cut_deg = f64::clamp(
        2.0 * half_diag_deg,
        MIN_SELECTION_RADIUS_DEG,
        MAX_PROJECTION_ANGLE_DEG,
    );
    // The cut is a dot-product test against cos(cut), no trig per star.
    let center = radec_to_xyz(p.ra_deg, p.dec_deg);
    let cos_cut = f64::cos(deg2rad(cut_deg));

    let mut kept: Vec<[f64; 2]> = Vec::new();
    for i in 0..index.meta.nstars {
        let v = index.star_xyz(i);
        let cos_ang = v[0] * center[0] + v[1] * center[1] + v[2] * center[2];
        if cos_ang < cos_cut {
            continue;
        }
        let (ra, dec) = xyz_to_radec(v);
        let (x, y) = wcs.radec_to_pix(ra, dec);
        if x >= 0.0 && x < p.width_pix && y >= 0.0 && y < p.height_pix {
            kept.push([x, y]);
        }
    }

    let mut rng = Rng::new(p.seed);
    // Subsample before the noise, so `max_stars` bounds the ramp's work, not ours.
    if p.max_stars != 0 && kept.len() > p.max_stars {
        // Partial Fisher-Yates: a shuffle's first slots are a uniform sample
        // without replacement.
        let take = p.max_stars;
        let n = kept.len();
        for i in 0..take {
            let j = i + rng.below(n - i);
            kept.swap(i, j);
        }
        kept.truncate(take);
    }
    if p.jitter_px > 0.0 {
        for s in &mut kept {
            s[0] += p.jitter_px * rng.normal();
            s[1] += p.jitter_px * rng.normal();
        }
    }
    // Counted after subsample and jitter: `n_source` is what can match, the noisy gate's divisor.
    let n_source = kept.len();
    for _ in 0..p.extra {
        kept.push([rng.range(0.0, p.width_pix), rng.range(0.0, p.height_pix)]);
    }
    // The list order is the depth ramp's star ranking, and a real detection list is
    // not ordered by sky position: shuffle, so the truth is the pattern alone.
    for i in (1..kept.len()).rev() {
        let j = rng.below(i + 1);
        kept.swap(i, j);
    }

    let truth = Truth {
        ra_deg: p.ra_deg,
        dec_deg: p.dec_deg,
        width_deg: p.width_pix * p.scale_arcsec / 3600.0,
        height_deg: p.height_pix * p.scale_arcsec / 3600.0,
        orient_deg: p.orient_deg,
        parity_neg: p.parity_neg,
        scale_arcsec: p.scale_arcsec,
        n_source,
        n_extra: p.extra,
    };
    Field { truth, stars: kept }
}

/// Generates the field `p` describes.
pub fn generate(p: &Params) -> Result<Field> {
    check_params(p)?;
    let index = Index::load(&p.index)?;
    Ok(project(&index, p))
}

/// The field's star list as the sweep wants it: through the same
/// [`augment::preprocess`] `xyls::load` applies, so a unit gate drives
/// `sweep::solve_field` with exactly what a `.xyls` of these stars would give.
#[cfg(test)]
pub(crate) fn generate_stars(p: &Params) -> Result<(Vec<Star>, Truth)> {
    let field = generate(p)?;
    Ok((field_stars(&field), field.truth))
}

/// A field of pure distractors: `n_stars` uniform-random pixels, nothing from any
/// index. The false-positive gate's input - a list the odds scan must not talk
/// into a solution. Its `Truth` has no usable sky: only `n_extra` and the frame
/// size mean anything, the rest are placeholders.
pub fn noise_field(
    width_pix: f64,
    height_pix: f64,
    scale_arcsec: f64,
    n_stars: usize,
    seed: u64,
) -> Field {
    let mut rng = Rng::new(seed);
    let stars = (0..n_stars)
        .map(|_| [rng.range(0.0, width_pix), rng.range(0.0, height_pix)])
        .collect();
    Field {
        truth: Truth {
            ra_deg: 0.0,
            dec_deg: 0.0,
            width_deg: width_pix * scale_arcsec / 3600.0,
            height_deg: height_pix * scale_arcsec / 3600.0,
            orient_deg: 0.0,
            parity_neg: true,
            scale_arcsec,
            n_source: 0,
            n_extra: n_stars,
        },
        stars,
    }
}

/// [`noise_field`] as a star list, for the in-memory unit gate.
#[cfg(test)]
pub(crate) fn noise_stars(
    width_pix: f64,
    height_pix: f64,
    scale_arcsec: f64,
    n_stars: usize,
    seed: u64,
) -> (Vec<Star>, Truth) {
    let field = noise_field(width_pix, height_pix, scale_arcsec, n_stars, seed);
    (field_stars(&field), field.truth)
}

/// The brightness of list entry `i` of `n`: a falling ramp, like a real detection
/// list. Brightness carries no weight in the pipeline (see `crate::xyls::Star`);
/// `written_xyls_read_back_through_the_reader` compares it across both producers.
fn flux_of(i: usize, n: usize) -> f64 {
    1e6 * (1.0 - i as f64 / f64::max(n as f64, 1.0))
}

/// The pixel list as `Star`s with a brightness ramp, through the reader's reordering.
#[cfg(test)]
fn field_stars(field: &Field) -> Vec<Star> {
    let stars = field
        .stars
        .iter()
        .enumerate()
        .map(|(i, [x, y])| Star {
            x: *x,
            y: *y,
            flux: flux_of(i, field.stars.len()),
        })
        .collect();
    augment::preprocess(stars)
}

/// The plate scale that puts the index' quad band across the frame: band middle
/// over `QUAD_OVER_FRAME` of the diagonal; `None` if there is no usable band.
fn scale_for_index(meta: &IndexMeta, width: f64, height: f64) -> Option<f64> {
    let (band_lo, band_hi) = (meta.scale_lower_arcsec, meta.scale_upper_arcsec);
    if !band_lo.is_finite() || !band_hi.is_finite() || band_lo <= 0.0 || band_hi < band_lo {
        return None;
    }
    let diag = f64::hypot(width, height);
    let mid_quad = f64::sqrt(band_lo * band_hi);
    let s = mid_quad / (QUAD_OVER_FRAME * diag);
    if !(MIN_SCALE_ARCSEC..=MAX_SCALE_ARCSEC).contains(&s) {
        return None;
    }
    // Usable only if the sweep's own selection keeps this index under that window.
    let (lo, hi) = scale_window(s);
    if !meta.fits_scale(lo, hi, width, height) {
        return None;
    }
    Some(s)
}

/// A `Params` for a random field of `width_pix` x `height_pix` in `index`: center
/// at a star of that index (hence inside its coverage), scale in the index' band,
/// uniform orientation, random parity; the first placement with at least
/// `MIN_SOURCE_STARS` source stars. Every choice comes from `seed`.
pub fn random_params(index: &Path, seed: u64, width_pix: f64, height_pix: f64) -> Result<Params> {
    let loaded = Index::load(index)?;
    ensure!(loaded.meta.nstars > 0, "{:?} has no stars", index);
    let Some(scale_arcsec) = scale_for_index(&loaded.meta, width_pix, height_pix) else {
        let band = (
            loaded.meta.scale_lower_arcsec,
            loaded.meta.scale_upper_arcsec,
        );
        bail!(
            "no usable plate scale for a {width_pix:.0}x{height_pix:.0} px field in {}: the index \
             quad band is {band:?} arcsec and the scale window is {MIN_SCALE_ARCSEC}..\
             {MAX_SCALE_ARCSEC} arcsec/pixel",
            index.display()
        );
    };
    let mut rng = Rng::new(seed);
    for attempt in 0..MAX_PLACEMENT_TRIES {
        let (ra, dec) = xyz_to_radec(loaded.star_xyz(rng.below(loaded.meta.nstars)));
        let p = Params {
            index: index.to_path_buf(),
            // Derived and distinct per attempt: `generate` re-rolls this seed, so a
            // retry that re-rolled nothing would redraw the same field and loop for
            // nothing. Any odd multiplier would do; this one spreads the bits well.
            seed: seed.wrapping_mul(1_000_003).wrapping_add(attempt as u64),
            ra_deg: ra,
            dec_deg: dec,
            width_pix,
            height_pix,
            scale_arcsec,
            orient_deg: rng.range(-180.0, 180.0),
            parity_neg: rng.next_u64() & 1 == 0,
            jitter_px: 0.0,
            extra: 0,
            max_stars: DEFAULT_MAX_STARS,
        };
        if project(&loaded, &p).truth.n_source >= MIN_SOURCE_STARS {
            return Ok(p);
        }
    }
    bail!(
        "every one of {MAX_PLACEMENT_TRIES} placements in {} had fewer than {MIN_SOURCE_STARS} \
         stars in a {width_pix:.0}x{height_pix:.0} px field at {scale_arcsec:.3} arcsec/pixel",
        index.display()
    )
}

/// The `--scale-low/--scale-high` pair (arcsec/pixel) to sweep a field of this
/// scale with, so its generating index is tried. See `SCALE_WINDOW_FRAC`.
#[must_use]
pub fn scale_window(scale_arcsec: f64) -> (f64, f64) {
    (
        scale_arcsec * (1.0 - SCALE_WINDOW_FRAC),
        scale_arcsec * (1.0 + SCALE_WINDOW_FRAC),
    )
}

/// An index file of `dir` to generate fields from: see `default_index_from`.
pub fn default_index(dir: &Path) -> Result<PathBuf> {
    default_index_from(&load_metas_in(&[dir.to_path_buf()], Unusable::Skip)?)
}

/// The index of `metas` to generate fields from: the sub-region tile with the most
/// stars, else the largest index whatever its coverage - a dense tile's pattern is
/// local, so the field needs few stars and the file stays small.
/// Deterministic: ties break on the path.
pub(crate) fn default_index_from(metas: &[IndexMeta]) -> Result<PathBuf> {
    let best = metas
        .iter()
        .filter(|m| m.nstars > 0)
        .min_by(|a, b| {
            // Tiles first (a lower `healpix` value is not "smaller", so rank on
            // the flag), then the most stars, then the path for a stable pick.
            (a.healpix < 0)
                .cmp(&(b.healpix < 0))
                .then(b.nstars.cmp(&a.nstars).then_with(|| a.path.cmp(&b.path)))
        })
        .ok_or_else(|| anyhow::anyhow!("no usable index file among {} metas", metas.len()))?;
    Ok(best.path.clone())
}

/// A pseudo-random index file of `dir`, drawn from `seed`: spreads a gate's fields
/// over the installed set. Deterministic - same `seed` over the same set, same
/// file; a draw, not a shuffle, so two seeds can land on one file.
pub fn random_index(dir: &Path, seed: u64) -> Result<PathBuf> {
    let metas = load_metas_in(&[dir.to_path_buf()], Unusable::Skip)?;
    random_index_from(&metas, seed)
        .map(|m| m.path.clone())
        .ok_or_else(|| anyhow::anyhow!("no usable index file in {}", dir.display()))
}

/// [`random_index`] over already-scanned metas. Sorted by path before the draw, so
/// the pick does not depend on the directory scan's order.
pub(crate) fn random_index_from(metas: &[IndexMeta], seed: u64) -> Option<&IndexMeta> {
    let mut usable: Vec<&IndexMeta> = metas.iter().filter(|m| m.nstars > 0).collect();
    usable.sort_by(|a, b| a.path.cmp(&b.path));
    if usable.is_empty() {
        return None;
    }
    Some(usable[Rng::new(seed).below(usable.len())])
}

/// Writes the field as a `.xyls`: plain primary header plus one BINTABLE of
/// `X`/`Y`/`FLUX` big-endian doubles - the layout `xyls::load` reads. No
/// `IMAGEW`/`IMAGEH`, so a caller must pass the frame size the way the shipped
/// `.xyls` do.
pub fn write_xyls(field: &Field, path: &Path) -> Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    out.write_all(&header_block(&[
        card("SIMPLE", "T"),
        card("BITPIX", "8"),
        card("NAXIS", "0"),
        card("EXTEND", "T"),
    ]))?;

    let row_len = 3 * size_of::<f64>();
    out.write_all(&header_block(&[
        card_str("XTENSION", "BINTABLE"),
        card("BITPIX", "8"),
        card("NAXIS", "2"),
        card("NAXIS1", &row_len.to_string()),
        card("NAXIS2", &field.stars.len().to_string()),
        card("PCOUNT", "0"),
        card("GCOUNT", "1"),
        card("TFIELDS", "3"),
        card_str("TTYPE1", "X"),
        card_str("TFORM1", "1D"),
        card_str("TUNIT1", "pix"),
        card_str("TTYPE2", "Y"),
        card_str("TFORM2", "1D"),
        card_str("TUNIT2", "pix"),
        card_str("TTYPE3", "FLUX"),
        card_str("TFORM3", "1D"),
    ]))?;

    let mut row = [0u8; 3 * size_of::<f64>()];
    for (i, [x, y]) in field.stars.iter().enumerate() {
        // The same brightness ramp as the in-memory list, so both paths match.
        let flux = flux_of(i, field.stars.len());
        for (slot, value) in row.chunks_mut(size_of::<f64>()).zip([*x, *y, flux]) {
            slot.copy_from_slice(&value.to_be_bytes());
        }
        out.write_all(&row)?;
    }
    // The data area is zero-padded to a whole number of blocks.
    let written = field.stars.len() * row_len;
    let pad = written.next_multiple_of(BLOCK_SIZE) - written;
    if pad > 0 {
        out.write_all(&vec![0u8; pad])?;
    }
    out.flush()?;
    Ok(())
}

/// One `KEY  = value` card (numeric or logical), right-justified to column 30.
fn card(key: &str, value: &str) -> String {
    format!("{:<8}= {:>20}{:<50}", key, value, "")
}

/// One `KEY  = 'string'` card: literal at column 11, padded to the standard's
/// minimum 8 characters - the shape `fits::parse_value` trims back off.
fn card_str(key: &str, value: &str) -> String {
    let width = usize::max(value.len(), 8);
    format!("{:<8}= {:<70}", key, format!("'{value:<width$}'"))
}

/// Cards plus `END`, space-padded to a whole `BLOCK_SIZE` header block. A card is
/// exactly `CARD_SIZE` bytes by construction; any other length is a bug here.
fn header_block(cards: &[String]) -> Vec<u8> {
    let mut block = String::new();
    for c in cards {
        debug_assert_eq!(c.len(), CARD_SIZE, "one card is one FITS record: {c:?}");
        block.push_str(c);
    }
    block.push_str(&format!("{:<CARD_SIZE$}", "END"));
    while !block.len().is_multiple_of(BLOCK_SIZE) {
        block.push(' ');
    }
    block.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::rad2deg;
    use crate::testutil;

    /// `Params` of a seeded generated field, or a skip when the installed index set
    /// cannot produce one at this frame size (a coarse all-sky-only set has none
    /// this small, which is not a code failure). Same gate as the `sweep.rs` legs.
    fn params_for(seed: u64, width: f64, height: f64) -> Option<Params> {
        testutil::generated_params(seed, width, height)
    }

    /// Relative deviation against `want`.
    fn rel_frac(got: f64, want: f64) -> f64 {
        f64::abs(got - want) / f64::abs(want)
    }

    /// A per-test scratch directory under the system temp dir.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ansolve_genfield_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// Sorts a point set so two of them compare regardless of list order.
    fn sorted(mut pts: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
        pts.sort_unstable_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
        pts
    }

    #[test]
    fn generated_truth_round_trips_through_the_wcs_conventions() {
        // Anti-drift pin: the CD parametrization must agree with what `Wcs` reports
        // for every orientation and both parities.
        for parity_neg in [true, false] {
            for deg in -180..=180 {
                let orient = deg as f64 + 0.25;
                let p = Params {
                    scale_arcsec: 1.0,
                    orient_deg: orient,
                    parity_neg,
                    ..test_params(0)
                };
                let wcs = truth_wcs(&p);
                assert_eq!(wcs.parity_is_neg(), parity_neg, "parity at {orient}");
                let got = testutil::wrap_deg(wcs.orientation_deg() - orient);
                assert!(
                    f64::abs(got) < 1e-9,
                    "orientation for {orient} deg, parity_neg {parity_neg}: got {} (off {got})",
                    wcs.orientation_deg()
                );
                assert!(
                    rel_frac(wcs.arcsec_per_pix(), 1.0) < 1e-12,
                    "scale at {orient}"
                );
            }
        }
    }

    #[test]
    fn params_of_a_random_field_are_self_consistent() {
        let Some(p) = params_for(7, 1920.0, 1080.0) else {
            return;
        };
        let f = generate(&p).expect("generate");
        assert!(f.truth.n_source >= MIN_SOURCE_STARS, "{:?}", f.truth);
        assert_eq!(f.stars.len(), f.truth.n_source + f.truth.n_extra);
        for [x, y] in &f.stars {
            assert!(x.is_finite() && y.is_finite(), "non-finite star {x},{y}");
            assert!(
                *x >= 0.0 && *x < p.width_pix && *y >= 0.0 && *y < p.height_pix,
                "star {x},{y} outside {}x{}",
                p.width_pix,
                p.height_pix
            );
        }
        // Sizes are the frame at the truth scale, as the success block prints them.
        assert!(rel_frac(f.truth.width_deg, 1920.0 * p.scale_arcsec / 3600.0) < 1e-12);
        assert!(rel_frac(f.truth.height_deg, 1080.0 * p.scale_arcsec / 3600.0) < 1e-12);
    }

    #[test]
    fn generate_is_reproducible_and_seed_dependent() {
        let Some(base) = params_for(11, 1920.0, 1080.0) else {
            return;
        };
        let again = generate(&base).expect("generate again");
        let first = generate(&base).expect("generate");
        assert_eq!(
            first.stars, again.stars,
            "same seed must give the same list"
        );
        let other = Params {
            seed: base.seed ^ 0x5eed,
            ..base.clone()
        };
        let moved = generate(&other).expect("generate at another seed");
        assert_eq!(
            moved.stars.len(),
            first.stars.len(),
            "the star count comes from the placement, not from the noise draw"
        );
        assert_ne!(first.stars, moved.stars, "another seed must move the stars");
    }

    #[test]
    fn stars_beyond_the_projection_angle_never_reach_the_frame() {
        // With the cut in place every kept star is well inside 90 deg of the
        // tangent point.
        let Some(p) = params_for(3, 1920.0, 1080.0) else {
            return;
        };
        let wcs = truth_wcs(&p);
        let f = generate(&p).expect("generate");
        let center = radec_to_xyz(p.ra_deg, p.dec_deg);
        for [x, y] in &f.stars {
            let (ra, dec) = wcs.pix_to_radec(*x, *y);
            let v = radec_to_xyz(ra, dec);
            let cos_ang = v[0] * center[0] + v[1] * center[1] + v[2] * center[2];
            let ang = rad2deg(f64::acos(f64::clamp(cos_ang, -1.0, 1.0)));
            assert!(
                ang < MAX_PROJECTION_ANGLE_DEG,
                "a star {ang:.1} deg from the tangent point survived the cut"
            );
        }
    }

    #[test]
    fn jitter_and_distractors_land_in_the_truth_counts() {
        let Some(base) = params_for(5, 1920.0, 1080.0) else {
            return;
        };
        let clean = generate(&base).expect("clean field");
        let dirty = generate(&Params {
            jitter_px: 0.3,
            extra: 40,
            ..base.clone()
        })
        .expect("noisy field");
        assert_eq!(dirty.truth.n_source, clean.truth.n_source);
        assert_eq!(dirty.truth.n_extra, 40);
        assert_eq!(dirty.stars.len(), clean.stars.len() + 40);
        // Jitter moves every source star a little. The two lists are shuffled
        // independently (the shuffle draws after the extras are appended), so match
        // each clean star to its nearest dirty one, sub-pixel mean offset expected.
        let mean = clean
            .stars
            .iter()
            .map(|[ax, ay]| {
                dirty
                    .stars
                    .iter()
                    .map(|[bx, by]| f64::hypot(ax - bx, ay - by))
                    .fold(f64::INFINITY, f64::min)
            })
            .sum::<f64>()
            / clean.stars.len() as f64;
        assert!(
            mean > 1e-9 && mean < 2.0,
            "mean nearest-star shift {mean} px"
        );
    }

    #[test]
    fn written_xyls_read_back_through_the_reader() {
        let Some(p) = params_for(13, 1920.0, 1080.0) else {
            return;
        };
        let field = generate(&p).expect("generate");
        let dir = scratch_dir("genfield-xyls");
        let path = dir.join("field.xyls");
        write_xyls(&field, &path).expect("write");

        let back = crate::xyls::load(&path).expect("read back");
        assert_eq!(back.len(), field.stars.len(), "row count");
        // `xyls::load` reorders by position, so compare point sets, not order; `1D`
        // columns carry full doubles, so coordinates come back bit-identical.
        let got = sorted(back.iter().map(|s| [s.x, s.y]).collect());
        let want = sorted(field.stars.clone());
        assert_eq!(got, want, "every star must survive the round trip");

        // And the in-memory path equals the file path star for star in the same
        // order (down to the brightness ramp): both apply `augment::preprocess` to
        // the same pixels, so an in-memory leg also tests `xyls::load`.
        let (stars, truth) = generate_stars(&p).expect("generate_stars");
        let via_file: Vec<(f64, f64, f64)> = back.iter().map(|s| (s.x, s.y, s.flux)).collect();
        let via_memory: Vec<(f64, f64, f64)> = stars.iter().map(|s| (s.x, s.y, s.flux)).collect();
        assert_eq!(
            via_file, via_memory,
            "the in-memory list and the .xyls must reach the solver identically"
        );
        assert_eq!(truth, field.truth);
    }

    #[test]
    fn jitter_is_the_requested_sigma() {
        // `jitter_px` sizes the noisy gate's budget, so it must be the sigma it
        // claims. Matched by nearest neighbour, not list position: `generate`'s
        // shuffle draws after the jitter. Source stars are far more widely spaced
        // than the jitter, so a nearest neighbour cannot be the wrong one.
        let Some(base) = params_for(23, 1920.0, 1080.0) else {
            return;
        };
        let sigma = 0.5;
        let clean = generate(&base).expect("clean field");
        let jittered = generate(&Params {
            jitter_px: sigma,
            ..base.clone()
        })
        .expect("jittered field");
        assert_eq!(jittered.stars.len(), clean.stars.len());
        // Each match is a 2D displacement: E[d^2] = 2 sigma^2, expected rms
        // sigma*sqrt(2). Over a few dozen stars that catches a sigma off by 2x.
        let sum_sq: f64 = clean
            .stars
            .iter()
            .map(|[ax, ay]| {
                jittered
                    .stars
                    .iter()
                    .map(|[bx, by]| (ax - bx) * (ax - bx) + (ay - by) * (ay - by))
                    .fold(f64::INFINITY, f64::min)
            })
            .sum();
        let rms = f64::sqrt(sum_sq / clean.stars.len() as f64);
        let want = sigma * f64::sqrt(2.0);
        assert!(
            (0.5 * want..=2.0 * want).contains(&rms),
            "jitter_px {sigma} gave an rms displacement of {rms} px, expected {want:.2}, over {} stars",
            clean.stars.len()
        );
    }

    #[test]
    fn max_stars_caps_without_duplicates_and_distractors_stay_in_frame() {
        let Some(base) = params_for(29, 1920.0, 1080.0) else {
            return;
        };
        let cap = 20;
        let full = generate(&base).expect("full field");
        if full.truth.n_source <= cap {
            // Nothing to subsample with this index set, so the cap logic would
            // prove nothing and only the distractor leg runs.
            assert!(
                full.truth.n_source >= MIN_SOURCE_STARS,
                "the placement is below its own floor: {:?}",
                full.truth
            );
        } else {
            let capped = generate(&Params {
                max_stars: cap,
                ..base.clone()
            })
            .expect("capped field");
            assert_eq!(capped.truth.n_source, cap, "the cap is a cap");
            let mut uniq = capped.stars.clone();
            uniq.sort_unstable_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
            let before = uniq.len();
            uniq.dedup();
            assert_eq!(uniq.len(), before, "the subsample repeats a star");
        }
        // Distractors are drawn inside the frame; jitter may push a source star
        // just outside it, which the verifier cannot match.
        let dirty = generate(&Params {
            jitter_px: 0.3,
            extra: 40,
            ..base.clone()
        })
        .expect("noisy field");
        assert_eq!(dirty.truth.n_extra, 40);
        assert_eq!(dirty.stars.len(), dirty.truth.n_source + 40);
        let outside = dirty
            .stars
            .iter()
            .filter(|[x, y]| {
                *x < -1.0 || *y < -1.0 || *x > base.width_pix + 1.0 || *y > base.height_pix + 1.0
            })
            .count();
        assert_eq!(outside, 0, "a star landed far outside the frame");
    }

    #[test]
    fn noise_field_is_all_distractors_in_frame() {
        let (w, h, n) = (1920.0, 1080.0, 30);
        let field = noise_field(w, h, 1.0, n, 7);
        assert_eq!(field.stars.len(), n, "the requested star count");
        assert_eq!(field.truth.n_extra, n);
        assert_eq!(field.truth.n_source, 0, "nothing comes from an index");
        // The gate that uses this asserts only "not solved", so the list must be
        // checked non-empty here and after `preprocess`.
        assert!(!field_stars(&field).is_empty(), "preprocess kept the list");
        for [x, y] in &field.stars {
            assert!(
                *x >= 0.0 && *x < w && *y >= 0.0 && *y < h,
                "distractor {x},{y} outside {w}x{h}"
            );
        }
    }

    #[test]
    fn the_truth_record_alone_rebuilds_the_projection() {
        // `Truth` is what a gate compares the answer against and `wcs_of` the
        // projection it implies; if they disagreed a gate would check the printed
        // numbers against a sky the pixels were not made from.
        let Some(p) = params_for(17, 1920.0, 1080.0) else {
            return;
        };
        let f = generate(&p).expect("generate");
        let from_truth = wcs_of(&f.truth);
        let from_params = truth_wcs(&p);
        assert_eq!(from_truth.crval_deg, from_params.crval_deg);
        for i in 0..2 {
            assert!(
                f64::abs(from_truth.crpix[i] - from_params.crpix[i]) < 1e-9,
                "crpix[{i}] {} vs {}",
                from_truth.crpix[i],
                from_params.crpix[i]
            );
            for j in 0..2 {
                assert!(
                    f64::abs(from_truth.cd[i][j] - from_params.cd[i][j]) < 1e-15,
                    "cd[{i}][{j}] {} vs {}",
                    from_truth.cd[i][j],
                    from_params.cd[i][j]
                );
            }
        }
        assert_eq!(from_truth.parity_is_neg(), f.truth.parity_neg);
        assert!(rel_frac(from_truth.arcsec_per_pix(), f.truth.scale_arcsec) < 1e-12);
        // And the pixels the field was built from stay where the truth puts them.
        for [x, y] in &f.stars {
            let (ra, dec) = from_truth.pix_to_radec(*x, *y);
            let (back_x, back_y) = from_truth.radec_to_pix(ra, dec);
            assert!(
                f64::abs(back_x - x) < 1e-9 && f64::abs(back_y - y) < 1e-9,
                "star {x},{y} did not survive the truth round trip"
            );
        }
    }

    #[test]
    fn default_index_picks_a_usable_index_file() {
        // Reached through the shared gate, so unit legs and outside callers resolve
        // the same index file.
        let Some(path) = testutil::generated_index() else {
            return;
        };
        let loaded = Index::load(&path).expect("the picked index loads");
        assert!(loaded.meta.nstars > 0);
        assert!(
            scale_for_index(&loaded.meta, 1920.0, 1080.0).is_some(),
            "the picked index {} has no plate scale for a 1920x1080 field",
            path.display()
        );
    }

    #[test]
    fn random_index_draws_are_stable_and_spread_over_the_set() {
        let Some(metas) = testutil::metas() else {
            return;
        };
        // Same seed, same file: a gate that names a seed owns one index file.
        for seed in [0u64, 1, 7, 42] {
            let once = random_index_from(metas, seed).expect("an installed file to draw");
            let twice = random_index_from(metas, seed).expect("the same draw twice");
            assert_eq!(once.path, twice.path, "draw {seed} is not stable");
            assert!(once.nstars > 0, "{} has no stars", once.path.display());
        }
        // The draw spreads over the set, and the directory wrapper hands back a
        // file that really loads.
        let distinct: std::collections::HashSet<_> = (0..64u64)
            .filter_map(|s| random_index_from(metas, s).map(|m| &m.path))
            .collect();
        assert!(
            distinct.len() > 1,
            "64 draws over {} index files landed on {}",
            metas.len(),
            distinct.len()
        );
        let dir = metas[0].path.parent().expect("an index file in a dir");
        let drawn = random_index(dir, 7).expect("a file of the tested directory");
        assert!(
            Index::load(&drawn)
                .expect("the drawn file loads")
                .meta
                .nstars
                > 0
        );
    }

    /// A `Params` with only what a convention test needs. The index path is unset:
    /// nothing here loads it.
    fn test_params(seed: u64) -> Params {
        Params {
            index: PathBuf::new(),
            seed,
            ra_deg: 0.0,
            dec_deg: 0.0,
            width_pix: 100.0,
            height_pix: 100.0,
            scale_arcsec: 1.0,
            orient_deg: 0.0,
            parity_neg: true,
            jitter_px: 0.0,
            extra: 0,
            max_stars: 0,
        }
    }

    #[test]
    fn hand_built_params_are_checked() {
        // Every branch of `check_params`, so a bad hand-built `Params` fails here.
        let bad = Params {
            scale_arcsec: 0.0,
            ..test_params(0)
        };
        assert!(generate(&bad).is_err(), "a zero plate scale is a bug");
        let bad = Params {
            scale_arcsec: f64::NAN,
            ..test_params(0)
        };
        assert!(generate(&bad).is_err(), "a NaN plate scale is a bug");
        let bad = Params {
            width_pix: 0.0,
            ..test_params(0)
        };
        assert!(generate(&bad).is_err(), "a zero-width frame is a bug");
        let bad = Params {
            dec_deg: 200.0,
            ..test_params(0)
        };
        assert!(
            generate(&bad).is_err(),
            "a declination off the sphere is a bug"
        );
        let bad = Params {
            jitter_px: -1.0,
            ..test_params(0)
        };
        assert!(generate(&bad).is_err(), "a negative sigma is a bug");
        let bad = Params {
            max_stars: MAX_OBJS + 1,
            ..test_params(0)
        };
        assert!(generate(&bad).is_err(), "beyond the depth ramp cap");
    }
}
