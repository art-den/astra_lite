//! Synthetic fields with exact sky truth, for the test suite.
//!
//! The reference stars of an installed index file are a real star pattern whose
//! sky positions the solver itself can read back (`Index::star_xyz`), so
//! projecting a subset of them through a WCS *chosen here* produces a star list
//! whose truth is exact by construction: the field center, plate scale,
//! orientation and parity of the answer are the four numbers that went into the
//! projection. That is what the shipped `testdata/` fields cannot give - their
//! truth is a reference run and carries real-observation noise and distortion.
//!
//! Two traps this module exists to avoid, both measured:
//!
//! * A gnomonic projection is only defined within 90 degrees of its tangent
//!   point. Beyond it a star lands back inside the frame, and the far-side stars
//!   are a *mirrored* pattern of the antipodal sky - such a field really does
//!   have a second solution half a sphere away. Hence
//!   `MAX_PROJECTION_ANGLE_DEG`, which keeps the truth unique.
//! * The source index must be one the sweep will try at the field's plate scale:
//!   a field made from an index the scale selection prunes never solves. So
//!   [`random_params`] derives the scale from the index' own `SCALE_L`/`SCALE_U`
//!   band and checks it with `IndexMeta::fits_scale`.
//!
//! The surface is deliberately plain data (`PathBuf`, `f64`, `Vec<[f64; 2]>`),
//! so this module can be published to `tests/` while every pipeline module stays
//! private. The in-memory star-list helpers (`generate_stars`, `noise_stars`,
//! `wcs_of`) exist only under `cfg(test)`: an outside caller goes through
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
// Only the in-memory star-list helpers need these: the file path hands the pixel
// list to `xyls::load`, which applies the same reordering itself.
#[cfg(test)]
use crate::{augment, xyls::Star};

/// Gate budgets of a generated field, shared by the unit legs and the
/// integration target so the two cannot drift apart. Center is in pixels of the
/// field's own plate scale, rotation in degrees, sizes relative.
///
/// The pattern of a clean generated field is the catalog itself, so its residual
/// is solver noise only: these are a drift alarm rather than a precision claim -
/// and still several times tighter than the shipped fields' budgets, whose truth
/// is a reference run carrying real-observation noise.
pub const TOL_CENTER_PX: f64 = 0.2;
pub const TOL_ORIENT_DEG: f64 = 0.05;
pub const TOL_SIZE_FRAC: f64 = 0.001;

/// Ceiling on the angular distance from the tangent point a generated field may
/// span, degrees. Well inside the 90 degrees where TAN is injective, and far
/// outside any field the solver is asked to produce, so it only ever removes the
/// antipodal contamination described in the module docs.
const MAX_PROJECTION_ANGLE_DEG: f64 = 60.0;
/// Floor on the sky radius the selection pass looks over, degrees. Keeps a tiny
/// narrow field from being trimmed by the cut instead of by the frame.
const MIN_SELECTION_RADIUS_DEG: f64 = 5.0;
/// Fewest stars taken from the index that a generated field must contain before
/// [`random_params`] accepts the placement. Below it a field is too sparse to
/// verify, and the odds gate has too little to work with.
const MIN_SOURCE_STARS: usize = 12;
/// Placements [`random_params`] tries before giving up on an index.
const MAX_PLACEMENT_TRIES: usize = 24;
/// Star count [`random_params`] caps a field at, below `MAX_OBJS` so the depth
/// ramp still reaches the whole list.
const DEFAULT_MAX_STARS: usize = 120;
/// Plate scale bounds, arcsec/pixel, that [`random_params`] will hand back.
const MIN_SCALE_ARCSEC: f64 = 0.05;
const MAX_SCALE_ARCSEC: f64 = 60.0;
/// The index quad size that sets the plate scale, as a fraction of the frame
/// diagonal: half the diagonal, so the source index' quads span a good part of
/// the field instead of a corner of it.
const QUAD_OVER_FRAME: f64 = 0.5;
/// Half-width of the plate-scale window [`scale_window`] returns, as a fraction
/// of the scale. Wide enough that the index a field was generated from survives
/// the sweep's scale selection (`random_params` guarantees one exists at the
/// field's scale), narrow enough that the sweep tries few indices.
const SCALE_WINDOW_FRAC: f64 = 0.02;

/// What field to generate. Every field is explicit, so a gate that builds
/// `Params` by hand gets the same field from the same `index` file, on any
/// machine (the index file itself is the machine-local half of the input).
#[derive(Debug, Clone)]
pub struct Params {
    /// Index file to take reference stars from. It must be one the sweep will
    /// try at `scale_arcsec` (see [`random_params`]).
    pub index: PathBuf,
    /// Seed of every choice [`generate`] makes: subsample, jitter, distractors
    /// and list order. The center, orientation and parity are not drawn here -
    /// [`random_params`] picks those with its own draw and stores the derived
    /// seed, so changing `seed` afterwards re-rolls the noise but not the sky.
    pub seed: u64,
    /// Truth field center, degrees. It must lie inside the index' coverage;
    /// [`random_params`] picks it from a star of the index to guarantee that.
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
    /// Sigma of the centroid noise added to every source star, pixels. `0.0`
    /// leaves the pattern exact; the field then matches the index star for star.
    pub jitter_px: f64,
    /// Uniform-random stars added on top of the sources: the bad-pixel and
    /// diffraction-spike model the verifier's distractor handling exists for.
    /// They count towards the list the depth ramp walks, so a large `extra` can
    /// push the tail of the list past `MAX_OBJS`, where the ramp never reaches.
    pub extra: usize,
    /// Cap on the source stars kept (random subsample); `0` keeps them all.
    /// Above `MAX_OBJS` the depth ramp would never reach the tail.
    pub max_stars: usize,
}

/// The sky truth a generated field was built from: the numbers the success block
/// prints, plus what went into the star list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Truth {
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub width_deg: f64,
    pub height_deg: f64,
    pub orient_deg: f64,
    pub parity_neg: bool,
    pub scale_arcsec: f64,
    /// Stars taken from the index, before the jitter and before any reordering
    /// can drop them: the upper bound of what can match.
    pub n_source: usize,
    /// Uniform-random stars added after them.
    pub n_extra: usize,
}

/// A generated field: its truth and its pixel list in the generator's own order
/// (a projection is shuffled, a pure-distractor frame is not). The order the
/// solver sees is produced later - by `augment::preprocess` for the in-memory
/// list, by `xyls::load` for the file.
#[derive(Debug, Clone)]
pub struct Field {
    pub truth: Truth,
    pub stars: Vec<[f64; 2]>,
}

/// Deterministic PRNG (splitmix64). The dependency set is frozen, so the suite
/// gets its own generator rather than a `rand` crate; nothing here needs
/// cryptographic quality, only reproducibility from a `u64` seed.
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

    /// A value in [0, 1) built from the 53 high bits of one draw: splitmix64's
    /// low bits are the weakest, so they are the ones dropped here.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }

    /// An index into `0..n`; `n` must not be zero. The modulo bias is irrelevant
    /// at these list sizes and the cost is one division.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }

    /// Standard normal, Box-Muller; the second root of the pair is discarded on
    /// purpose. The `max` keeps `ln` finite for the one draw whose high 53 bits
    /// all come out zero.
    fn normal(&mut self) -> f64 {
        let u = f64::max(self.unit(), f64::MIN_POSITIVE);
        let v = self.unit();
        f64::sqrt(-2.0 * f64::ln(u)) * f64::cos(2.0 * std::f64::consts::PI * v)
    }
}

/// CD matrix of a truth WCS with the requested printed orientation and parity.
///
/// Both branches are the inverse of what `Wcs` reports (`parity_is_neg` is
/// `det(CD) > 0`, and `orientation_deg` folds the parity sign into its `atan2`
/// arguments); `generated_truth_round_trips_through_the_wcs_conventions` pins
/// that for every orientation and both parities.
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

/// The truth WCS of `t`: tangent point at the printed field center, which is
/// where the success block reports from (`Wcs::recentered_at` of
/// [`pixel_center_of_size`]), so the comparison needs no reprojection.
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

/// The truth WCS of a `Params` (the frame size comes from the params, not from
/// a `Truth`).
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

/// Validates the parameters a caller built by hand; anything here would
/// otherwise surface as a field that cannot solve, which is a confusing failure
/// for a test whose own numbers are wrong.
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

/// The field `p` describes, from an already-loaded index: the index stars inside
/// the projection cut that land in the frame, randomly subsampled to
/// `max_stars`, jittered, extended with `extra` distractors, then shuffled (the
/// shuffle is why the list order carries no brightness or position meaning).
fn project(index: &Index, p: &Params) -> Field {
    let wcs = truth_wcs(p);
    let half_diag_deg = f64::hypot(p.width_pix, p.height_pix) * p.scale_arcsec / 7200.0;
    // Look out to twice the frame's half-diagonal so the frame is filled from a
    // pattern wider than itself, with a floor for tiny narrow fields (at these
    // scales the diagonal term is usually below it, so the floor is what bites)
    // and the antipodal ceiling of the module docs above.
    let cut_deg = f64::clamp(
        2.0 * half_diag_deg,
        MIN_SELECTION_RADIUS_DEG,
        MAX_PROJECTION_ANGLE_DEG,
    );
    // The cut is a dot-product test: cos(cut) on the tangent-point direction, no
    // trig per star (see `coords::distsq` for the same chord trick).
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
    // Subsample before the noise, so `max_stars` bounds the work the solver's
    // depth ramp has to do rather than the work this module did.
    if p.max_stars != 0 && kept.len() > p.max_stars {
        // Partial Fisher-Yates: the first `max_stars` slots of a shuffle are a
        // uniform sample without replacement, and only need that many swaps.
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
    // Counted after the subsample and the jitter: `n_source` is how many stars
    // can actually match, which is also the denominator of the noisy-field gate.
    let n_source = kept.len();
    for _ in 0..p.extra {
        kept.push([rng.range(0.0, p.width_pix), rng.range(0.0, p.height_pix)]);
    }
    // The list order is the depth ramp's star ranking, and a real detection list
    // is not ordered by sky position - so shuffle, and let the truth be the
    // pattern rather than any implied brightness order.
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

/// The field's star list as the sweep wants it: a `Vec<Star>` that has been
/// through the same [`augment::preprocess`] `xyls::load` applies, so a unit gate
/// drives `sweep::solve_field` with exactly the list a `.xyls` of these stars
/// would produce - without writing or reading a file.
#[cfg(test)]
pub(crate) fn generate_stars(p: &Params) -> Result<(Vec<Star>, Truth)> {
    let field = generate(p)?;
    Ok((field_stars(&field), field.truth))
}

/// A field of pure distractors: `n_stars` uniform-random pixels and nothing from
/// any index. The false-positive gate's input - a list the odds scan must not
/// talk into a solution.
///
/// Its `Truth` carries no usable sky at all: only `n_extra` and the frame mean
/// anything here, and the center, rotation and parity are placeholders.
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

/// The brightness of list entry `i` of `n`: a falling ramp, so the `FLUX` column
/// is not a constant, like a real detection list. Brightness carries no weight in
/// the pipeline (see `crate::xyls::Star`); what the two producers below must
/// agree on is the pixel list, and `written_xyls_read_back_through_the_reader`
/// compares this column across them too.
fn flux_of(i: usize, n: usize) -> f64 {
    1e6 * (1.0 - i as f64 / f64::max(n as f64, 1.0))
}

/// The pixel list as `Star`s with a falling brightness ramp, run through the
/// position reordering the reader applies.
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

/// The plate scale that puts the index' own quad band across the frame: the
/// middle of the band, divided by the fraction of the frame diagonal a quad
/// should span. Returns `None` for an index with no usable band.
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
    // The scale is only usable if the sweep's own selection keeps this index at
    // it, under the very window a run of this field is swept with.
    let (lo, hi) = scale_window(s);
    if !meta.fits_scale(lo, hi, width, height) {
        return None;
    }
    Some(s)
}

/// A `Params` for a random field of `width_pix` x `height_pix` in `index`:
/// center at a star of that index (hence inside its coverage), plate scale in
/// the index' band, orientation uniform and parity random, then the first
/// placement with at least `MIN_SOURCE_STARS` source stars.
///
/// Every choice comes from `seed`, so a gate names one seed and owns one field.
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
            // Derived rather than the argument, and distinct per attempt: the
            // seed is what `generate` re-rolls, so a retry that re-rolled nothing
            // would reproduce the same star set and loop for nothing. Any odd
            // multiplier would do; this one just spreads the bits well.
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

/// The `--scale-low/--scale-high` pair (arcsec/pixel) a run should sweep a
/// field of this plate scale with, so the index it was generated from is among
/// the indices the sweep tries. See `SCALE_WINDOW_FRAC`.
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

/// The index of `metas` to generate fields from: the sub-region tile with the
/// most stars, else the largest index whatever its coverage.
///
/// A dense tile is the cheap choice: the pattern is local, so the field needs few
/// stars, and the file stays small. Taking metas rather than a directory lets a
/// caller that has already scanned the headers not scan them again.
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

/// A pseudo-random index file of `dir` to generate fields from, drawn from
/// `seed`: spreads a gate's fields over the installed set instead of always
/// handing back the same file. Deterministic - the same `seed` over the same set
/// picks the same file, and a pick is a draw, not a shuffle, so two seeds can
/// land on one file.
pub fn random_index(dir: &Path, seed: u64) -> Result<PathBuf> {
    let metas = load_metas_in(&[dir.to_path_buf()], Unusable::Skip)?;
    random_index_from(&metas, seed)
        .map(|m| m.path.clone())
        .ok_or_else(|| anyhow::anyhow!("no usable index file in {}", dir.display()))
}

/// [`random_index`] over metas someone else has already scanned, so an in-crate
/// gate can draw over the whole installed set without reparsing headers. Sorted
/// by path before the draw, so the pick does not depend on the order the
/// directory scan happened to produce.
pub(crate) fn random_index_from(metas: &[IndexMeta], seed: u64) -> Option<&IndexMeta> {
    let mut usable: Vec<&IndexMeta> = metas.iter().filter(|m| m.nstars > 0).collect();
    usable.sort_by(|a, b| a.path.cmp(&b.path));
    if usable.is_empty() {
        return None;
    }
    Some(usable[Rng::new(seed).below(usable.len())])
}

/// Writes the field as a `.xyls`: the plain primary header plus one BINTABLE of
/// `X`/`Y`/`FLUX` big-endian doubles, which is the layout `xyls::load` reads.
///
/// Hand-written because the crate has no FITS writer, and a test that cannot
/// write its own input has no truth to check against. No `IMAGEW`/`IMAGEH`
/// cards, so a caller must pass the frame size the way the shipped `.xyls` do.
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
        // The same brightness ramp the in-memory list gets, so both paths hand
        // the solver the same values.
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

/// One `KEY  = value` card with a numeric or logical value, right-justified to
/// end at column 30 where the standard puts it, leaving columns 31-80 blank.
fn card(key: &str, value: &str) -> String {
    format!("{:<8}= {:>20}{:<50}", key, value, "")
}

/// One `KEY  = 'string'` card: the literal starts at column 11 and is padded to
/// the standard's minimum string width of 8 characters, which is the shape
/// `fits::parse_value` trims back off.
fn card_str(key: &str, value: &str) -> String {
    let width = usize::max(value.len(), 8);
    format!("{:<8}= {:<70}", key, format!("'{value:<width$}'"))
}

/// Cards plus `END`, padded with spaces to a whole `BLOCK_SIZE` header block.
/// Both card helpers above produce exactly `CARD_SIZE` bytes, so a card of any
/// other length is a bug in this module rather than input to be truncated.
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

    /// `Params` of a seeded generated field, or a skip when the installed index
    /// set cannot produce one at this frame size (a coarse all-sky-only set has
    /// no field this small, and that is not a code failure). The same gate the
    /// `sweep.rs` generated-field legs use.
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
        // The generator's CD parametrization must agree with what `Wcs` reports,
        // for every orientation and both parities: this is the anti-drift pin, so
        // a field cannot be generated against a truth the reader would not print.
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
        // The antipodal contamination the module docs describe: with the cut in
        // place, every kept star is well inside 90 deg of the tangent point.
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
        // Jitter moves every source star, by a little. The two lists are shuffled
        // independently (the shuffle draws after the extras are appended), so the
        // order carries nothing: match each clean star to its nearest dirty star
        // and require a sub-pixel mean offset - far above zero, far below the
        // 1920 px frame.
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
        // `xyls::load` reorders by position (`augment::preprocess`), so compare
        // the point sets, not the order. `1D` columns carry full doubles, so the
        // coordinates must come back bit-identical.
        let got = sorted(back.iter().map(|s| [s.x, s.y]).collect());
        let want = sorted(field.stars.clone());
        assert_eq!(got, want, "every star must survive the round trip");

        // And the in-memory path is the file path, star for star and in the same
        // order (down to the brightness ramp): both apply `augment::preprocess`
        // to the same pixels. This is what makes an in-memory unit leg's result a
        // statement about what `xyls::load` hands the sweep.
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
        // `jitter_px` sizes the budget of the noisy gate, so it has to be the
        // sigma it claims. Stars are matched by nearest neighbour, not by list
        // position: the shuffle inside `generate` draws after the jitter, so the
        // two lists come out in different orders. Source stars are far more
        // widely spaced than the jitter, so a nearest neighbour cannot be the
        // wrong one (and if it were, the band below would not hold).
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
        // Every match is a 2D displacement, so E[d^2] = 2 sigma^2 and the expected
        // rms is sigma*sqrt(2). Over a few dozen stars that statistic is stable
        // enough to catch a sigma off by 2x, which is all the band is for.
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
            // Nothing to subsample on this machine's index; the cap logic below
            // would prove nothing, so only the distractor leg runs.
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
        // Distractors are drawn inside the frame, and with jitter on, source stars
        // may land just outside it - which the verifier simply cannot match.
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
        // The gate that uses this asserts only "not solved", so the list itself
        // must be checked not to be empty (or to have been emptied downstream).
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
        // `Truth` is what a gate compares the answer against, and `wcs_of` is the
        // projection it implies. If the two ever disagreed, a gate would be
        // checking the printed numbers against a sky the pixels were not made
        // from, so the pixel list must survive a round trip through the truth.
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
        // The selector is reached through the shared gate, so the unit legs and
        // an outside caller resolve the same index file.
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
        // The draw spreads over the set instead of pinning one file, and the
        // public wrapper over a directory hands back a file that really loads.
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

    /// A `Params` with only what a convention test needs filled in. The index
    /// path is unset until a test needs one: nothing here loads it.
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
        // Every branch of `check_params`, so a hand-built `Params` fails here
        // instead of as a field that mysteriously never solves.
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
