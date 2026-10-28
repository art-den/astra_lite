//! Loading the old-format (`AN_FILE='QUAD'`) astrometry.net index file.
//!
//! An index is the primary header, one `quads` table holding the star indices
//! of every quad, and the two kd-trees `codes` (the quad codes) and `stars`
//! (the index stars). Only that layout is supported, and its data areas are
//! byte views into the mmap'd file rather than decoded copies.
//!
//! Two kinds of sky coverage share it: all-sky indices (`HEALPIX=-1`,
//! `ALLSKY=T`) and healpix sub-region tiles (`HEALPIX>=0` + `HPNSIDE`), whose
//! cell number uses the astrometry.net "xy" pixel numbering - the tile/hint
//! overlap geometry lives in `healpix.rs`.

use anyhow::{Context, Result, ensure};
use std::fs;
use std::path::{Path, PathBuf};

use crate::coords::{RAD2ARCSEC, rad2arcsec};
use crate::fits::{BlobView, Endian, Fits, FitsHeaders, Header, Headers};
use crate::kdtree::KdTree;
use crate::params::{QSF_HI, QSF_LO};
use crate::solver::star_outside_hint;

/// Everything we need from the primary header (and the codes-tree header) to
/// decide whether an index is usable.
#[derive(Debug, Clone)]
pub struct IndexMeta {
    pub path: PathBuf,
    pub indexid: i64,
    pub dimquads: usize,
    pub nquads: usize,
    pub nstars: usize,
    /// Bounds of the quad-size band the index covers, arcsec on the sky
    /// (`SCALE_L`/`SCALE_U` carry the same band in radians). Quad size is the
    /// backbone `|AB|` length, the quantity `QSF_LO`/`QSF_HI` bound in pixels
    /// on the field side; [`Self::fits_scale`] overlaps the two bands.
    pub scale_lower_arcsec: f64,
    pub scale_upper_arcsec: f64,
    pub healpix: i64,
    /// `HPNSIDE`: the healpix subdivision of a sub-region tile (`healpix >= 0`).
    /// 0 for all-sky indices and when the key is absent. A tile whose nside is
    /// missing or not a power of two is refused at load as corrupt - no
    /// subdivision is invented for it.
    pub hpnside: i64,
    pub allsky: bool,
    pub endian: Endian,
    /// Quad-convention flags, read from the codes kd-tree header; a file
    /// without that header falls back to the primary one. They say which
    /// filters the index builder applied to its codes, so the field side can
    /// prune its own codes the same way: `circle` is the code circle centered
    /// (0.5, 0.5) of radius 1/sqrt(2) and is required (see `from_fits`);
    /// `cx_less_than_dx` (`CXDX`, `cx <= dx` for the non-backbone stars) and
    /// `meanx_less_than_half` (`CXDXLT1`, the code's mean x never above 0.5)
    /// are the canonicalization filters `solver.rs` passes to
    /// `codes::canonical_ok`.
    pub circle: bool,
    pub cx_less_than_dx: bool,
    pub meanx_less_than_half: bool,
}

/// First-column (`TTYPE1`) names that mark a table this reader cannot
/// interpret - the index flavors outside its scope (new-format indices, H4,
/// a healpix lookup). Finding one of them in a file is a load error, not a
/// partial read of a format we would get wrong.
const UNSUPPORTED_TABLES: [&str; 5] = [
    "index_stars",
    "index_margins",
    "quad_degs",
    "h42232",
    "healpix",
];

/// Reads a non-negative integer header key as `usize` (`NQUADS`/`NSTARS`).
/// A negative value is a clean load error (exit 2), not a debug overflow panic
/// or an `as usize` bit-cast that yields a garbage value like 18446744073709551615.
fn require_usize(primary: &Header, key: &str) -> Result<usize> {
    let v = primary.require_i64(key)?;
    usize::try_from(v).map_err(|_| anyhow::anyhow!("{key} is {v} (must be non-negative)"))
}

impl IndexMeta {
    /// Builds the metadata from any header source: a whole file (`Fits`) or the
    /// header-only reader (`FitsHeaders`).
    ///
    /// Error messages here never mention `path`: the caller adds the
    /// `loading index {path}` context, so a filename appears exactly once in
    /// the rendered error.
    fn from_fits(fits: &dyn Headers, path: &Path) -> Result<Self> {
        let primary = fits.primary();
        let an_file = primary
            .get_str("AN_FILE")
            .ok_or_else(|| anyhow::anyhow!("no AN_FILE key: not an astrometry.net index file"))?;
        ensure!(
            an_file == "QUAD",
            "unsupported index file type AN_FILE={an_file:?} (only 'QUAD' is supported)"
        );
        for name in fits.table_names() {
            ensure!(
                !UNSUPPORTED_TABLES.contains(&name),
                "unsupported index feature: table {name:?} (plain QUAD indices only)"
            );
        }

        let healpix = primary.get_i64("HEALPIX").unwrap_or(-1);
        let allsky = primary.get_bool("ALLSKY").unwrap_or(healpix == -1);
        let hpnside = primary.get_i64("HPNSIDE").unwrap_or(0);
        if healpix >= 0 {
            // A healpix sub-region tile: one cell of the sky carrying the same
            // QUAD table layout. The cell number is in the xy scheme
            // `healpix.rs` implements, not the FITS ring order.
            ensure!(
                !allsky,
                "contradictory index header: HEALPIX={healpix} with ALLSKY=T"
            );
            ensure!(
                hpnside > 0,
                "healpix sub-region index HEALPIX={healpix} has no HPNSIDE; a tile \
                 without nside is treated as corrupt"
            );
            ensure!(
                u64::try_from(hpnside).is_ok_and(u64::is_power_of_two),
                "HPNSIDE={hpnside} is not a power of two"
            );
            // Only the xy cell numbering is implemented here, and the check
            // does not look at nside: any NESTED key is refused, because a tile
            // read under the wrong numbering points at the wrong sky cell - and
            // so misplaces the whole field.
            if let Some(comp) = primary.get_str("HPNCOMP") {
                ensure!(
                    !comp.eq_ignore_ascii_case("NESTED"),
                    "healpix tile scheme HPNCOMP={comp:?}: only the xy cell numbering is \
                     supported"
                );
            }
        } else {
            ensure!(
                healpix == -1 && allsky,
                "unsupported index position keys: HEALPIX={healpix} ALLSKY={allsky} (expected \
                 all-sky HEALPIX=-1/ALLSKY=T, or a healpix tile HEALPIX>=0 with HPNSIDE)"
            );
        }

        let dimquads = primary.require_i64("DIMQUADS")? as usize;
        ensure!(
            dimquads == 3 || dimquads == 4,
            "unsupported DIMQUADS={dimquads} (only 3 and 4 are supported)"
        );

        // The quad-convention flags (CIRCLE/CXDX/CXDXLT1) are looked for in the
        // codes kd-tree header; only a file that has no such header is read
        // from the primary one.
        let flag_header = match fits.header_by_ttype("kdtree_header_codes") {
            Ok(h) => h,
            Err(_) => fits.primary(),
        };

        // SCALE_L/SCALE_U are the index's quad-size band in radians. A damaged
        // band never errors later, it only skews index selection: a NaN bound
        // makes its side of the `fits_scale` overlap test always false (both
        // NaN and the index is a candidate for every field), a non-positive
        // lower bound disables the low-side check outright, and an inverted
        // band admits fields the index cannot serve. Reject them at load
        // (exit 2).
        let scale_lower_rad = primary.require_f64("SCALE_L")?;
        let scale_upper_rad = primary.require_f64("SCALE_U")?;
        ensure!(
            scale_lower_rad.is_finite()
                && scale_upper_rad.is_finite()
                && scale_lower_rad > 0.0
                && scale_lower_rad <= scale_upper_rad,
            "invalid SCALE_L/SCALE_U ({scale_lower_rad}, {scale_upper_rad}) radians"
        );

        let meta = Self {
            path: path.to_path_buf(),
            indexid: primary.require_i64("INDEXID")?,
            dimquads,
            nquads: require_usize(primary, "NQUADS")?,
            nstars: require_usize(primary, "NSTARS")?,
            scale_lower_arcsec: rad2arcsec(scale_lower_rad),
            scale_upper_arcsec: rad2arcsec(scale_upper_rad),
            healpix,
            hpnside,
            allsky,
            endian: fits.endian(),
            circle: flag_header.get_bool("CIRCLE").unwrap_or(false),
            cx_less_than_dx: flag_header.get_bool("CXDX").unwrap_or(false),
            meanx_less_than_half: flag_header.get_bool("CXDXLT1").unwrap_or(false),
        };
        // The solver applies the code-circle filter to every field quad
        // unconditionally (`codes::in_circle`, reached from `solver.rs`), and
        // that pruning is only sound when the index codes were built under the
        // same convention. Without CIRCLE the miss is silent: field quads whose
        // code falls outside the circle are pruned while the index may still
        // hold their match. Refuse the file here rather than report a field
        // unsolved.
        ensure!(
            meta.circle,
            "code kd-tree does not declare CIRCLE; unsupported quad convention"
        );
        Ok(meta)
    }

    /// Index scale selection: does this index's quad-size band overlap the one
    /// the field can produce?
    ///
    /// The field's band runs from `QSF_LO` * the shorter frame side * `app_min`
    /// to `QSF_HI` * the frame diagonal * `app_max`: the smallest and largest
    /// angular backbone length its quads can have. The overlap test is
    /// inclusive, so bands that merely touch still select the index.
    ///
    /// `app_min`/`app_max` are in arcsec/px, `width`/`height` in px.
    pub fn fits_scale(&self, app_min: f64, app_max: f64, width: f64, height: f64) -> bool {
        let quadsize_min = QSF_LO * f64::min(width, height);
        let fmin = quadsize_min * app_min;
        let fmax = QSF_HI * f64::hypot(width, height) * app_max;
        !(fmin > self.scale_upper_arcsec || fmax < self.scale_lower_arcsec)
    }

    /// Mean catalog density over the sky this index covers, stars per square
    /// degree.
    ///
    /// An all-sky index covers 4pi sr = 41252.96 deg^2. A ring-scheme healpix
    /// tile of `hpnside` covers 1/(12 nside^2) of the sphere and healpix pixels
    /// are equal-area, so one cell's area follows from `nside` alone. `None`
    /// when the file offers no usable star count or nside, which callers must
    /// treat as "no density estimate", never as 0.
    #[must_use]
    pub fn stars_per_deg2(&self) -> Option<f64> {
        if self.nstars == 0 {
            return None;
        }
        // Square degrees in the whole sphere, and the equal-area cells a
        // ring-scheme healpix grid of `nside` divides the sphere into (12 at
        // nside 1).
        const SPHERE_DEG2: f64 = 41252.96125917812;
        const HEALPIX_BASE_CELLS: f64 = 12.0;
        let sphere_frac = if self.allsky {
            1.0
        } else {
            if self.hpnside < 1 {
                return None;
            }
            let n = self.hpnside as f64;
            1.0 / (HEALPIX_BASE_CELLS * n * n)
        };
        let deg2 = SPHERE_DEG2 * sphere_frac;
        Some(self.nstars as f64 / deg2)
    }

    pub fn describe(&self) -> String {
        let mut s = format!(
            "index {}: quads={} stars={} dimquads={} scale=[{:.6}, {:.6}] arcsec, endian={}",
            self.indexid,
            self.nquads,
            self.nstars,
            self.dimquads,
            self.scale_lower_arcsec,
            self.scale_upper_arcsec,
            endian_name(self.endian),
        );
        // Name the cell so the verbose per-index lines (which print this
        // string) tell tiles of one indexid apart.
        if self.healpix >= 0 {
            s.push_str(&format!(
                ", healpix={} hpnside={}",
                self.healpix, self.hpnside
            ));
        }
        s
    }
}

fn endian_name(e: Endian) -> &'static str {
    match e {
        Endian::Little => "little (04:03:02:01)",
        Endian::Big => "big (01:02:03:04)",
    }
}

/// A fully loaded index: quads plus both kd-trees. Their bulk arrays are byte
/// views sharing the mmap'd file and elements are decoded on access, so loading
/// an index costs only the pages the solver actually touches. Those are clean,
/// evictable page cache, never an anonymous copy, which is what keeps the memory
/// profile independent of the index file size.
pub struct Index {
    pub meta: IndexMeta,
    /// `nquads * dimquads` star indices: per quad its slots `(A, B, C[, D])` as
    /// u32, `A`/`B` the backbone pair the quad code is built on. A blob column
    /// of the mapped file, read through [`Self::quad`] by the solver's hit
    /// processing and by verification.
    quads: BlobView,
    /// Tree over the quad codes: one record per quad holding an (x, y) pair for
    /// every non-backbone star, hence `ndim = 2 * (DIMQUADS - 2)` (checked at
    /// load). The quantized values are decoded per the tree's `KDT_DATA` kind,
    /// `u16` or `u32`, both accepted.
    pub codes: KdTree,
    /// Tree over the index stars: the quantized unit-sphere direction of each
    /// star (`ndim == 3`, checked at load), dequantized by [`Self::star_xyz`].
    pub stars: KdTree,
    /// The stars tree's `CUTNSIDE` header key. Nothing in this solver reads it
    /// (the star lookup it belongs to is out of scope), so `--dump-index` only
    /// echoes it, or -1 when the stars tree header has no such key.
    pub cutside: i64,
}

/// The quads of one index that a `--ra/--dec` hint can reach at all, and the
/// compact codes tree built over them.
///
/// Which quads are reachable belongs to the index file and the hint alone - never
/// to the field, the depth round or the worker schedule - so a sweep computes it
/// once per `(file, hint)` and reuses it for every round. Two uses:
/// * [`HintIndex::any`] decides whether the file stays in the sweep at all: with
///   no reachable quad no code hit of its own can pass the pre-fit matched-star
///   gate of `solver.rs::process_hit`, so the file never reaches a fit and
///   `sweep.rs` drops it from the later rounds;
/// * [`HintIndex::codes`] is the same subset as a small tree over only the
///   reachable code records, which is what the queries of a hinted attempt search
///   instead of the index's own (full) codes tree.
pub(crate) struct HintIndex {
    /// Whether the file holds at least one reachable quad.
    pub any: bool,
    /// The reachable quads as a tree, when the caller wanted one, there are any,
    /// and the tree could be built (a subset too large to copy is not worth a
    /// tree, see [`crate::params::HINT_TREE_MAX_RECORDS`], and the sweep's budget
    /// for how many files get one). Without it the search stays on the index's
    /// own codes tree, which answers the same queries.
    pub codes: Option<HintCodes>,
}

/// The hint-reachable code records of one index file, as their own tree.
pub(crate) struct HintCodes {
    /// A tree over the reachable code records (`kdtree::KdTree::filtered`): same
    /// quantized rows, same external-unit geometry, fewer records to descend.
    pub tree: KdTree,
    /// Record `i` of `tree` is the code of this index quad.
    pub quad_of: Vec<u32>,
}

impl HintIndex {
    /// An index with (or without) a reachable quad, for the sweep tests that
    /// exercise the elimination rule without opening an index file.
    #[cfg(test)]
    pub(crate) fn for_test(any: bool) -> Self {
        Self { any, codes: None }
    }
}

impl Index {
    /// Metadata of `path`, or `None` when the file is not an astrometry.net
    /// index at all (e.g. a plain image dropped into the index directory, or a
    /// file whose header blocks cannot be read); those are reported on stderr
    /// and skipped. An index in a format this solver cannot read is returned as
    /// `Err` and the caller decides what it means (see [`load_metas_in`] and
    /// [`Unusable`]).
    ///
    /// Only the header blocks are read: a directory scan must not pull the
    /// kd-tree data of every index into memory.
    pub fn try_load_meta(path: &Path) -> Result<Option<IndexMeta>> {
        let fits = match FitsHeaders::open(path) {
            Ok(fits) => fits,
            Err(e) => {
                eprintln!("Warning: skipping {}: {e}", path.display());
                return Ok(None);
            }
        };
        if fits.primary().get_str("AN_FILE").is_none() {
            eprintln!("Warning: {} is not an index file, skipping", path.display());
            return Ok(None);
        }
        Ok(Some(IndexMeta::from_fits(&fits, path).with_context(
            || format!("loading index {}", path.display()),
        )?))
    }

    /// Loads metadata, quads and both kd-trees.
    pub fn load(path: &Path) -> Result<Index> {
        Self::load_body(path).with_context(|| format!("loading index {}", path.display()))
    }

    /// Body of [`Self::load`]. Its own messages name the failing step, never
    /// `path`; the filename comes from the context `load` adds (once).
    fn load_body(path: &Path) -> Result<Index> {
        let fits = Fits::open(path)?;
        let meta = IndexMeta::from_fits(&fits, path).context("reading the index header")?;

        let quads = fits.blob_view("quads").context("reading the quads table")?;
        let quad_u32s = quads.u32_entries().context("quads table")?;
        // `NQUADS * DIMQUADS` can overflow `usize` when a corrupt header makes
        // NQUADS huge; checked_mul turns that into a clean error (exit 2).
        let expected_quads = meta.nquads.checked_mul(meta.dimquads).ok_or_else(|| {
            anyhow::anyhow!(
                "NQUADS {} * DIMQUADS {} overflows",
                meta.nquads,
                meta.dimquads
            )
        })?;
        ensure!(
            quad_u32s == expected_quads,
            "quads table has {quad_u32s} u32, expected {expected_quads}"
        );
        // One-time pass: a star index >= NSTARS would make `star_xyz` ->
        // `KdTree::coord` address bytes outside the star table later, on a
        // rayon worker - a slice-index panic, or whatever trailing padding the
        // offset happens to land on. Reject the corrupt index at load (exit 2).
        // One compare per u32 of the table (nquads * dimquads), cheap next to
        // loading the trees; it does fault the quads pages in, which stay clean
        // evictable page cache.
        ensure!(
            (0..quad_u32s).all(|i| (quads.uint_at(meta.endian, 4, i) as usize) < meta.nstars),
            "quads table references star index >= NSTARS ({})",
            meta.nstars
        );

        let codes = KdTree::load(&fits, "codes").context("reading the codes kd-tree")?;
        let stars = KdTree::load(&fits, "stars").context("reading the stars kd-tree")?;
        ensure!(
            codes.ndata == meta.nquads,
            "codes tree has {} records, NQUADS is {}",
            codes.ndata,
            meta.nquads
        );
        ensure!(
            stars.ndata == meta.nstars,
            "stars tree has {} records, NSTARS is {}",
            stars.ndata,
            meta.nstars
        );
        ensure!(
            codes.ndim == 2 * (meta.dimquads - 2),
            "codes tree ndim={} does not match DIMQUADS={}",
            codes.ndim,
            meta.dimquads
        );
        ensure!(stars.ndim == 3, "stars tree ndim={} is not 3", stars.ndim);

        let cutside = fits
            .header_by_ttype("kdtree_header_stars")
            .context("reading the stars kd-tree header")?
            .get_i64("CUTNSIDE")
            .unwrap_or(-1);

        Ok(Self {
            meta,
            quads,
            codes,
            stars,
            cutside,
        })
    }

    /// Star indices of quad `q`, in `(A, B, C[, D])` order, decoded from the
    /// mapped file. Slot 3 is 0 when `DIMQUADS == 3`; callers must only read
    /// `..meta.dimquads`.
    #[must_use]
    pub fn quad(&self, q: usize) -> [u32; 4] {
        let d = self.meta.dimquads;
        let mut ids = [0u32; 4];
        for (i, id) in ids[..d].iter_mut().enumerate() {
            *id = self.quads.uint_at(self.meta.endian, 4, q * d + i);
        }
        ids
    }

    /// Dequantized unit-sphere position of index star `i`.
    #[must_use]
    pub fn star_xyz(&self, i: usize) -> [f64; 3] {
        self.stars.coord3(i)
    }

    /// Computes [`HintIndex`] for a precomputed hint circle (the pair
    /// `PositionHint::precompute` produces), so the accept test here is the one
    /// the solver's pre-fit gate applies: star `i` is in when
    /// [`star_outside_hint`] reports it out of the circle, and quad `q` is
    /// reachable when all `dimquads` of its stars are in.
    ///
    /// The two passes walk the star and quad tables in full, which faults the
    /// pages they live on, so a sweep computes this at most once per index file
    /// per process (see `sweep.rs`) - and that full walk is also what makes the
    /// reachable subset cheap enough to be worth a tree of its own.
    ///
    /// `want_tree` asks for the compact tree over the reachable quads. A caller
    /// that has no room for one (`sweep.rs`'s budget over the sweep's files)
    /// passes `false`, and the quad walk then stops at the first reachable quad:
    /// without a tree to fill, `any` is all the verdict is, and one quad answers
    /// it.
    #[must_use]
    pub fn hint_index(&self, hint: &([f64; 3], f64), want_tree: bool) -> HintIndex {
        let d = self.meta.dimquads;
        // Star pass: one bit per star, in star-number order.
        let mut instar = vec![0u64; self.meta.nstars.div_ceil(64)];
        for i in 0..self.meta.nstars {
            if !star_outside_hint(&self.star_xyz(i), hint) {
                instar[i / 64] |= 1u64 << (i % 64);
            }
        }
        // Quad pass: reachable when no slot is missing from the star bits. The
        // slots past `dimquads` are the dim3 sentinel and stay unread. Collection
        // stops one past the size a tree is still worth building; without a tree
        // the first reachable quad is already the whole verdict.
        let cap = if want_tree {
            crate::params::HINT_TREE_MAX_RECORDS + 1
        } else {
            1
        };
        let mut quads: Vec<u32> = Vec::new();
        for q in 0..self.meta.nquads {
            let ids = self.quad(q);
            if ids[..d]
                .iter()
                .all(|&s| instar[s as usize / 64] & (1u64 << (s as usize % 64)) != 0)
            {
                quads.push(q as u32);
                if quads.len() >= cap {
                    break;
                }
            }
        }
        let codes = if want_tree {
            self.codes
                .filtered(&quads)
                .map(|(tree, quad_of)| HintCodes { tree, quad_of })
        } else {
            None
        };
        HintIndex {
            any: !quads.is_empty(),
            codes,
        }
    }

    /// Text dump printed by the hidden `--dump-index` flag.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("file: {}\n", self.meta.path.display()));
        s.push_str(&format!(
            "  AN_FILE=QUAD  INDEXID={}  HEALPIX={}  HPNSIDE={}  ALLSKY={}  ENDIAN={}\n",
            self.meta.indexid,
            self.meta.healpix,
            self.meta.hpnside,
            self.meta.allsky,
            endian_name(self.meta.endian),
        ));
        s.push_str(&format!(
            "  DIMQUADS={}  NQUADS={}  NSTARS={}\n",
            self.meta.dimquads, self.meta.nquads, self.meta.nstars
        ));
        s.push_str(&format!(
            "  scale: {:.6} - {:.6} arcsec  (radians {:.12} - {:.12})\n",
            self.meta.scale_lower_arcsec,
            self.meta.scale_upper_arcsec,
            self.meta.scale_lower_arcsec / RAD2ARCSEC,
            self.meta.scale_upper_arcsec / RAD2ARCSEC,
        ));
        s.push_str(&format!("  CUTNSIDE (stars tree) = {}\n", self.cutside));
        s.push_str(&self.codes.describe());
        s.push_str(&self.stars.describe());
        // Spot check of the dequantization: a handful of index stars as RA/Dec.
        if self.meta.nstars > 0 {
            s.push_str("  sample stars (dequantized xyz -> RA,Dec):\n");
            let mut ids = vec![0usize, self.meta.nstars / 2, self.meta.nstars - 1];
            ids.sort_unstable();
            ids.dedup();
            for i in ids {
                let (ra, dec) = crate::coords::xyz_to_radec(self.star_xyz(i));
                s.push_str(&format!("    star {i}: ra={ra:.6} dec={dec:.6}\n",));
            }
        }
        s
    }
}

/// All `*.fits` files in `dir`, sorted by name. A directory without a single one
/// is an error; `metas_in` is the caller that may turn it into a warning.
pub fn index_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("cannot read directory {}: {e}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let is_fits = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("fits"));
        if !is_fits {
            continue;
        }
        // `fs::metadata` follows symlinks (unlike `DirEntry::file_type`), so a
        // symlink-to-a-regular-file index is accepted; `is_file` skips directories,
        // broken symlinks and non-regular files (a FIFO would block `fs::read`).
        if fs::metadata(&path).is_ok_and(|md| md.is_file()) {
            files.push(path);
        }
    }
    files.sort();
    ensure!(
        !files.is_empty(),
        "no .fits index files found in {}",
        dir.display()
    );
    Ok(files)
}

/// Index sweep order: largest `scale_upper_arcsec` first, ties broken by the
/// larger `indexid`, then (for equal-scale tiles of one index) by ascending
/// `healpix` so the tile order is deterministic and not just a stable-sort
/// coincidence.
///
/// Ordering by quad size instead of by file name is a project decision: the
/// indices that cover the widest fields are tried first, and the sweep order is
/// a property of the indices rather than of how their files happen to be named.
pub fn sweep_cmp(a: &IndexMeta, b: &IndexMeta) -> std::cmp::Ordering {
    b.scale_upper_arcsec
        .total_cmp(&a.scale_upper_arcsec)
        .then(b.indexid.cmp(&a.indexid))
        .then(a.healpix.cmp(&b.healpix))
}

/// Duplicated indices: `(indexid, count)` for every run of adjacent `metas`
/// entries sharing `(indexid, healpix)`. `metas` must already be in `sweep_cmp`
/// order: it sorts by `SCALE_U` first, so copies of one index are adjacent only
/// while that bound matches; copies that sort apart with another index between
/// them go unreported.
fn duplicate_indexids(metas: &[IndexMeta]) -> Vec<(i64, usize)> {
    metas
        .chunk_by(|a, b| a.indexid == b.indexid && a.healpix == b.healpix)
        .filter(|group| group.len() > 1)
        .map(|group| (group[0].indexid, group.len()))
        .collect()
}

/// What to do when an index directory cannot contribute an index: either a file
/// that IS an astrometry.net index but in a format this solver cannot read
/// (the new index format, H4, a `HPNCOMP=NESTED` tile), or a directory that
/// cannot be scanned at all (unreadable, or without any `.fits` file).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unusable {
    /// Fail the run (exit 2): the directory came from `--index-dir`, so the user
    /// asked for exactly this content.
    Fail,
    /// Warn and go on: the directory was discovered in a system astrometry.net
    /// install, whose data directory mixes in indices we do not support.
    Skip,
}

/// One warning per unusable index file, up to this many; the rest are only
/// counted. A system data directory may hold many files this solver cannot
/// read, and one line each would bury the real output.
const MAX_UNUSABLE_WARNINGS: usize = 3;

/// Metadata of every index file in `dir`, handled per `unusable`.
fn metas_in(dir: &Path, unusable: Unusable) -> Result<Vec<IndexMeta>> {
    let files = match index_files(dir) {
        Ok(files) => files,
        Err(e) => match unusable {
            Unusable::Fail => return Err(e),
            Unusable::Skip => {
                eprintln!("Warning: {e:#}, skipping");
                return Ok(Vec::new());
            }
        },
    };
    let mut metas = Vec::new();
    let mut skipped = 0usize;
    for path in files {
        match Index::try_load_meta(&path) {
            Ok(Some(meta)) => metas.push(meta),
            // Not an index file at all: `try_load_meta` already warned.
            Ok(None) => {}
            Err(e) => match unusable {
                Unusable::Fail => return Err(e),
                Unusable::Skip => {
                    skipped += 1;
                    if skipped <= MAX_UNUSABLE_WARNINGS {
                        eprintln!("Warning: skipping {}: {}", path.display(), e.root_cause());
                    }
                }
            },
        }
    }
    if skipped > MAX_UNUSABLE_WARNINGS {
        eprintln!(
            "Warning: {skipped} index files in {} are unusable and were skipped",
            dir.display()
        );
    }
    Ok(metas)
}

/// `, `-joined paths, for messages.
pub(crate) fn path_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Loads metadata of every index in `dirs`, ordered from the largest quad scale
/// to the smallest (a project decision, see [`sweep_cmp`]).
///
/// Files that are not astrometry.net indices at all are skipped with a warning
/// on stderr (see [`Index::try_load_meta`]). Everything else that cannot be
/// used (see [`Unusable`]) is a hard error for a directory named by
/// `--index-dir` and a warning for a directory discovered in a system install.
pub fn load_metas_in(dirs: &[PathBuf], unusable: Unusable) -> Result<Vec<IndexMeta>> {
    ensure!(
        !dirs.is_empty(),
        "no index directories to scan (pass --index-dir)"
    );
    let mut metas = Vec::new();
    for dir in dirs {
        metas.extend(metas_in(dir, unusable)?);
    }
    ensure!(
        !metas.is_empty(),
        "no supported astrometry.net index found in {} (old-format QUAD indices: all-sky or \
         xy-scheme healpix tiles)",
        path_list(dirs)
    );
    metas.sort_by(sweep_cmp);
    // The sweep tries every entry in turn, so a duplicated INDEXID means the
    // same index is solved twice, and the verbose per-index lines print
    // INDEXID plus the cell, so the copies would print identically. Warn once
    // per duplicate; deduping is left to the user.
    let where_ = path_list(dirs);
    for (indexid, count) in duplicate_indexids(&metas) {
        eprintln!(
            "Warning: indexid {indexid} appears {count} times in {where_}: the same index will \
             be solved more than once"
        );
    }
    Ok(metas)
}

/// The tests take index files from the astrometry.net index set this machine
/// provides, through the `testutil` selectors; every selector reports a skip
/// and returns `None` when no index data is available.
#[cfg(test)]
use crate::testutil;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::{deg2arcsec, deg2distsq, distsq, radec_to_xyz, xyz_to_radec};

    /// The quads-table decoding, as an invariant over whichever installed
    /// index the selector returns: every sampled row addresses the star table,
    /// and the slots past `DIMQUADS` read back as `0` - the sentinel of a dim3
    /// quad (`Index::quad` zero-fills them, so this leg pins the accessor's
    /// contract, not the file). Rows are walked on a fixed stride, not at
    /// random.
    #[test]
    fn quad_rows_address_the_star_table() {
        let Some(meta) = testutil::smallest_index(0) else {
            return;
        };
        let index = testutil::load(meta);
        let d = index.meta.dimquads;
        let stride = usize::max(1, index.meta.nquads / 300);
        let mut sampled = 0;
        for q in (0..index.meta.nquads).step_by(stride) {
            sampled += 1;
            let quad = index.quad(q);
            for &s in &quad[..d] {
                assert!(
                    (s as usize) < index.meta.nstars,
                    "quad {q}: star {s} >= nstars"
                );
            }
            assert!(
                quad[d..].iter().all(|&s| s == 0),
                "quad {q}: slot past DIMQUADS is not the 0 sentinel"
            );
        }
        assert!(sampled > 1, "stride sampled only {sampled} quads");
    }

    /// The kd-tree range tables. Dequantization is
    /// `q * invscale + minval[dim]` with `invscale = 1 / scale`, so `scale` is
    /// the reciprocal of the quantization step, set by the window and the data
    /// width. Read from the smallest index the selector finds and asserted
    /// bit-exactly: the stars tree spans the unit cube the star directions live
    /// in, in 32-bit levels (step 2 / (2^32 - 1), scale 2147483647.5), and the
    /// codes tree the bounding box of the code circle, from (1 - sqrt2)/2 to
    /// (1 + sqrt2)/2, in 16-bit levels. A shifted window or a different bit
    /// width fails here.
    #[test]
    fn tree_ranges_match_the_code_circle() {
        let Some(meta) = testutil::smallest_index(0) else {
            return;
        };
        let index = testutil::load(meta);
        for (tree, minval, maxval, scale) in [
            (
                &index.stars,
                &[-1.0, -1.0, -1.0][..],
                &[1.0, 1.0, 1.0][..],
                2147483647.5,
            ),
            (
                &index.codes,
                &[-0.20710678118654757, -0.20710678118654757][..],
                &[1.2071067811865475, 1.2071067811865475][..],
                46340.2429050604,
            ),
        ] {
            for d in 0..tree.ndim {
                assert_eq!(tree.minval[d], minval[d], "{} minval", tree.name);
                assert_eq!(tree.maxval[d], maxval[d], "{} maxval", tree.name);
            }
            assert_eq!(tree.scale, scale, "{} scale", tree.name);
        }
    }

    /// A deterministic sample of an installed index' stars: `star_xyz` must
    /// dequantize to a unit vector whose RA/Dec round-trips back through
    /// `radec_to_xyz` (a dequantization regression breaks it; a coherent
    /// `coords.rs` convention flip moves both sides of the round trip and is
    /// caught by the sky-truth tests instead). Stars are walked on a fixed
    /// stride.
    #[test]
    fn star_positions_roundtrip_via_radec() {
        let Some(meta) = testutil::smallest_index(0) else {
            return;
        };
        let index = testutil::load(meta);
        let stride = usize::max(1, index.meta.nstars / 300);
        let mut sampled = 0;
        for i in (0..index.meta.nstars).step_by(stride) {
            sampled += 1;
            let xyz = index.star_xyz(i);
            let norm = f64::sqrt(xyz[0] * xyz[0] + xyz[1] * xyz[1] + xyz[2] * xyz[2]);
            assert!((norm - 1.0).abs() < 1e-6, "star {i}: |xyz| = {norm}");
            let (ra, dec) = xyz_to_radec(xyz);
            assert!(
                (0.0..360.0).contains(&ra) && (-90.0..=90.0).contains(&dec),
                "star {i}: ra={ra} dec={dec} out of range"
            );
            let back = radec_to_xyz(ra, dec);
            let dot = xyz[0] * back[0] + xyz[1] * back[1] + xyz[2] * back[2];
            assert!(
                (dot - 1.0).abs() < 1e-6,
                "star {i}: radec round-trip, dot = {dot}"
            );
        }
        assert!(sampled > 1, "stride sampled only {sampled} stars");
    }

    /// The hint index must agree with the pre-fit matched-star gate it stands in
    /// for: star `i` counts when `!(d > r2) && !d.is_nan()` for
    /// `d = distsq(star_xyz(i), center)`, and a quad is reachable exactly when
    /// every one of its `dimquads` slots counts. Checked against that formula over
    /// the whole file, both for the yes/no verdict and for the record list the
    /// compact tree is built from.
    ///
    /// Two of the circles are chosen so the verdict does not depend on what the
    /// catalog holds: a wide circle drawn around a star of the file accepts every
    /// quad, while a circle of radius 59.9 deg drawn around the center of the
    /// sphere rather than on its surface has every unit-length star at distance
    /// 1 from it, outside the radius, so it accepts nothing. The third is the
    /// interesting case: a circle that takes in part of the file.
    #[test]
    fn hint_index_matches_the_star_by_star_gate() {
        let Some(meta) = testutil::smallest_index(0) else {
            return;
        };
        let index = testutil::load(meta);
        let d = index.meta.dimquads;
        let (ra, dec) = xyz_to_radec(index.star_xyz(index.quad(0)[0] as usize));
        let mid = index.star_xyz(index.meta.nstars / 2);
        for (center, r2, all, what) in [
            (
                radec_to_xyz(ra, dec),
                deg2distsq(179.9),
                Some(true),
                "a circle that spans the whole sky",
            ),
            (
                [0.0f64; 3],
                deg2distsq(59.9),
                Some(false),
                "a circle around the center of the sphere",
            ),
            (mid, deg2distsq(6.0), None, "a narrow circle"),
        ] {
            let hint = (center, r2);
            // The gate re-stated here rather than called: `star_outside_hint` is
            // what this test must be able to contradict, negation and NaN
            // behaviour included.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            let star_in = |s: u32| {
                let dd = distsq(&index.star_xyz(s as usize), &hint.0);
                !(dd > hint.1) && !dd.is_nan()
            };
            let want: Vec<u32> = (0..index.meta.nquads)
                .filter(|&q| index.quad(q)[..d].iter().all(|&s| star_in(s)))
                .map(|q| q as u32)
                .collect();
            let got = index.hint_index(&hint, true);
            assert_eq!(got.any, !want.is_empty(), "{what}: any = {}", got.any);
            if let Some(all) = all {
                assert_eq!(got.any, all, "{what}: any = {}", got.any);
            }
            // The cheaper call of a caller that has no room for a tree: the same
            // verdict, reached without collecting the quads or building anything.
            let bare = index.hint_index(&hint, false);
            assert_eq!(
                bare.any, got.any,
                "{what}: want_tree=false changed the verdict"
            );
            assert!(
                bare.codes.is_none(),
                "{what}: want_tree=false still built a tree"
            );
            let Some(codes) = &got.codes else {
                assert!(
                    want.is_empty() || want.len() > crate::params::HINT_TREE_MAX_RECORDS,
                    "{what}: {} reachable quads and no tree",
                    want.len()
                );
                continue;
            };
            assert_eq!(
                codes.tree.ndata,
                usize::min(want.len(), crate::params::HINT_TREE_MAX_RECORDS),
                "{what}: the tree holds a different number of records than the gate reaches"
            );
            let mut listed = codes.quad_of.clone();
            listed.sort_unstable();
            assert_eq!(
                listed,
                want.iter()
                    .copied()
                    .take(crate::params::HINT_TREE_MAX_RECORDS)
                    .collect::<Vec<_>>(),
                "{what}: the tree's records are not the quads the gate reaches"
            );
        }
    }

    /// Self-consistency over the installed set (headers for all, full load for
    /// the few smallest): every header the loader accepted must satisfy the
    /// rules it enforces, and a loaded file's tables must agree with its own
    /// headers. A parser regression breaks one of these invariants; no per-file
    /// numbers are pinned.
    #[test]
    fn installed_indices_are_self_consistent() {
        let Some(all) = testutil::metas() else {
            return;
        };
        for m in all {
            // The loader refuses a non-CIRCLE index, so every usable meta has it.
            assert!(m.circle, "index {}: CIRCLE must be set", m.indexid);
            assert_eq!(
                m.healpix < 0,
                m.allsky,
                "index {}: HEALPIX={} contradicts ALLSKY={}",
                m.indexid,
                m.healpix,
                m.allsky
            );
            if m.healpix >= 0 {
                assert!(
                    m.hpnside > 0 && u64::try_from(m.hpnside).is_ok_and(u64::is_power_of_two),
                    "index {} tile {}: HPNSIDE={} is not a power of two > 0",
                    m.indexid,
                    m.healpix,
                    m.hpnside
                );
            }
            assert!(
                m.scale_lower_arcsec < m.scale_upper_arcsec,
                "index {}: empty scale band",
                m.indexid
            );
        }
        // Full load of the few smallest installed files (cheap by design):
        // the tables must agree with the headers, and `lr` is read as `u32`,
        // monotonic, ending at `ndata - 1`, with the leaves tiling [0, ndata).
        // `metas()` is non-empty here, so the loop runs at least once.
        let Some(files) = testutil::smallest_indices(3) else {
            return;
        };
        for &m in &files {
            let index = testutil::load(m);
            assert_eq!(
                index.quads.u32_entries().unwrap(),
                m.nquads * m.dimquads,
                "index {}: quads table rows disagree with NQUADS * DIMQUADS",
                m.indexid
            );
            assert_eq!(
                index.stars.ndata, m.nstars,
                "index {}: stars tree ndata disagrees with NSTARS",
                m.indexid
            );
            assert_eq!(
                index.codes.ndata, m.nquads,
                "index {}: codes tree ndata disagrees with NQUADS",
                m.indexid
            );
            for tree in [&index.codes, &index.stars] {
                assert!(
                    (1..tree.nbottom).all(|j| tree.lr_entry(j - 1) <= tree.lr_entry(j)),
                    "index {} tree {}: lr is not monotonic",
                    m.indexid,
                    tree.name
                );
                assert_eq!(
                    tree.lr_entry(tree.nbottom - 1) as usize,
                    tree.ndata - 1,
                    "index {} tree {}: last lr",
                    m.indexid,
                    tree.name
                );
                tree.check_structure()
                    .unwrap_or_else(|e| panic!("index {} tree {}: {e}", m.indexid, tree.name));
                tree.self_check()
                    .unwrap_or_else(|e| panic!("index {} tree {}: {e}", m.indexid, tree.name));
            }
        }
    }

    /// Index scale selection: a narrow scale hint keeps only the indices whose
    /// quad-size band overlaps the one the field can produce, and the survivors
    /// keep the sweep order. Asserted over a synthetic scale ladder, so it
    /// needs no index files on disk.
    #[test]
    fn index_scale_selection() {
        let mut metas = vec![
            meta_with_scale(4110, 3600.0, 5100.0),
            meta_with_scale(4111, 5100.0, 7200.0),
            meta_with_scale(4112, 7200.0, 10200.0),
            meta_with_scale(4113, 10200.0, 14400.0),
            meta_with_scale(4114, 14400.0, 20400.0),
            meta_with_scale(4115, 20400.0, 28800.0),
        ];
        metas.sort_by(sweep_cmp);
        // `sweep.rs` sweeps the meta list as-is, so it must arrive sorted.
        assert_eq!(meta_ids(&metas), vec![4115, 4114, 4113, 4112, 4111, 4110]);
        let (w, h) = (4656.0, 3522.0);
        // A frame 4656 px wide covering 3.5 deg: 2.71 arcsec/px.
        let app = deg2arcsec(3.5) / w;
        let chosen: Vec<i64> = metas
            .iter()
            .filter(|m| m.fits_scale(app, app, w, h))
            .map(|m| m.indexid)
            .collect();
        assert_eq!(
            chosen,
            vec![4114, 4113, 4112, 4111, 4110],
            "a 3.5 deg field cannot be solved by the 20400-28800\" index",
        );
        // The blind-mode range (the field width spanning 0.1 - 180 deg) is wide
        // enough that every band above overlaps it.
        let all: Vec<i64> = metas
            .iter()
            .filter(|m| m.fits_scale(deg2arcsec(0.1) / w, deg2arcsec(180.0) / w, w, h))
            .map(|m| m.indexid)
            .collect();
        assert_eq!(all.len(), 6);
    }

    // ---- Order and scale selection, without index files -------------------

    /// A hand-built meta for the ordering/scale/duplicate tests; only the id,
    /// healpix and scale bounds are read by them.
    fn meta_with_scale(indexid: i64, lower_arcsec: f64, upper_arcsec: f64) -> IndexMeta {
        IndexMeta {
            path: PathBuf::from(format!("index-{indexid}.fits")),
            indexid,
            dimquads: 3,
            nquads: 1,
            nstars: 2,
            scale_lower_arcsec: lower_arcsec,
            scale_upper_arcsec: upper_arcsec,
            healpix: -1,
            hpnside: 0,
            allsky: true,
            endian: Endian::Little,
            circle: true,
            cx_less_than_dx: false,
            meanx_less_than_half: false,
        }
    }

    fn meta_ids(metas: &[IndexMeta]) -> Vec<i64> {
        metas.iter().map(|m| m.indexid).collect()
    }

    /// The catalog surface density behind the probe's chance-coincidence rate:
    /// an all-sky index spreads its stars over the whole sphere, a healpix tile
    /// over `1 / (12 nside^2)` of it, so the same star count means densities two
    /// orders of magnitude apart. `None` (never 0) when the file gives nothing
    /// to divide by.
    #[test]
    fn stars_per_deg2_divides_by_the_sky_the_index_covers() {
        let allsky = |nstars: usize| IndexMeta {
            nstars,
            ..meta_with_scale(4211, 0.0, 1.0)
        };
        let tile = |nstars: usize, nside: i64| IndexMeta {
            nstars,
            ..meta_tile(4204, 3, nside, 0.0, 1.0)
        };
        // 182520 stars over 41252.96 deg^2.
        assert!((allsky(182520).stars_per_deg2().unwrap() - 4.4244).abs() < 1e-4);
        // nside 2 -> 48 equal-area cells -> 859.4 deg^2 per tile.
        assert!((tile(484000, 2).stars_per_deg2().unwrap() - 563.16).abs() < 1e-2);
        // A coarser grid spreads the same stars over more sky: nside 1 is 4x
        // the area of nside 2, so a quarter of the density.
        assert!(
            (tile(484000, 1).stars_per_deg2().unwrap() - 563.16 / 4.0).abs() < 1e-2,
            "nside 1 must dilute the same star count over 4x the sky"
        );
        // No star count, or a tile with no usable nside, is "no estimate".
        assert_eq!(allsky(0).stars_per_deg2(), None);
        assert_eq!(tile(484000, 0).stars_per_deg2(), None);
    }

    /// A healpix sub-region tile of `indexid`/`healpix` with the given scale
    /// bounds, for the tile ordering/dedup tests.
    fn meta_tile(indexid: i64, healpix: i64, nside: i64, lower: f64, upper: f64) -> IndexMeta {
        IndexMeta {
            healpix,
            hpnside: nside,
            allsky: false,
            ..meta_with_scale(indexid, lower, upper)
        }
    }

    /// `sweep_cmp` is a project decision (largest quad size first), asserted
    /// directly here; `index_scale_selection` sweeps the same order over its
    /// synthetic scale ladder.
    #[test]
    fn sweep_cmp_orders_by_largest_quad_scale() {
        let mut metas = vec![
            meta_with_scale(4111, 5100.0, 7200.0),
            meta_with_scale(4115, 20400.0, 28800.0),
            meta_with_scale(4110, 3600.0, 5100.0),
            meta_with_scale(4113, 10200.0, 14400.0),
        ];
        metas.sort_by(sweep_cmp);
        assert_eq!(meta_ids(&metas), vec![4115, 4113, 4111, 4110]);
    }

    /// Equal quad sizes fall back to the larger `indexid` first, and the
    /// comparison is a total order (`load_metas_in` relies on both).
    #[test]
    fn sweep_cmp_breaks_scale_ties_by_larger_indexid() {
        let mut metas = vec![
            meta_with_scale(4110, 1.0, 7200.0),
            meta_with_scale(4112, 1.0, 7200.0),
            meta_with_scale(4111, 1.0, 7200.0),
        ];
        metas.sort_by(sweep_cmp);
        assert_eq!(meta_ids(&metas), vec![4112, 4111, 4110]);
        assert_eq!(sweep_cmp(&metas[0], &metas[0]), std::cmp::Ordering::Equal);
    }

    /// Tiles of one indexid with equal scale bounds order by ascending
    /// `healpix` (the final `sweep_cmp` tie-break), and are not duplicates.
    #[test]
    fn sweep_cmp_orders_equal_tiles_by_ascending_healpix() {
        let mut metas = vec![
            meta_tile(4206, 5, 1, 960.0, 1320.0),
            meta_tile(4206, 0, 1, 960.0, 1320.0),
            meta_tile(4206, 2, 1, 960.0, 1320.0),
        ];
        metas.sort_by(sweep_cmp);
        let hp: Vec<i64> = metas.iter().map(|m| m.healpix).collect();
        assert_eq!(hp, vec![0, 2, 5]);
        assert!(
            duplicate_indexids(&metas).is_empty(),
            "distinct tiles of one indexid must not warn as duplicates"
        );
        // Same tile twice IS a duplicate.
        let mut dup = metas.clone();
        dup.push(metas[0].clone());
        dup.sort_by(sweep_cmp);
        assert_eq!(duplicate_indexids(&dup), vec![(4206, 2)]);
    }

    /// The edges of `fits_scale` on a 1024x1920 field. The frame is an 8-15-17
    /// triangle scaled by 128, so `hypot` is exactly 2176 and `QSF_LO * min(w,
    /// h)` exactly 102.4; with `QSF_HI` at 1.0 the boundary products below are
    /// exact too.
    #[test]
    fn fits_scale_below_within_and_above_the_index_scale() {
        let m = meta_with_scale(1, 34.0, 409.6);
        // fmin = 102.4 * 4.25, above the index upper bound of 409.6: the biggest
        // quad this index holds is smaller than the smallest the field can
        // produce.
        assert!(!m.fits_scale(4.25, 1.0, 1024.0, 1920.0));
        // Exactly at the upper bound (102.4 * 4.0 = 409.6): inclusive.
        assert!(m.fits_scale(4.0, 1.0, 1024.0, 1920.0));
        // fmax = 2176 * 0.01 = 21.76 below the index lower bound: even the
        // smallest quad of the index overshoots the whole field.
        assert!(!m.fits_scale(0.001, 0.01, 1024.0, 1920.0));
        // Exactly at the lower bound (2176 * 0.015625 = 34): also inclusive.
        assert!(m.fits_scale(0.001, 0.015625, 1024.0, 1920.0));
        // Anything else comfortably inside the index range is a candidate.
        assert!(m.fits_scale(1.0, 1.0, 1024.0, 1920.0));
    }

    /// The duplicate-index warning of `load_metas_in`: one entry per duplicated
    /// `indexid` after the sweep sort, and nothing for distinct ids (a set of
    /// distinct indices, like a normal install, must stay silent).
    #[test]
    fn duplicate_indexids_are_reported_once_per_index() {
        let mut metas = vec![
            meta_with_scale(4111, 5100.0, 7200.0),
            meta_with_scale(4112, 7200.0, 10200.0),
            meta_with_scale(4111, 5100.0, 7200.0),
            meta_with_scale(4113, 10200.0, 14400.0),
            meta_with_scale(4111, 5100.0, 7200.0),
            meta_with_scale(4112, 7200.0, 10200.0),
        ];
        metas.sort_by(sweep_cmp);
        assert_eq!(
            duplicate_indexids(&metas),
            vec![(4112, 2), (4111, 3)],
            "one entry per duplicated indexid, with its copy count"
        );

        let distinct = vec![
            meta_with_scale(4115, 20400.0, 28800.0),
            meta_with_scale(4114, 14400.0, 20400.0),
            meta_with_scale(4113, 10200.0, 14400.0),
        ];
        assert!(
            duplicate_indexids(&distinct).is_empty(),
            "distinct indexids must not warn"
        );
    }

    // ---- Crafted-bytes regression tests for the load-time guards. ----------
    // None of these need an installed index: they build minimal FITS bytes in
    // memory, so they run (and must pass) on a machine with no index data at all.
    // Each asserts a clean `Err`, never a panic.

    /// Accumulates 2880-byte FITS blocks so a test can hand `Fits`/`Index::load`
    /// a parseable file built from raw card strings.
    #[derive(Default)]
    struct FitsWriter {
        buf: Vec<u8>,
    }

    impl FitsWriter {
        /// Appends `cards` plus an `END` card, padded to a whole 2880-byte block.
        fn push_block(&mut self, cards: &[&str]) {
            let mut block: Vec<u8> = Vec::new();
            for card in cards {
                let mut c = card.as_bytes().to_vec();
                c.resize(crate::fits::CARD_SIZE, b' ');
                block.extend_from_slice(&c);
            }
            let mut end = b"END".to_vec();
            end.resize(crate::fits::CARD_SIZE, b' ');
            block.extend_from_slice(&end);
            let pad =
                usize::div_ceil(block.len(), crate::fits::BLOCK_SIZE) * crate::fits::BLOCK_SIZE;
            block.resize(pad, b' ');
            self.buf.extend_from_slice(&block);
        }
    }

    /// A valid allsky QUAD primary header. `Header::get` returns the last value,
    /// so tests override a key simply by repeating it after these cards.
    const GOOD_PRIMARY: [&str; 14] = [
        "SIMPLE  =                    T",
        "BITPIX  =                    8",
        "NAXIS   =                    0",
        "ENDIAN  = '04:03:02:01'",
        "AN_FILE = 'QUAD'",
        "HEALPIX =                   -1",
        "ALLSKY  =                    T",
        "DIMQUADS=                    3",
        "INDEXID =                    7",
        "NQUADS  =                    1",
        "NSTARS  =                    2",
        "SCALE_L = 0.001",
        "SCALE_U = 0.01",
        "CIRCLE  =                    T",
    ];

    /// A primary-only FITS carrying `GOOD_PRIMARY` plus `extra` override cards.
    fn make_meta_bytes(extra: &[&str]) -> Vec<u8> {
        let mut w = FitsWriter::default();
        let mut cards: Vec<&str> = GOOD_PRIMARY.to_vec();
        cards.extend_from_slice(extra);
        w.push_block(&cards);
        w.buf
    }

    /// A primary plus a single-row `quads` BLOB column holding `quads`. `overrides`
    /// replace header keys (last card wins).
    fn make_index_bytes(overrides: &[&str], quads: &[u32]) -> Vec<u8> {
        let mut w = FitsWriter::default();
        let mut primary: Vec<&str> = GOOD_PRIMARY.to_vec();
        primary.extend_from_slice(overrides);
        w.push_block(&primary);

        let naxis1 = quads.len() * 4;
        let naxis1_card = format!("NAXIS1  = {naxis1}");
        let tform1_card = format!("TFORM1  = '{naxis1}A'");
        let table = [
            "XTENSION= 'BINTABLE'",
            "BITPIX  =                    8",
            "NAXIS   =                    2",
            naxis1_card.as_str(),
            "NAXIS2  =                    1",
            "TFIELDS =                    1",
            tform1_card.as_str(),
            "TTYPE1  = 'quads'",
        ];
        w.push_block(&table);

        let mut data = Vec::new();
        for &v in quads {
            data.extend_from_slice(&u32::to_le_bytes(v));
        }
        let pad = usize::div_ceil(data.len(), crate::fits::BLOCK_SIZE) * crate::fits::BLOCK_SIZE;
        data.resize(pad, 0);
        w.buf.extend_from_slice(&data);
        w.buf
    }

    /// Writes `bytes` to a temp file, runs `Index::load`, then always removes
    /// the file. A `Result` is returned so the caller asserts `Err` (not panic).
    fn load_bytes(tag: &str, bytes: &[u8]) -> Result<Index> {
        let path =
            std::env::temp_dir().join(format!("ansolve_test_{tag}_{}.fits", std::process::id()));
        fs::write(&path, bytes).expect("write temp index file");
        let result = Index::load(&path);
        let _ = fs::remove_file(&path);
        result
    }

    /// `from_fits` must reject the crafted header with a clean `Err` (no panic).
    fn assert_meta_rejected(extra: &[&str]) {
        let fits = Fits::from_bytes(make_meta_bytes(extra)).expect("crafted primary should parse");
        let err = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).unwrap_err();
        eprintln!("rejected: {err}");
    }

    #[test]
    fn crafted_negative_counts_are_rejected() {
        assert_meta_rejected(&["NSTARS  =                   -1"]);

        let fits = Fits::from_bytes(make_meta_bytes(&["NQUADS  =                   -1"]))
            .expect("crafted primary should parse");
        let err = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).unwrap_err();
        let msg = err.to_string();
        // A bare `as usize` cast would have turned -1 into
        // 18446744073709551615; the guard must name the key instead.
        assert!(
            msg.contains("NQUADS") && msg.contains("non-negative"),
            "{msg}"
        );
    }

    #[test]
    fn crafted_overflowing_quad_count_is_rejected() {
        // NQUADS = i64::MAX, DIMQUADS = 4 would wrap `nquads * dimquads`.
        let bytes = make_index_bytes(
            &[
                "DIMQUADS=                    4",
                "NQUADS  =       9223372036854775807",
            ],
            &[0, 1, 1, 0],
        );
        let err = load_bytes("overflow", &bytes)
            .err()
            .expect("overflowing NQUADS * DIMQUADS must be rejected");
        // `to_string()` renders only the outer context; `{:#}` is the whole chain.
        let msg = format!("{err:#}");
        assert!(msg.contains("overflows"), "{msg}");
    }

    #[test]
    fn crafted_bad_scale_is_rejected() {
        assert_meta_rejected(&["SCALE_L = NaN"]);
        assert_meta_rejected(&["SCALE_U = NaN"]);
        assert_meta_rejected(&["SCALE_L =                   -1"]);
        assert_meta_rejected(&["SCALE_L = 0.0"]);
        assert_meta_rejected(&["SCALE_L = 0.02", "SCALE_U = 0.01"]);
    }

    #[test]
    fn crafted_scale_nan_reaches_the_guard_as_nan() {
        let fits = Fits::from_bytes(make_meta_bytes(&["SCALE_L = NaN"])).unwrap();
        assert!(
            fits.primary().get_f64("SCALE_L").unwrap().is_nan(),
            "the crafted NaN must parse as NaN, not fail to parse"
        );
        assert!(IndexMeta::from_fits(&fits, Path::new("crafted.fits")).is_err());
    }

    #[test]
    fn crafted_out_of_range_star_index_is_rejected() {
        // NSTARS 2, quads reference star 999999: the guard must fire (exit 2).
        let err = load_bytes("oor_star", &make_index_bytes(&[], &[0, 1, 999999]))
            .err()
            .expect("out-of-range star index must be rejected");
        assert!(
            format!("{err:#}").contains("quads table references"),
            "{err:#}"
        );
    }

    #[test]
    fn crafted_star_index_out_of_range_when_nstars_zero() {
        let bytes = make_index_bytes(&["NSTARS  =                    0"], &[0, 1, 1]);
        let err = load_bytes("nstars0", &bytes)
            .err()
            .expect("NSTARS 0 with any star reference must be rejected");
        assert!(
            format!("{err:#}").contains("quads table references"),
            "{err:#}"
        );
    }

    #[test]
    fn crafted_in_range_star_index_passes_the_guard() {
        // All star indices < NSTARS, so the guard stays silent; the crafted file
        // has no kd-tree tables, so `load` must fail later (not at the guard).
        let err = load_bytes("inrange", &make_index_bytes(&[], &[0, 1, 1]))
            .err()
            .expect("crafted file has no kd-tree tables");
        let msg = format!("{err:#}");
        assert!(
            !msg.contains("quads table references"),
            "guard over-rejected: {msg}"
        );
    }

    /// `GOOD_PRIMARY` without the cards starting with `prefix` (last card wins,
    /// so dropping a card is how a test exercises a missing key).
    fn good_primary_without(prefix: &str) -> Vec<&'static str> {
        GOOD_PRIMARY
            .iter()
            .copied()
            .filter(|c| !c.starts_with(prefix))
            .collect()
    }

    /// `from_fits` must reject the crafted header with a clean `Err` (no panic)
    /// whose message contains every needle.
    fn assert_meta_rejected_with(extra: &[&str], needles: &[&str]) {
        let fits = Fits::from_bytes(make_meta_bytes(extra)).expect("crafted primary should parse");
        let msg = IndexMeta::from_fits(&fits, Path::new("crafted.fits"))
            .unwrap_err()
            .to_string();
        for needle in needles {
            assert!(msg.contains(needle), "{needle} missing from: {msg}");
        }
    }

    #[test]
    fn crafted_circle_false_is_rejected() {
        // The solver applies the circle filter unconditionally, so an index
        // that does not declare the convention is refused at load rather than
        // trusted to solve by luck.
        let fits = Fits::from_bytes(make_meta_bytes(&["CIRCLE  =                    F"]))
            .expect("crafted primary should parse");
        let err = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).unwrap_err();
        assert!(err.to_string().contains("CIRCLE"), "{err}");
    }

    #[test]
    fn crafted_missing_circle_is_rejected() {
        // GOOD_PRIMARY declares CIRCLE=T; drop that card to exercise the
        // missing-key `unwrap_or(false)` fallback path.
        let mut w = FitsWriter::default();
        w.push_block(&good_primary_without("CIRCLE"));
        let fits = Fits::from_bytes(w.buf).expect("crafted primary should parse");
        let err = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).unwrap_err();
        assert!(err.to_string().contains("CIRCLE"), "{err}");
    }

    /// A plain FITS file (no `AN_FILE`) is refused by `from_fits`; turning that
    /// into a skip is `try_load_meta`'s job, not the header gate's.
    #[test]
    fn crafted_missing_an_file_is_rejected() {
        let mut w = FitsWriter::default();
        w.push_block(&good_primary_without("AN_FILE"));
        let fits = Fits::from_bytes(w.buf).expect("crafted primary should parse");
        let err = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).unwrap_err();
        assert!(err.to_string().contains("no AN_FILE"), "{err}");
    }

    #[test]
    fn crafted_wrong_an_file_type_is_rejected() {
        assert_meta_rejected_with(
            &["AN_FILE = 'INDEX'"],
            &[
                "unsupported index file type",
                "INDEX",
                "only 'QUAD' is supported",
            ],
        );
    }

    /// A healpix sub-region tile with the all-sky table layout is accepted
    /// (`HEALPIX>=0` + `HPNSIDE` power of two, `ALLSKY` false or absent),
    /// while contradictory or nonsensical combinations are refused.
    #[test]
    fn crafted_healpix_tile_is_accepted() {
        let fits = Fits::from_bytes(make_meta_bytes(&[
            "HEALPIX =                  123",
            "ALLSKY  =                    F",
            "HPNSIDE =                    4",
        ]))
        .expect("crafted primary should parse");
        let meta = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).expect("tile must load");
        assert_eq!((meta.healpix, meta.hpnside, meta.allsky), (123, 4, false));

        // A tile header carrying no ALLSKY key at all: `allsky` then defaults
        // to `healpix == -1`, i.e. false.
        let mut w = FitsWriter::default();
        let mut cards = good_primary_without("ALLSKY");
        cards.extend_from_slice(&[
            "HEALPIX =                    7",
            "HPNSIDE =                    2",
        ]);
        w.push_block(&cards);
        let fits = Fits::from_bytes(w.buf).expect("crafted primary should parse");
        let meta = IndexMeta::from_fits(&fits, Path::new("crafted.fits")).expect("tile must load");
        assert_eq!((meta.healpix, meta.hpnside, meta.allsky), (7, 2, false));
        assert!(
            meta.describe().contains("healpix=7 hpnside=2"),
            "{}",
            meta.describe()
        );
    }

    /// The contradictory/nonsensical tile combinations the gate must refuse.
    #[test]
    fn crafted_bad_healpix_tiles_are_rejected() {
        // HEALPIX >= 0 but ALLSKY=T: contradictory.
        assert_meta_rejected_with(
            &[
                "HEALPIX =                    5",
                "HPNSIDE =                    2",
            ],
            &["contradictory", "ALLSKY"],
        );
        // Tile with no HPNSIDE key at all: treated as corrupt.
        assert_meta_rejected_with(
            &[
                "HEALPIX =                    5",
                "ALLSKY  =                    F",
            ],
            &["HPNSIDE"],
        );
        // Tile with HPNSIDE = 0.
        assert_meta_rejected_with(
            &[
                "HEALPIX =                    5",
                "ALLSKY  =                    F",
                "HPNSIDE =                    0",
            ],
            &["HPNSIDE"],
        );
        // Non-power-of-two HPNSIDE.
        assert_meta_rejected_with(
            &[
                "HEALPIX =                    5",
                "ALLSKY  =                    F",
                "HPNSIDE =                    3",
            ],
            &["power of two"],
        );
        // NESTED cell numbering, which this reader cannot interpret.
        assert_meta_rejected_with(
            &[
                "HEALPIX =                    5",
                "ALLSKY  =                    F",
                "HPNSIDE =                    2",
                "HPNCOMP = 'NESTED'",
            ],
            &["NESTED"],
        );
        assert_meta_rejected_with(
            &[
                "HEALPIX =                    5",
                "ALLSKY  =                    F",
                "HPNSIDE =                    2",
                "HPNCOMP = 'nested'",
            ],
            &["HPNCOMP"],
        );
        // Only NESTED is refused: a RING key is not checked at all, so this
        // nside=1 tile loads.
        let fits = Fits::from_bytes(make_meta_bytes(&[
            "HEALPIX =                    5",
            "ALLSKY  =                    F",
            "HPNSIDE =                    1",
            "HPNCOMP = 'RING'",
        ]))
        .expect("crafted primary should parse");
        assert!(IndexMeta::from_fits(&fits, Path::new("crafted.fits")).is_ok());
    }

    /// The remaining unsupported position-key spellings: an `ALLSKY=F`
    /// allsky-sentinel header and a negative-but-not--1 HEALPIX.
    #[test]
    fn crafted_unsupported_position_keys_are_rejected() {
        assert_meta_rejected_with(
            &["ALLSKY  =                    F"],
            &["unsupported index position keys"],
        );
        assert_meta_rejected_with(
            &["HEALPIX =                   -2"],
            &["unsupported index position keys"],
        );
    }

    #[test]
    fn crafted_unsupported_dimquads_is_rejected() {
        assert_meta_rejected_with(
            &["DIMQUADS=                    5"],
            &["unsupported DIMQUADS=5"],
        );
    }

    /// Load-error convention: the inner messages carry no path, the caller's
    /// `loading index {path}` context does, so a filename appears exactly once
    /// in the rendered error (for both a header gate and a table guard).
    #[test]
    fn load_errors_name_the_file_exactly_once() {
        let header_err = load_bytes(
            "once_hdr",
            &make_meta_bytes(&["SCALE_L = 0.02", "SCALE_U = 0.01"]),
        )
        .err()
        .expect("inverted SCALE_L/SCALE_U must be rejected");
        let table_err = load_bytes("once_table", &make_index_bytes(&[], &[0, 1, 999999]))
            .err()
            .expect("out-of-range star index must be rejected");
        for (tag, err) in [("once_hdr", &header_err), ("once_table", &table_err)] {
            let msg = format!("{err:#}");
            let name = format!("ansolve_test_{tag}_{}", std::process::id());
            assert!(msg.contains(&name), "{msg}");
            assert_eq!(msg.matches(&name).count(), 1, "path repeated: {msg}");
        }
    }

    #[test]
    fn crafted_circle_true_is_accepted() {
        let fits = Fits::from_bytes(make_meta_bytes(&[])).expect("crafted primary should parse");
        let meta =
            IndexMeta::from_fits(&fits, Path::new("crafted.fits")).expect("CIRCLE=T must load");
        assert!(meta.circle);
    }

    // ---- Directory scanning: the strict and the lenient policy --------------
    // (the lenient one is what a directory discovered in a system install gets).

    /// A temp directory of crafted files, removed with the guard.
    struct TempIndexDir(PathBuf);

    impl TempIndexDir {
        fn new(tag: &str, files: &[(&str, Vec<u8>)]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ansolve_dirtest_{tag}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create temp index dir");
            for (name, bytes) in files {
                fs::write(dir.join(name), bytes).expect("write temp index file");
            }
            Self(dir)
        }
    }

    impl Drop for TempIndexDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// An index header this solver refuses: a NESTED-scheme healpix tile.
    fn unusable_index_bytes() -> Vec<u8> {
        make_meta_bytes(&[
            "HEALPIX =                    0",
            "ALLSKY  =                    F",
            "HPNSIDE =                    2",
            "HPNCOMP = 'NESTED'",
        ])
    }

    #[test]
    fn an_unusable_index_is_fatal_only_for_a_named_directory() {
        let dir = TempIndexDir::new(
            "policy",
            &[
                ("index-good.fits", make_index_bytes(&[], &[0, 1, 1])),
                ("index-nested-tile.fits", unusable_index_bytes()),
            ],
        );
        let metas =
            load_metas_in(std::slice::from_ref(&dir.0), Unusable::Skip).expect("Skip must go on");
        assert_eq!(meta_ids(&metas), vec![7]);

        let err = load_metas_in(std::slice::from_ref(&dir.0), Unusable::Fail)
            .expect_err("an index of a directory named by --index-dir must not be dropped");
        assert!(format!("{err:#}").contains("NESTED"), "{err:#}");
    }

    /// A directory without any usable index is an error in both modes (exit 2);
    /// only the strict one reports the directory it could not read as such.
    #[test]
    fn an_unreadable_directory_fails_only_in_strict_mode() {
        let missing = PathBuf::from("/no/such/index-dir");
        let strict = load_metas_in(std::slice::from_ref(&missing), Unusable::Fail).unwrap_err();
        assert!(
            format!("{strict:#}").contains("cannot read directory"),
            "{strict:#}"
        );
        let lenient = load_metas_in(&[missing], Unusable::Skip).unwrap_err();
        let msg = format!("{lenient:#}");
        assert!(msg.contains("no supported astrometry.net index"), "{msg}");
        assert!(!msg.contains("cannot read directory"), "{msg}");
    }

    #[test]
    fn an_empty_directory_list_is_reported() {
        let err = load_metas_in(&[], Unusable::Skip).unwrap_err();
        assert!(format!("{err:#}").contains("--index-dir"), "{err:#}");
    }

    /// Indices of several directories are merged into one sweep-ordered list,
    /// so the sweep does not depend on the order the directories came from.
    #[test]
    fn metas_of_several_directories_are_merged_in_sweep_order() {
        let small = TempIndexDir::new(
            "merge_small",
            &[(
                "index.fits",
                make_index_bytes(&["INDEXID =                    1"], &[0, 1, 1]),
            )],
        );
        let large = TempIndexDir::new(
            "merge_large",
            &[(
                "index.fits",
                make_index_bytes(
                    &["INDEXID =                    2", "SCALE_U = 0.1"],
                    &[0, 1, 1],
                ),
            )],
        );
        let metas = load_metas_in(&[small.0.clone(), large.0.clone()], Unusable::Skip)
            .expect("both directories hold an index");
        assert_eq!(meta_ids(&metas), vec![2, 1]);
    }
}
