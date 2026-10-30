//! A kd-tree over the quantized coordinate arrays stored in an index file.
//!
//! The layout the loader reads and the searches walk:
//! * heap numbering: node 0 is the root, children of `i` are `2i+1` (left) and
//!   `2i+2` (right), every node `i >= ninterior` is leaf `node - ninterior`,
//!   `nnodes - ninterior = nbottom`;
//! * `lr[j]` is the *inclusive* right end of leaf `j`'s data range (always read as
//!   `u32`, regardless of `KDT_INT`): `[j == 0 ? 0 : lr[j-1] + 1, lr[j]]`, which
//!   `leaf_range` reports half-open so empty leaves stay empty;
//! * `split[i]` packs value and dimension: `dim = split & dimmask`,
//!   `value = split & !dimmask`, `dimmask = 2^ceil(log2(ndim)) - 1`;
//! * the split plane divides the halves: left child owns `value[dim] <= split`, right
//!   child owns `value[dim] >= split`, and every search prunes on that rule
//!   (`descend`, the near/far choice in `nearest_inner`) - halves stored the other way
//!   round would silently lose hits, which the tests' brute-force comparisons guard;
//! * dequantization: `value = q * invscale + minval[dim]`, `invscale = 1/scale`.
//!
//! Distances are L2 in the *external* (double) units: query and tolerance come in that
//! space (the solver queries the codes tree with a code vector and
//! `params::CODE_TOL`) and each component is dequantized before subtraction, never
//! compared in quantized units.

use anyhow::{Context, Result, bail, ensure};
use core::ops::ControlFlow;

use crate::fits::{BlobView, Endian, Fits, Form};

/// Integer kind of the `KDT_INT` / `KDT_DATA` arrays we support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntKind {
    U16,
    U32,
}

impl IntKind {
    pub fn from_name(s: &str) -> Result<Self> {
        match s.trim() {
            "u16" => Ok(IntKind::U16),
            "u32" => Ok(IntKind::U32),
            other => bail!("unsupported kd-tree integer type {other:?} (only u16/u32)"),
        }
    }

    pub fn size(self) -> usize {
        match self {
            IntKind::U16 => 2,
            IntKind::U32 => 4,
        }
    }
}

/// A search result: record index in the tree's data array, squared distance in
/// external units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub index: usize,
    pub dist2: f64,
}

/// A compact kd-tree over quantized records.
///
/// `lr`, `split` and `data` are lazy [`BlobView`] byte views into the mmap: no
/// index-sized anonymous buffer, so the memory cost is independent of the index
/// size. Dequantizing eagerly would trade the file's 2- or 4-byte integers for
/// 8-byte doubles - 4x the `u16` blob, 2x the `u32` one - more anonymous memory than
/// the peak-RSS budget for index work allows. Coordinates are therefore dequantized
/// on access (`q * invscale + minval[d]`).
#[derive(Debug)]
pub struct KdTree {
    pub name: String,
    pub ndim: usize,
    pub ndata: usize,
    pub nnodes: usize,
    /// Number of bottom-level (leaf) nodes: `(nnodes + 1) / 2`.
    pub nbottom: usize,
    /// Number of interior nodes: `nnodes - nbottom`.
    pub ninterior: usize,
    pub minval: Vec<f64>,
    pub maxval: Vec<f64>,
    pub scale: f64,
    pub invscale: f64,
    /// Integer kind of the `split` array (`KDT_INT`).
    pub int_kind: IntKind,
    /// Integer kind of the quantized data (`KDT_DATA`).
    pub data_kind: IntKind,
    /// Byte order of the raw blobs (from the file's `ENDIAN` key).
    endian: Endian,
    dimmask: u32,
    /// `KDT_LINL`: the leaf ranges are uniform, so no `lr` table is read.
    pub linear_lr: bool,
    /// Inclusive right ends of the leaf data ranges, `nbottom` u32 entries;
    /// `None` when `linear_lr`.
    lr: Option<BlobView>,
    /// Packed split value + dimension, `ninterior` `KDT_INT` entries;
    /// `None` when `ninterior == 0`.
    split: Option<BlobView>,
    /// The quantized records: `ndata` rows of `ndim * data_kind` integers.
    data: BlobView,
}

/// Upper bound on `KDT_NDIM`: a corrupt header would overflow the doubling in
/// `dimmask_for`, which in release (the multiply wraps to 0) never ends.
const MAX_NDIM: usize = 64;

/// The mask isolating the split dimension from a packed `split` entry:
/// `2^ceil(log2(ndim)) - 1`, the bits needed to number `ndim` dimensions (`3` for
/// `ndim = 4`, `7` for `ndim = 5`). For `ndim = 1` it is `0`: the entry holds the
/// value alone.
fn dimmask_for(ndim: usize) -> Result<u32> {
    ensure!((1..=MAX_NDIM).contains(&ndim), "kd-tree with ndim={ndim}");
    let mut val: u32 = 1;
    while (val as usize) < ndim {
        val *= 2;
    }
    Ok(val - 1)
}

/// DFS stack slots for the searches: pending nodes are pairwise incomparable, so at
/// most one per tree level is in flight, and a tree whose nodes fit a `u32` has at
/// most `u32::BITS` levels. The array is exactly that bound plus the push headroom
/// `descend` `debug_assert!`s; the zero-init is measurable in the hot searches, so it
/// carries no slack.
const SEARCH_STACK_SLOTS: usize = u32::BITS as usize + 2;

/// The record-distance strategy [`Scan::dist2`] dispatches on once per scan.
enum Dist<'a> {
    /// Flat little-endian blob with `ndim` 3 hoisted: fixed-array query/`minval` and
    /// a compile-time row stride, so the decode is a constant-sized unrolled loop.
    Fixed3 {
        raw: &'a [u8],
        q: [f64; 3],
        m: [f64; 3],
        e2: bool,
    },
    Fixed4 {
        raw: &'a [u8],
        q: [f64; 4],
        m: [f64; 4],
        e2: bool,
    },
    /// Any other layout or `ndim`: [`KdTree::dist2_cutoff`] per record (it keeps its
    /// own flat little-endian fast path).
    General,
}

/// Decode context for one traversal.
///
/// The layout (byte order, blob packing, element kind, `ndim`) is fixed for the
/// whole search, but the generic accessors re-derive the decode choice on every
/// element read: [`KdTree::dist2_cutoff`] per record, [`KdTree::split_at`] per node,
/// [`KdTree::leaf_range`] per leaf entry. `Scan` resolves those choices once, keeps
/// the `ndim` 3/4 record loop constant-sized, and works off indexed slices of the
/// once-taken raw byte views.
///
/// The fast legs must stay BIT-IDENTICAL to the accessors they replace; any other
/// layout falls through to the unchanged `KdTree` methods.
struct Scan<'a> {
    tree: &'a KdTree,
    query: &'a [f64],
    dist: Dist<'a>,
    /// Flat little-endian `split` blob (a raw view whenever present);
    /// `int_e2` decides the element width.
    split_raw: Option<&'a [u8]>,
    int_e2: bool,
    /// Flat little-endian `lr` blob (`u32` entries). `None` also covers
    /// `linear_lr`, which has no table at all.
    lr_raw: Option<&'a [u8]>,
}

/// The fixed-`ndim` flat little-endian record scan: exactly the arithmetic of
/// [`KdTree::dist2_cutoff`]'s flat path (`N` dimensions of `E`-byte quantized ints),
/// with the row bound and element-kind dispatch hoisted out.
#[inline(always)]
fn dist2_flat<const N: usize, const E: usize>(
    raw: &[u8],
    rec: usize,
    q: &[f64; N],
    m: &[f64; N],
    invscale: f64,
    cutoff: f64,
) -> f64 {
    let mut d2 = 0.0;
    let row = &raw[rec * (N * E)..(rec + 1) * (N * E)];
    for (d, c) in row.as_chunks::<E>().0.iter().enumerate() {
        // `E` is const, so only one of these decodes survives monomorphization.
        let quantized = if E == 2 {
            u32::from(u16::from_le_bytes(c[..2].try_into().expect("2-byte chunk")))
        } else {
            u32::from_le_bytes(c[..4].try_into().expect("4-byte chunk"))
        };
        let delta = q[d] - (f64::from(quantized) * invscale + m[d]);
        d2 += delta * delta;
        if d2 > cutoff {
            return f64::INFINITY;
        }
    }
    d2
}

impl<'a> Scan<'a> {
    /// Resolves the decode legs for `tree`. `query` must already be `tree.ndim` long,
    /// which both searches assert before constructing a `Scan`.
    fn new(tree: &'a KdTree, query: &'a [f64]) -> Self {
        let le = tree.endian == Endian::Little;
        let dist = if le && tree.data.is_flat() {
            let raw = tree.data.raw_bytes();
            let e2 = tree.data_kind == IntKind::U16;
            // `minval` is `ndim` long and `query` was checked against `ndim`, so
            // both conversions succeed or fail together.
            if let (Ok(q), Ok(m)) = (
                <[f64; 3]>::try_from(query),
                <[f64; 3]>::try_from(tree.minval.as_slice()),
            ) {
                Dist::Fixed3 { raw, q, m, e2 }
            } else if let (Ok(q), Ok(m)) = (
                <[f64; 4]>::try_from(query),
                <[f64; 4]>::try_from(tree.minval.as_slice()),
            ) {
                Dist::Fixed4 { raw, q, m, e2 }
            } else {
                Dist::General
            }
        } else {
            Dist::General
        };
        let split_raw = (le && tree.ninterior > 0)
            .then(|| {
                tree.split
                    .as_ref()
                    .expect("split table absent only when ninterior == 0")
            })
            .filter(|b| b.is_flat())
            .map(|b| b.raw_bytes());
        let lr_raw = (le && !tree.linear_lr)
            .then(|| {
                tree.lr
                    .as_ref()
                    .expect("lr table absent only when linear_lr")
            })
            .filter(|b| b.is_flat())
            .map(|b| b.raw_bytes());
        Self {
            tree,
            query,
            dist,
            split_raw,
            int_e2: tree.int_kind == IntKind::U16,
            lr_raw,
        }
    }

    /// Squared distance from the query to record `rec`, bailing out like
    /// [`KdTree::dist2_cutoff`] (and bit-identical to it).
    #[inline(always)]
    fn dist2(&self, rec: usize, cutoff: f64) -> f64 {
        let inv = self.tree.invscale;
        match &self.dist {
            Dist::Fixed3 { raw, q, m, e2 } => {
                if *e2 {
                    dist2_flat::<3, 2>(raw, rec, q, m, inv, cutoff)
                } else {
                    dist2_flat::<3, 4>(raw, rec, q, m, inv, cutoff)
                }
            }
            Dist::Fixed4 { raw, q, m, e2 } => {
                if *e2 {
                    dist2_flat::<4, 2>(raw, rec, q, m, inv, cutoff)
                } else {
                    dist2_flat::<4, 4>(raw, rec, q, m, inv, cutoff)
                }
            }
            Dist::General => self.tree.dist2_cutoff(rec, self.query, cutoff),
        }
    }

    /// Unpacks and dequantizes `split[node]` like [`KdTree::split_external`].
    #[inline]
    fn split_external(&self, node: usize) -> (usize, f64) {
        let Some(raw) = self.split_raw else {
            return self.tree.split_external(node);
        };
        let packed = if self.int_e2 {
            u32::from(u16::from_le_bytes(
                raw[node * 2..node * 2 + 2]
                    .try_into()
                    .expect("2-byte split entry"),
            ))
        } else {
            u32::from_le_bytes(
                raw[node * 4..node * 4 + 4]
                    .try_into()
                    .expect("4-byte split entry"),
            )
        };
        let dim = usize::try_from(packed & self.tree.dimmask).unwrap_or(usize::MAX);
        (
            dim,
            f64::from(packed & !self.tree.dimmask) * self.tree.invscale + self.tree.minval[dim],
        )
    }

    /// The half-open data range of leaf `j` like [`KdTree::leaf_range`].
    #[inline]
    fn leaf_range(&self, leaf: usize) -> (usize, usize) {
        let Some(raw) = self.lr_raw else {
            return self.tree.leaf_range(leaf);
        };
        assert!(
            leaf < self.tree.nbottom,
            "leaf {leaf} out of {}",
            self.tree.nbottom
        );
        // One past the inclusive `lr[j]`, as `leaf_range` reports it.
        let end_after = |j: usize| -> usize {
            usize::try_from(u32::from_le_bytes(
                raw[j * 4..j * 4 + 4].try_into().expect("4-byte lr entry"),
            ))
            .expect("u32 fits in usize")
                + 1
        };
        let left = if leaf == 0 { 0 } else { end_after(leaf - 1) };
        (left, end_after(leaf))
    }

    /// Pushes the node's children onto `stack`, near child first so a pop takes the
    /// far child first (a range search does not care about hit order). The far child
    /// is skipped once the gap along the split dimension exceeds `tol`: everything
    /// below it lies on the far side of the plane, so its distance is at least `|gap|`.
    /// A `gap < 0` puts the query below the plane, and the left child owns
    /// `value[dim] <= split`, so there the left child is the near one.
    #[inline]
    fn descend(&self, node: usize, tol: f64, stack: &mut [u32], sp: &mut usize) {
        debug_assert!(*sp + 2 <= stack.len(), "dfs stack overflow");
        let (dim, rsplit) = self.split_external(node);
        let gap = self.query[dim] - rsplit;
        let (near, far) = if gap < 0.0 {
            (2 * node + 1, 2 * node + 2)
        } else {
            (2 * node + 2, 2 * node + 1)
        };
        stack[*sp] = near as u32;
        *sp += 1;
        if f64::abs(gap) <= tol {
            stack[*sp] = far as u32;
            *sp += 1;
        }
    }
}

impl KdTree {
    /// Loads the tree whose header table is `kdtree_header_<name>`.
    pub fn load(fits: &Fits, name: &str) -> Result<Self> {
        let endian = fits.endian();
        let header = fits.header_by_ttype(&format!("kdtree_header_{name}"))?;

        let tree_name = header.require_str("KDT_NAME")?;
        ensure!(
            tree_name == name,
            "kd-tree named {tree_name:?} found where {name:?} was expected"
        );
        ensure!(
            header.get_str("KDT_EXT").unwrap_or("double") == "double",
            "unsupported kd-tree external type {:?} (only double)",
            header.get_str("KDT_EXT")
        );
        ensure!(
            header.get_i64("KDT_VER").unwrap_or(1) == 1,
            "unsupported kd-tree version {:?}",
            header.get_i64("KDT_VER")
        );
        // A tree is stored as the chunks header, lr, perm, bb, split, splitdim, data
        // and range; this loader reads header, lr, split, range and data. The three
        // rejected here each change how the rest must be read: `perm` renumbers the
        // records (a hit's row index would have to be mapped through it before it
        // names a record of the index), `bb` carries per-node bounding boxes to prune
        // on instead of the split planes, `splitdim` keeps the dimension in its own
        // array (freeing, so invalidating, the low bits of `split`). None is decoded,
        // so its presence is an error.
        for forbidden in ["bb", "splitdim", "perm"] {
            let table = format!("kdtree_{forbidden}_{name}");
            ensure!(
                fits.find_table(&table).is_none(),
                "unsupported index feature: kd-tree table {table:?}"
            );
        }

        // Sizes are validated before any arithmetic uses them: a negative card
        // through `as usize` becomes `usize::MAX` (and `as usize` truncates on 32-bit
        // targets), so the conversion here is `usize::try_from`; `ndim == 0` is
        // rejected here, not as a later blob-size mismatch.
        let ndim = usize::try_from(header.require_i64("KDT_NDIM")?)
            .map_err(|_| anyhow::anyhow!("kd-tree {name:?}: negative KDT_NDIM"))?;
        ensure!(
            (1..=MAX_NDIM).contains(&ndim),
            "kd-tree {name:?}: KDT_NDIM {ndim} is outside 1..={MAX_NDIM}"
        );
        let ndata = usize::try_from(header.require_i64("KDT_NDAT")?)
            .map_err(|_| anyhow::anyhow!("kd-tree {name:?}: negative KDT_NDAT"))?;
        ensure!(ndata >= 1, "kd-tree {name:?} has no data records");
        let nnodes = usize::try_from(header.require_i64("KDT_NNOD")?)
            .map_err(|_| anyhow::anyhow!("kd-tree {name:?}: negative KDT_NNOD"))?;
        ensure!(nnodes >= 1, "kd-tree {name:?} has no nodes");
        let int_kind = IntKind::from_name(header.require_str("KDT_INT")?)?;
        let data_kind = IntKind::from_name(header.require_str("KDT_DATA")?)?;
        let (nbottom, ninterior) = Self::node_counts(nnodes)?;

        // lr entries are u32 regardless of KDT_INT. `KDT_LINL` means uniformly sized
        // leaves, so the ranges are recomputed from `ndata` and `nbottom` and any `lr`
        // table the file still carries is never looked up - hence the flag is tested
        // first. Otherwise the entries are read lazily.
        let linear_lr = header.get_bool("KDT_LINL").unwrap_or(false);
        let lr = if linear_lr {
            None
        } else {
            let lr_ttype = format!("kdtree_lr_{name}");
            let lr = fits
                .blob_view(&lr_ttype)
                .with_context(|| format!("kd-tree {name:?} has no lr table"))?;
            let n = lr
                .u32_entries()
                .with_context(|| format!("kd-tree {name:?} lr table"))?;
            ensure!(
                n == nbottom,
                "kd-tree {name:?}: lr has {n} entries, expected {nbottom}"
            );
            Some(lr)
        };

        let split = if ninterior == 0 {
            None
        } else {
            let view = fits.raw_view(&format!("kdtree_split_{name}"))?;
            let want = ninterior.checked_mul(int_kind.size()).ok_or_else(|| {
                anyhow::anyhow!("kd-tree {name:?}: ninterior {ninterior} overflows")
            })?;
            ensure!(
                view.payload_bytes() == want,
                "kd-tree {name:?}: split is {} bytes, expected {want}",
                view.payload_bytes()
            );
            Some(view)
        };

        // range table: minval[ndim], maxval[ndim], scale (all f64). Small fixed size
        // (ndim <= MAX_NDIM), so it is copied eagerly.
        let range = fits.raw_view(&format!("kdtree_range_{name}"))?;
        let raw = range.raw_bytes();
        let want = (2 * ndim + 1) * 8;
        ensure!(
            raw.len() == want,
            "kd-tree {name:?}: range is {} bytes, expected {want}",
            raw.len()
        );
        let mut vals = Vec::with_capacity(2 * ndim + 1);
        for chunk in raw.as_chunks::<8>().0 {
            vals.push(endian.read_f64(chunk));
        }
        let scale = vals[2 * ndim];
        ensure!(
            f64::is_finite(scale) && scale > 0.0,
            "kd-tree {name:?}: non-finite or non-positive scale {scale}"
        );
        let minval = vals[..ndim].to_vec();
        let maxval = vals[ndim..2 * ndim].to_vec();
        // Non-finite or inverted bounds poison every dequantized coordinate (NaN
        // coords match nothing, so the solver would quietly find nothing), so they
        // must fail here rather than at search time.
        for d in 0..ndim {
            ensure!(
                f64::is_finite(minval[d]) && f64::is_finite(maxval[d]),
                "kd-tree {name:?}: non-finite range bound at dim {d}: [{}, {}]",
                minval[d],
                maxval[d]
            );
            ensure!(
                minval[d] <= maxval[d],
                "kd-tree {name:?}: minval[{d}] {} is above maxval[{d}] {}",
                minval[d],
                maxval[d]
            );
        }

        let data_table = fits.table_by_ttype(&format!("kdtree_data_{name}"))?;
        let row_len = data_table.row_len;
        ensure!(
            data_table.nrows == ndata,
            "kd-tree {name:?}: data table has {} rows, ndata is {ndata}",
            data_table.nrows
        );
        ensure!(
            row_len == ndim * data_kind.size(),
            "kd-tree {name:?}: data row is {row_len} bytes, expected {}",
            ndim * data_kind.size()
        );
        let Some(first) = data_table.columns.first() else {
            bail!("kd-tree {name:?}: data table has no columns");
        };
        ensure!(
            matches!(first.form, Form::Blob(_)),
            "kd-tree {name:?}: data column {:?} is not a raw blob",
            first.name
        );
        // One view over the whole row area: the integer of record `rec`, dimension
        // `d` is addressed as `rec * ndim + d` and dequantized in `coord`, so no
        // record is ever copied out of the mmap.
        let data = fits.raw_view(&format!("kdtree_data_{name}"))?;
        ensure!(
            row_len.checked_mul(ndata) == Some(data.payload_bytes()),
            "kd-tree {name:?}: data blob is {} bytes, expected {ndata} rows of {row_len}",
            data.payload_bytes()
        );

        let tree = Self {
            name: name.to_string(),
            ndim,
            ndata,
            nnodes,
            nbottom,
            ninterior,
            minval,
            maxval,
            scale,
            invscale: 1.0 / scale,
            int_kind,
            data_kind,
            endian,
            dimmask: dimmask_for(ndim)?,
            linear_lr,
            lr,
            split,
            data,
        };
        tree.check_structure()
            .with_context(|| format!("kd-tree {name:?}"))?;
        Ok(tree)
    }

    /// `nbottom = ceil(nnodes / 2)` and `ninterior = nnodes - nbottom`.
    ///
    /// These trees are full binary trees, so `nnodes` is odd: only then do the
    /// children of the last interior node (nodes `2 * ninterior - 1` and
    /// `2 * ninterior`) land on the last two leaves `nbottom - 2` and `nbottom - 1`
    /// (for `nnodes >= 3`); with an even count the second lands on `nbottom`, a leaf
    /// index that does not exist, and `leaf_range` asserts before it reads anything.
    /// The `u32` bound keeps node ids representable in the search stacks.
    pub fn node_counts(nnodes: usize) -> Result<(usize, usize)> {
        ensure!(
            nnodes % 2 == 1,
            "kd-tree with even nnodes={nnodes}: these trees are full binary trees"
        );
        ensure!(
            nnodes <= u32::MAX as usize,
            "kd-tree with nnodes={nnodes} exceeds the u32 node ids of the searches"
        );
        let nbottom = usize::div_ceil(nnodes, 2);
        Ok((nbottom, nnodes - nbottom))
    }

    /// Dequantized coordinate: `q * invscale + minval[d]`, decoded from the quantized
    /// blob on demand (it stays in the mmap; the struct docs give the reason there is
    /// no eager `f64` copy).
    #[inline]
    #[must_use]
    pub fn coord(&self, rec: usize, d: usize) -> f64 {
        let q = self.raw_at(rec, d);
        f64::from(q) * self.invscale + self.minval[d]
    }

    /// The stored quantized integer of record `rec`, dimension `d`: `coord` without
    /// the dequantization, for a caller working in quantized units (the split values
    /// of a filtered tree are one).
    #[inline]
    #[must_use]
    fn raw_at(&self, rec: usize, d: usize) -> u32 {
        self.data
            .uint_at(self.endian, self.data_kind.size(), rec * self.ndim + d)
    }

    /// Dequantized record `rec`.
    pub fn record(&self, rec: usize) -> Vec<f64> {
        (0..self.ndim).map(|d| self.coord(rec, d)).collect()
    }

    /// Dequantized first three coordinates of `rec` (the 3D star trees).
    ///
    /// The flat little-endian fast path reads the record as one byte span and
    /// dispatches on the element kind once, with the same dequantization as [`coord`]:
    /// bit-identical to three `coord` calls.
    #[must_use]
    pub fn coord3(&self, rec: usize) -> [f64; 3] {
        debug_assert!(self.ndim >= 3, "coord3 on a tree with ndim < 3");
        if self.endian == Endian::Little && self.data.is_flat() {
            let mut v = [0.0f64; 3];
            match self.data_kind.size() {
                2 => {
                    let raw = self.data.raw_bytes();
                    let row = &raw[rec * self.ndim * 2..rec * self.ndim * 2 + 6];
                    for (d, &[a, b]) in row.as_chunks::<2>().0.iter().take(3).enumerate() {
                        let q = u32::from(u16::from_le_bytes([a, b]));
                        v[d] = f64::from(q) * self.invscale + self.minval[d];
                    }
                }
                4 => {
                    let raw = self.data.raw_bytes();
                    let row = &raw[rec * self.ndim * 4..rec * self.ndim * 4 + 12];
                    for (d, c) in row.as_chunks::<4>().0.iter().take(3).enumerate() {
                        let q = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                        v[d] = f64::from(q) * self.invscale + self.minval[d];
                    }
                }
                // No other element kind: fall back to the general decode below.
                _ => return [self.coord(rec, 0), self.coord(rec, 1), self.coord(rec, 2)],
            }
            return v;
        }
        [self.coord(rec, 0), self.coord(rec, 1), self.coord(rec, 2)]
    }

    /// The inclusive right end stored in leaf `leaf`'s `lr` entry. Panics when
    /// `linear_lr` (no table); callers gate on `leaf_range`.
    #[must_use]
    pub fn lr_entry(&self, leaf: usize) -> u32 {
        self.lr
            .as_ref()
            .expect("lr table absent only when linear_lr")
            .uint_at(self.endian, 4, leaf)
    }

    /// Half-open `[left, right)` data range of leaf `j`; empty when
    /// `left >= right`.
    pub fn leaf_range(&self, leaf: usize) -> (usize, usize) {
        assert!(leaf < self.nbottom, "leaf {leaf} out of {}", self.nbottom);
        if self.linear_lr {
            // Uniform leaves: leaf j owns [j * ndata / nbottom,
            // (j + 1) * ndata / nbottom); each end is the next leaf's start and the
            // final end is `ndata`, so the ranges tile [0, ndata).
            let l = Self::linear_lr_end(leaf, self.ndata, self.nbottom);
            let r = Self::linear_lr_end(leaf + 1, self.ndata, self.nbottom);
            return (l, r);
        }
        let left = if leaf == 0 {
            0
        } else {
            usize::try_from(self.lr_entry(leaf - 1)).expect("u32 fits in usize") + 1
        };
        // The table stores the *inclusive* right end; +1 never overflows a usize
        // (u32 + 1).
        (
            left,
            usize::try_from(self.lr_entry(leaf)).expect("u32 fits in usize") + 1,
        )
    }

    /// The uniform leaf boundary `leaf * ndata / nbottom` (integer division), product
    /// taken in `u128`: on a 32-bit target `leaf * ndata` passes `u32::MAX` at about a
    /// million records (500_000 * 1_000_000 = 5e11), and a wrapped product would hand
    /// out wrong, possibly overlapping ranges. `nbottom >= 1` because `nnodes >= 1`,
    /// but it is floored at 1 here so the helper cannot divide by zero on its own.
    fn linear_lr_end(leaf: usize, ndata: usize, nbottom: usize) -> usize {
        let leaf = u64::try_from(leaf).unwrap_or(u64::MAX);
        let ndata = u64::try_from(ndata).unwrap_or(u64::MAX);
        let nbottom = u64::try_from(nbottom).unwrap_or(u64::MAX).max(1);
        usize::try_from(u128::from(leaf) * u128::from(ndata) / u128::from(nbottom))
            .unwrap_or(usize::MAX)
    }

    /// Unpacks `split[node]` into `(dim, split value)`, the value still in
    /// quantized tree units.
    pub fn split_at(&self, node: usize) -> (usize, u32) {
        let raw = self
            .split
            .as_ref()
            .expect("split table absent only when ninterior == 0")
            .uint_at(self.endian, self.int_kind.size(), node);
        let dim = usize::try_from(raw & self.dimmask).unwrap_or(usize::MAX);
        (dim, raw & !self.dimmask)
    }

    /// Split value dequantized into external units the way `coord` does it:
    /// `s * invscale + minval[dim]`.
    fn split_external(&self, node: usize) -> (usize, f64) {
        let (dim, s) = self.split_at(node);
        let value = f64::from(s) * self.invscale + self.minval[dim];
        (dim, value)
    }

    /// Structural self-check, run on every tree at load: blob sizes agree with the
    /// header counts, the node counts with each other, every packed split dimension
    /// is inside `ndim`, `lr` has one entry per leaf, and the leaf ranges tile
    /// `[0, ndata)` without gaps or overlaps (for a table-driven tree that means `lr`
    /// is non-decreasing with last entry `ndata - 1`). A corrupt index fails here
    /// (exit 2) instead of quietly returning wrong hits later.
    pub fn check_structure(&self) -> Result<()> {
        ensure!(
            self.ndata.checked_mul(self.ndim * self.data_kind.size())
                == Some(self.data.payload_bytes())
                && self.data.nrows() == self.ndata,
            "kd-tree {:?}: data blob size mismatch",
            self.name
        );
        // The searches address a node as a leaf exactly when `node >= ninterior`, so
        // the three counts must agree. A loaded tree gets them from
        // `node_counts(nnodes)`; one built at run time (`filtered`) sets all three by
        // hand, which is what this check is for.
        ensure!(
            self.nnodes == self.nbottom + self.ninterior,
            "kd-tree {:?}: {} nodes for {} leaves + {} interior",
            self.name,
            self.nnodes,
            self.nbottom,
            self.ninterior
        );
        let split_len = self.split.as_ref().map_or(0, |v| v.payload_bytes());
        ensure!(
            split_len == usize::saturating_mul(self.ninterior, self.int_kind.size()),
            "kd-tree {:?}: {} split bytes for {} interior nodes",
            self.name,
            split_len,
            self.ninterior
        );
        for node in 0..self.ninterior {
            let (dim, _) = self.split_at(node);
            ensure!(
                dim < self.ndim,
                "kd-tree {:?}: split dim {dim} of node {node} is out of ndim {}",
                self.name,
                self.ndim
            );
        }
        if self.linear_lr {
            ensure!(
                self.lr.is_none(),
                "kd-tree {:?}: linear lr with a table",
                self.name
            );
        } else {
            let n = self.lr.as_ref().map_or(0, |v| v.payload_bytes() / 4);
            ensure!(
                n == self.nbottom,
                "kd-tree {:?}: {} lr entries for {} leaves",
                self.name,
                n,
                self.nbottom
            );
        }

        let mut cursor = 0usize;
        for leaf in 0..self.nbottom {
            let (l, r) = self.leaf_range(leaf);
            ensure!(
                l == cursor,
                "kd-tree {:?}: leaf {leaf} starts at {l}, expected {cursor} (gap or overlap)",
                self.name
            );
            ensure!(
                l <= r && r <= self.ndata,
                "kd-tree {:?}: leaf {leaf} range [{l}, {r}) is out of ndata {}",
                self.name,
                self.ndata
            );
            cursor = r;
        }
        ensure!(
            cursor == self.ndata,
            "kd-tree {:?}: leaves cover [0, {cursor}), ndata is {}",
            self.name,
            self.ndata
        );
        Ok(())
    }

    /// A compact tree over `recs` alone - the records of `self`, in any order and
    /// without duplicates - for a caller that must only ever see that subset. Returns
    /// the tree with its record map: new record `i` is `rec_map[i]` of `self`.
    ///
    /// The quantized rows are copied verbatim (decoded from the source's byte order,
    /// written little-endian, so every dequantized coordinate is unchanged) and
    /// re-medianed into a fresh perfect tree by [`PartBuild`]. `minval`/`maxval`/
    /// `scale` are inherited, keeping the external-unit geometry - and therefore every
    /// query answer - the source tree's.
    ///
    /// The result is anonymous memory (`ndim` integers per record plus two `u32` per
    /// node), so this is for a few thousand records. `None` means "no tree": `recs` is
    /// empty, larger than `params::HINT_TREE_MAX_RECORDS`, out of range, wider than
    /// `params::HINT_TREE_MAX_ROW_BYTES` per record (that cap is the byte budget the
    /// record cap was counted at, so a wider row is refused, not given a bigger
    /// budget), or a node's records admit no storable plane above them (`u32` data
    /// inside the top `dimmask` steps of its range). A caller that gets `None` must
    /// fall back to searching `self`, which is always correct: this says nothing about
    /// whether any record matches, and an empty `recs` is not a verdict of "no hits" -
    /// only its caller knows that subset is empty.
    ///
    /// For every query the new tree returns exactly the records of `recs` inside the
    /// ball - interchangeable with the source tree plus a filter. Every split value
    /// (the median planes and the one a node with inseparable records gets) is chosen
    /// so the left half's quantized coordinate is `<= plane` and the right half's above
    /// it: the invariant `descend` prunes on, in the quantized units the entry stores
    /// (the masked low bits are why the rule is against the plane, not the median
    /// record's own value).
    pub fn filtered(&self, recs: &[u32]) -> Option<(KdTree, Vec<u32>)> {
        let m = recs.len();
        if m == 0 || m > crate::params::HINT_TREE_MAX_RECORDS {
            return None;
        }
        // The record cap is a byte budget counted at one row width: the widest codes
        // row the index format produces. A caller with wider rows would get a tree
        // bigger by that factor, so it gets none.
        let esize = self.data_kind.size();
        if self.ndim * esize > crate::params::HINT_TREE_MAX_ROW_BYTES {
            return None;
        }
        if recs
            .iter()
            .any(|&r| usize::try_from(r).unwrap_or(usize::MAX) >= self.ndata)
        {
            return None;
        }
        debug_assert!(
            {
                let mut sorted = recs.to_vec();
                sorted.sort_unstable();
                sorted.dedup();
                sorted.len() == recs.len()
            },
            "filtered: a repeated record would be searched twice"
        );
        // Leaves: the largest power of two that still leaves `HINT_TREE_LEAF` records
        // each, so no leaf is forced empty by the shape alone.
        let mut nbottom = 1usize;
        while nbottom * 2 * crate::params::HINT_TREE_LEAF <= m {
            nbottom *= 2;
        }
        let nnodes = 2 * nbottom - 1;
        let ninterior = nbottom - 1;
        let mut build = PartBuild::new(self, recs, nbottom);
        if !build.part(0, m, 0, 0, nbottom) {
            return None;
        }
        let PartBuild {
            order,
            split,
            counts,
            ..
        } = build;

        // `lr` holds the inclusive right end of each leaf's range, so the running
        // total of the leaf counts is one past it. An empty leaf repeats the previous
        // end; a total of 0 would need `-1`, which no `u32` holds - the case the build
        // keeps away from leaf 0 by never letting a node's left half come out empty.
        let mut acc = 0u32;
        let mut lr = Vec::with_capacity(nbottom * 4);
        for &count in &counts {
            acc = acc.checked_add(count)?;
            lr.extend_from_slice(&acc.checked_sub(1)?.to_le_bytes());
        }

        let split_bytes: Vec<u8> = split.iter().flat_map(|v| u32::to_le_bytes(*v)).collect();
        let mut data = vec![0u8; m * self.ndim * esize];
        for (row, &rec) in order.iter().enumerate() {
            for d in 0..self.ndim {
                let v = self
                    .data
                    .uint_at(self.endian, esize, rec as usize * self.ndim + d);
                let off = (row * self.ndim + d) * esize;
                match esize {
                    2 => data[off..off + 2].copy_from_slice(&u16::try_from(v).ok()?.to_le_bytes()),
                    4 => data[off..off + 4].copy_from_slice(&v.to_le_bytes()),
                    // `IntKind::size` is 2 or 4.
                    _ => return None,
                }
            }
        }

        let tree = KdTree {
            name: format!("{}:filtered", self.name),
            ndim: self.ndim,
            ndata: m,
            nnodes,
            nbottom,
            ninterior,
            minval: self.minval.clone(),
            maxval: self.maxval.clone(),
            scale: self.scale,
            invscale: self.invscale,
            int_kind: IntKind::U32,
            data_kind: self.data_kind,
            endian: Endian::Little,
            dimmask: self.dimmask,
            linear_lr: false,
            lr: Some(BlobView::owned(lr, 4)),
            split: (ninterior > 0).then(|| BlobView::owned(split_bytes, 4)),
            data: BlobView::owned(data, self.ndim * esize),
        };
        tree.check_structure().ok()?;
        Some((tree, order))
    }

    /// Squared L2 distance from `query` to record `rec` in external units, bailing
    /// out as soon as the running total exceeds `cutoff`: callers only need to know
    /// the record is outside, so the remaining dimensions are not worth reading.
    ///
    /// A bailed-out record reports `f64::INFINITY`, not a value just above `cutoff`:
    /// from `cutoff >= 4.0` up the ulp is at least 4 * `f64::EPSILON`, so `cutoff +
    /// eps` rounds back to `cutoff` and such a record would pass the caller's
    /// `d2 <= cutoff` test as a hit. A bail is never mistaken for a real distance in
    /// `nearest`: while its best is still the `INFINITY` sentinel the running total
    /// cannot exceed the cutoff, so the exact distance comes back.
    fn dist2_cutoff(&self, rec: usize, query: &[f64], cutoff: f64) -> f64 {
        // The per-record accessor: searches go through `Scan::dist2`, which hoists
        // this flat little-endian fast path once per traversal and delegates other
        // layouts back here. The legs must stay arithmetically identical, including
        // the INFINITY bail (`scan_paths_match_the_general_accessors` pins this body,
        // the general path below it and `dist2_flat` against each other).
        if self.endian == Endian::Little && self.data.is_flat() {
            let raw = self.data.raw_bytes();
            let mut d2 = 0.0;
            let mut decodable = true;
            match self.data_kind.size() {
                2 => {
                    let row = &raw[rec * self.ndim * 2..(rec + 1) * self.ndim * 2];
                    for (d, &[a, b]) in row.as_chunks::<2>().0.iter().enumerate() {
                        let q = u32::from(u16::from_le_bytes([a, b]));
                        let delta = query[d] - (f64::from(q) * self.invscale + self.minval[d]);
                        d2 += delta * delta;
                        if d2 > cutoff {
                            return f64::INFINITY;
                        }
                    }
                }
                4 => {
                    let row = &raw[rec * self.ndim * 4..(rec + 1) * self.ndim * 4];
                    for (d, c) in row.as_chunks::<4>().0.iter().enumerate() {
                        let q = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                        let delta = query[d] - (f64::from(q) * self.invscale + self.minval[d]);
                        d2 += delta * delta;
                        if d2 > cutoff {
                            return f64::INFINITY;
                        }
                    }
                }
                _ => decodable = false,
            }
            if decodable {
                return d2;
            }
        }
        let mut d2 = 0.0;
        for (d, &q) in query.iter().enumerate() {
            let delta = q - self.coord(rec, d);
            d2 += delta * delta;
            if d2 > cutoff {
                return f64::INFINITY;
            }
        }
        d2
    }

    /// All records within `tol` (L2, external units) of `query`, handed to `f` as
    /// `(record, squared distance)` in traversal order.
    ///
    /// This, [`KdTree::ball_search_into`] and [`KdTree::try_ball_search_visit`] all
    /// run the one traversal [`KdTree::ball_search_try`], so they cannot drift.
    /// Visiting lets a caller act on a hit without materializing the ones it will
    /// drop (what [`crate::verify`] does: for a garbage fit the ball can be the whole
    /// star table, nearly all of it failing the in-frame test).
    pub fn ball_search_visit(&self, query: &[f64], tol: f64, mut f: impl FnMut(usize, f64)) {
        self.ball_search_try(query, tol, &mut |index, dist2| {
            f(index, dist2);
            ControlFlow::Continue(())
        });
    }

    /// [`KdTree::ball_search_visit`] whose callback can stop the search: `f` returns
    /// [`ControlFlow::Break`] to abandon the traversal, which then reports `false`
    /// (records that would have followed were never visited); a search that ran to
    /// the end reports `true`.
    ///
    /// This bounds the cost of collecting a ball no longer wanted: neither the tree
    /// nor the ball has a size bound a caller could trust in advance, and
    /// [`KdTree::ball_search_visit`]'s callback cannot abort.
    pub fn try_ball_search_visit(
        &self,
        query: &[f64],
        tol: f64,
        mut f: impl FnMut(usize, f64) -> ControlFlow<()>,
    ) -> bool {
        self.ball_search_try(query, tol, &mut f)
    }

    /// The shared ball traversal: `f` sees every record within `tol` in traversal
    /// order and may break the search early (see [`KdTree::try_ball_search_visit`]).
    fn ball_search_try(
        &self,
        query: &[f64],
        tol: f64,
        f: &mut impl FnMut(usize, f64) -> ControlFlow<()>,
    ) -> bool {
        assert_eq!(query.len(), self.ndim, "ball search dimension mismatch");
        if self.ndata == 0 || tol < 0.0 {
            return true;
        }
        // `Scan` resolves the layout's decode choices once per traversal.
        let scan = Scan::new(self, query);
        let maxd2 = tol * tol;
        // Pending nodes are pairwise incomparable in the tree - each is the sibling
        // of a node on the path being descended - so at most one per level is in
        // flight, which `SEARCH_STACK_SLOTS` bounds. The hot searches never touch the
        // allocator.
        let mut stack = [0u32; SEARCH_STACK_SLOTS];
        let mut sp = 0usize;
        stack[sp] = 0;
        sp += 1;
        while sp > 0 {
            sp -= 1;
            let node = stack[sp] as usize;
            if node >= self.ninterior {
                let (l, r) = scan.leaf_range(node - self.ninterior);
                for rec in l..r {
                    let d2 = scan.dist2(rec, maxd2);
                    if d2 <= maxd2 && f(rec, d2).is_break() {
                        return false;
                    }
                }
                continue;
            }
            scan.descend(node, tol, &mut stack, &mut sp);
        }
        true
    }

    /// All records within `tol` (L2, external units) of `query`, appended to `out` in
    /// traversal order; use [`KdTree::ball_search`] when they must be sorted by
    /// distance, [`KdTree::ball_search_visit`] to consume them without a buffer.
    pub fn ball_search_into(&self, query: &[f64], tol: f64, out: &mut Vec<Hit>) {
        self.ball_search_visit(query, tol, |index, dist2| {
            out.push(Hit { index, dist2 });
        });
    }

    /// All records within `tol` of `query`, sorted by ascending squared distance. The
    /// per-field-code query uses [`KdTree::ball_search_into`] and sorts the buffer
    /// itself, and `verify::collect_refs` filters during [`KdTree::ball_search_visit`]
    /// and sorts only the survivors; this wrapper is what [`KdTree::self_check`] and
    /// the tests call.
    pub fn ball_search(&self, query: &[f64], tol: f64) -> Vec<Hit> {
        let mut hits = Vec::new();
        self.ball_search_into(query, tol, &mut hits);
        hits.sort_by(|a, b| f64::total_cmp(&a.dist2, &b.dist2));
        hits
    }

    /// Nearest record to `query` (branch and bound). Every stack entry carries a lower
    /// bound on the distance from `query` to anything below it (`gap^2` for a far
    /// child, `0.0` for a near one), and an entry whose bound has fallen behind the
    /// best distance so far is dropped on pop. The near child is visited first, the
    /// opposite of `descend`'s push order in the range search.
    ///
    /// With no cutoff, a query whose nearest record is far away still pays for the
    /// full descent; up to some radius use [`KdTree::nearest_within`].
    pub fn nearest(&self, query: &[f64]) -> Option<Hit> {
        // The infinite-cutoff case: `dist2_cutoff` never bails against an
        // `INFINITY` best, so every distance reported here is exact.
        self.nearest_inner(query, f64::INFINITY)
    }

    /// The nearest record within `cutoff2` (squared L2, external units), or `None`
    /// when no record lies inside it. The boundary is inclusive, like the
    /// `d2 <= maxd2` test of [`KdTree::ball_search_visit`].
    ///
    /// [`KdTree::nearest`] cannot stand in for it: here the cutoff starts as the best
    /// distance, so a record beyond it is rejected by `dist2_cutoff` without ever
    /// becoming the best, and the far-child test prunes like a ball search of the same
    /// radius - a query with nothing near it costs the descent path plus the leaves
    /// touching the ball.
    ///
    /// A caller asking only "is anything inside?" for a whole frame of queries should
    /// not pay one descent each: [`crate::verify`] collects one
    /// [`KdTree::try_ball_search_visit`] over the frame and tests the queries against
    /// what it holds. This is the probe's per-probe fallback and the reference the
    /// batched verdict is pinned against
    /// (`kdtree::tests::batched_probe_ball_matches_nearest_within`).
    #[must_use]
    pub fn nearest_within(&self, query: &[f64], cutoff2: f64) -> Option<Hit> {
        // A negative or non-finite cutoff asks for nothing (the infinite-cutoff case
        // is `nearest`, which bypasses this).
        if cutoff2 < 0.0 || !cutoff2.is_finite() {
            return None;
        }
        self.nearest_inner(query, cutoff2)
    }

    /// The branch and bound shared by [`KdTree::nearest`] (infinite cutoff) and
    /// [`KdTree::nearest_within`]; `cutoff2` must be non-negative and finite. The
    /// stack is the same fixed [`SEARCH_STACK_SLOTS`] array as the ball search, bounded
    /// the same way, which keeps both searches allocation-free.
    fn nearest_inner(&self, query: &[f64], cutoff2: f64) -> Option<Hit> {
        assert_eq!(query.len(), self.ndim, "nearest dimension mismatch");
        if self.ndata == 0 {
            return None;
        }
        // Same once-per-traversal decode context as `ball_search_try`.
        let scan = Scan::new(self, query);
        let mut best = Hit {
            index: usize::MAX,
            dist2: cutoff2,
        };
        // (node, lower bound on the distance from `query` to anything below it)
        let mut stack = [(0u32, 0.0f64); SEARCH_STACK_SLOTS];
        let mut sp = 0usize;
        stack[sp] = (0, 0.0);
        sp += 1;
        while sp > 0 {
            sp -= 1;
            let (node, lb) = stack[sp];
            if lb > best.dist2 {
                continue;
            }
            let node = node as usize;
            if node >= self.ninterior {
                let (l, r) = scan.leaf_range(node - self.ninterior);
                for rec in l..r {
                    let d2 = scan.dist2(rec, best.dist2);
                    // Lowest index wins an exact tie; `best.dist2` starting at the
                    // cutoff also makes the cutoff itself a hit.
                    if d2 < best.dist2 || (d2 == best.dist2 && rec < best.index) {
                        best = Hit {
                            index: rec,
                            dist2: d2,
                        };
                    }
                }
                continue;
            }
            let (dim, rsplit) = scan.split_external(node);
            let gap = query[dim] - rsplit;
            let (near, far) = if gap < 0.0 {
                (2 * node + 1, 2 * node + 2)
            } else {
                (2 * node + 2, 2 * node + 1)
            };
            let far_d2 = gap * gap;
            debug_assert!(sp + 2 <= SEARCH_STACK_SLOTS, "dfs stack overflow");
            if far_d2 <= best.dist2 {
                stack[sp] = (far as u32, far_d2);
                sp += 1;
            }
            stack[sp] = (near as u32, 0.0);
            sp += 1;
        }
        if best.index == usize::MAX {
            None
        } else {
            Some(best)
        }
    }

    /// Records used by `self_check`: first, last, middle and a stride sample.
    fn sample_records(&self) -> Vec<usize> {
        if self.ndata == 0 {
            return Vec::new();
        }
        let mut v = vec![0, self.ndata - 1, self.ndata / 2];
        let stride = usize::max(1, self.ndata / 8);
        let mut i = 0;
        while i < self.ndata {
            v.push(i);
            i += stride;
        }
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Self-test of the searches: a record queried with a zero radius must be
    /// its own nearest neighbour and must appear in the zero-radius ball search.
    pub fn self_check(&self) -> Result<()> {
        for rec in self.sample_records() {
            let q = self.record(rec);
            let nearest = self
                .nearest(&q)
                .ok_or_else(|| anyhow::anyhow!("kd-tree {:?}: nearest found nothing", self.name))?;
            ensure!(
                nearest.dist2 == 0.0,
                "kd-tree {:?}: nearest of record {rec} is at dist2 {}, expected 0",
                self.name,
                nearest.dist2
            );
            let hits = self.ball_search(&q, 0.0);
            ensure!(
                hits.iter().any(|h| h.index == rec),
                "kd-tree {:?}: zero-radius ball search missed record {rec}",
                self.name
            );
            ensure!(
                hits.is_sorted_by(|a, b| a.dist2 <= b.dist2),
                "kd-tree {:?}: ball search results are not distance-sorted",
                self.name
            );
        }
        Ok(())
    }

    /// Human-readable description of the tree (used by `--dump-index`).
    pub fn describe(&self) -> String {
        let lr_check = match self.check_structure() {
            Ok(()) => "lr monotonic, leaves tile [0, ndata)".to_string(),
            Err(e) => format!("STRUCTURE ERROR: {e}"),
        };
        let lr_last = if self.lr.is_none() {
            "linear".to_string()
        } else {
            usize::try_from(self.lr_entry(self.nbottom - 1))
                .map(|v| v.to_string())
                .unwrap_or_else(|_| "?".to_string())
        };
        let search_check = match self.self_check() {
            Ok(()) => "zero-radius nearest/ball queries resolve to the record itself".to_string(),
            Err(e) => format!("SEARCH ERROR: {e}"),
        };
        format!(
            "  tree {name}: int={int} data={data} ndim={ndim} ndata={ndata} nnodes={nnodes} \
             nbottom={nbottom} ninterior={ninterior}\n\
             \x20   minval={minval:?}\n\
             \x20   maxval={maxval:?}\n\
             \x20   scale={scale} invscale={invscale}\n\
             \x20   lr[{nbottom}](u32) last={lr_last}; {check}\n\
             \x20   {search_check}\n",
            name = self.name,
            int = kind_name(self.int_kind),
            data = kind_name(self.data_kind),
            ndim = self.ndim,
            ndata = self.ndata,
            nnodes = self.nnodes,
            nbottom = self.nbottom,
            ninterior = self.ninterior,
            minval = self.minval,
            maxval = self.maxval,
            scale = self.scale,
            invscale = self.invscale,
            lr_last = lr_last,
            check = lr_check,
            search_check = search_check,
        )
    }
}

fn kind_name(k: IntKind) -> &'static str {
    match k {
        IntKind::U16 => "u16",
        IntKind::U32 => "u32",
    }
}

/// The record array and the node tables a [`KdTree::filtered`] build is writing.
///
/// A depth-first walk of a perfect tree over `order`: the records, permuted in place
/// into the order the leaves read them. A node's records always form one contiguous
/// span of `order`, so a leaf's data range follows from the leaf counts and the
/// ranges tile the records the way `lr` stores them.
struct PartBuild<'a> {
    src: &'a KdTree,
    /// The records, permuted into tree order: the record map of the result.
    order: Vec<u32>,
    /// `split[node]` of the interior nodes, packed `value | dim`.
    split: Vec<u32>,
    /// Records per leaf, in leaf order.
    counts: Vec<u32>,
    /// The granularity a split value must have: `dimmask + 1`, because the low
    /// `dimmask` bits of a `split` entry hold the dimension.
    gran: u32,
}

impl<'a> PartBuild<'a> {
    fn new(src: &'a KdTree, recs: &[u32], nbottom: usize) -> Self {
        PartBuild {
            src,
            order: recs.to_vec(),
            split: vec![0u32; nbottom - 1],
            counts: vec![0u32; nbottom],
            gran: src.dimmask + 1,
        }
    }

    /// The quantized coordinate `d` of record `rec`, as the source tree stores it.
    #[inline]
    fn key(&self, rec: u32, d: usize) -> u32 {
        self.src.raw_at(rec as usize, d)
    }

    /// Writes the subtree rooted at `node` over `order[lo .. hi]`, whose leaves are
    /// `counts[leaf .. leaf + nleaf]`. Reports `false` when the records cannot be put
    /// in a tree whose splits the searches can prune on soundly; then no partial
    /// result is usable.
    fn part(&mut self, lo: usize, hi: usize, node: usize, leaf: usize, nleaf: usize) -> bool {
        let n = hi - lo;
        if n == 0 {
            // Nothing to place: this subtree's leaves keep their count of 0.
            return true;
        }
        if node >= self.split.len() {
            self.counts[leaf] = u32::try_from(n).unwrap_or(u32::MAX);
            return true;
        }
        let left = 2 * node + 1;
        let right = 2 * node + 2;
        let nleft = nleaf / 2;
        let (dimmask, ndim) = (self.src.dimmask, self.src.ndim);
        // Coordinate range of this node's records per dim: the widest dim splits
        // best, a dim with no storable value in its range cannot split at all.
        let mut qmin = [u32::MAX; MAX_NDIM];
        let mut qmax = [0u32; MAX_NDIM];
        for &rec in &self.order[lo..hi] {
            for d in 0..ndim {
                let v = self.key(rec, d);
                qmin[d] = u32::min(qmin[d], v);
                qmax[d] = u32::max(qmax[d], v);
            }
        }
        // The source, held on its own so the median's comparator can read
        // coordinates while `order` is being permuted.
        let src = self.src;
        let gran = self.gran;
        let mut tried = 0u64;
        for _ in 0..crate::params::HINT_TREE_MAX_PART {
            // The widest spread not tried yet; no positive spread left means the
            // records here share a coordinate in every dim.
            let mut dim = None;
            let mut best = 0u32;
            for d in 0..ndim {
                if tried & (1u64 << d) != 0 {
                    continue;
                }
                let spread = qmax[d].wrapping_sub(qmin[d]);
                if spread > best {
                    best = spread;
                    dim = Some(d);
                }
            }
            let Some(d) = dim else { break };
            tried |= 1u64 << d;
            let mid = lo + n / 2;
            self.order[lo..hi].select_nth_unstable_by(mid - lo, |x, y| {
                u32::cmp(&src.raw_at(*x as usize, d), &src.raw_at(*y as usize, d))
            });
            // The split value: the median's, masked the way the format masks it,
            // bumped by `gran` (so the dimension bits stay clear) to the first
            // storable value not below `qmin[d]`. It must land inside
            // `[qmin[d], qmax[d])` so each half gets a record.
            let mut s = self.key(self.order[mid], d) & !dimmask;
            if s < qmin[d] {
                let Some(v) = s.checked_add(gran) else {
                    continue;
                };
                s = v;
            }
            if s < qmin[d] || s >= qmax[d] {
                continue;
            }
            // `<= s` goes left, `> s` right: exactly the rule `descend` prunes on,
            // in the quantized units the entry stores (dequantizing is monotone).
            let mut p = lo;
            for i in lo..hi {
                if self.key(self.order[i], d) <= s {
                    self.order.swap(p, i);
                    p += 1;
                }
            }
            self.split[node] = s | d as u32;
            if !self.part(lo, p, left, leaf, nleft) {
                return false;
            }
            return self.part(p, hi, right, leaf + nleft, nleaf - nleft);
        }
        // No dim offers a plane inside `[qmin[d], qmax[d])`: these records agree within
        // one storage step in every coordinate. They all go left, and the plane is the
        // smallest storable value at or above their dim-0 maximum.
        //
        // What `descend` needs of any plane is that the left half holds no record above
        // it: a query loses a record only by sitting more than its tolerance above the
        // plane while that record sits at or under it. Rounding the half's own maximum
        // up provides that for every element kind; the largest storable value alone
        // would not, because `KDT_DATA = u32` records can sit above it, and then a
        // query just past the plane would prune the half that holds every record.
        //
        // Rounding up leaves one case with no plane: keys inside the top `dimmask`
        // steps of the `u32` range have nothing storable above them, so the honest
        // answer is no tree and the caller keeps searching the source tree.
        let ceil = (u64::from(qmax[0]) + u64::from(dimmask)) & !u64::from(dimmask);
        let Ok(s) = u32::try_from(ceil) else {
            return false;
        };
        // Dim 0, and `ceil` is a multiple of `gran`, so the dim bits are clear.
        self.split[node] = s;
        self.part(lo, hi, left, leaf, nleft)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::distsq;
    use crate::fits::Fits;
    use crate::testutil;
    use crate::verify::probe_pin::{probe_ball_coords, probe_ball_radius, within_chord2};

    /// These tests are `ANY_INDEX`: any old-format installed index works, so the
    /// smallest installed file is used, which keeps the brute-force comparisons
    /// fast. They self-skip (via `testutil::smallest_index`) with no index
    /// installed.
    fn load_tree(which: &str) -> Option<KdTree> {
        let meta = testutil::smallest_index(0)?;
        let fits = Fits::open(&meta.path).unwrap();
        Some(KdTree::load(&fits, which).unwrap())
    }

    fn stars_tree() -> Option<KdTree> {
        load_tree("stars")
    }

    fn codes_tree() -> Option<KdTree> {
        load_tree("codes")
    }

    /// Deterministic pseudo-random generator: the same query points every run, and
    /// no rng dependency.
    struct Lcg(u64);

    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 11
        }

        /// Uniform in `[lo, hi)`.
        fn next_range(&mut self, lo: f64, hi: f64) -> f64 {
            let u = self.next_u64() as f64 / (1u64 << 53) as f64;
            lo + (hi - lo) * u
        }
    }

    fn brute_force_nearest(tree: &KdTree, q: &[f64]) -> Hit {
        let mut best = Hit {
            index: usize::MAX,
            dist2: f64::INFINITY,
        };
        for rec in 0..tree.ndata {
            let mut d2 = 0.0;
            for (d, &qv) in q.iter().enumerate() {
                let delta = qv - tree.coord(rec, d);
                d2 += delta * delta;
            }
            if d2 < best.dist2 {
                best = Hit {
                    index: rec,
                    dist2: d2,
                };
            }
        }
        best
    }

    fn brute_force_ball(tree: &KdTree, q: &[f64], tol: f64) -> Vec<usize> {
        let mut hits = Vec::new();
        for rec in 0..tree.ndata {
            let mut d2 = 0.0;
            for (d, &qv) in q.iter().enumerate() {
                let delta = qv - tree.coord(rec, d);
                d2 += delta * delta;
            }
            if d2 <= tol * tol {
                hits.push(rec);
            }
        }
        hits.sort_unstable();
        hits
    }

    /// The tree traversal must find exactly what a linear scan finds.
    #[test]
    fn searches_match_brute_force() {
        let (Some(stars), Some(codes)) = (stars_tree(), codes_tree()) else {
            return;
        };
        for tree in [&stars, &codes] {
            let mut rng = Lcg(0x5eed);
            for _ in 0..12 {
                let q: Vec<f64> = (0..tree.ndim)
                    .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
                    .collect();

                let fast = tree.nearest(&q).unwrap();
                let slow = brute_force_nearest(tree, &q);
                assert_eq!(fast.index, slow.index, "{} nearest", tree.name);
                assert!(
                    (fast.dist2 - slow.dist2).abs() < 1e-12,
                    "{} dist2",
                    tree.name
                );

                // A tolerance spanning a good part of the space exercises the
                // far-child pruning in both directions.
                for tol in [0.01, 0.05, 0.2] {
                    let mut fast: Vec<usize> = tree
                        .ball_search(&q, tol)
                        .into_iter()
                        .map(|h| h.index)
                        .collect();
                    let slow = brute_force_ball(tree, &q, tol);
                    fast.sort_unstable();
                    assert_eq!(fast, slow, "{} ball search at tol {tol}", tree.name);
                }
            }
        }
    }

    /// Brute-force reference for [`KdTree::nearest_within`]: the nearest record at
    /// squared distance `<= cutoff2` (an exact tie goes to the lowest index), `None`
    /// when the cutoff holds nothing. It mirrors the searched semantics rather than
    /// deriving them, so a drift in the tree walk shows up here.
    fn brute_force_nearest_within(tree: &KdTree, q: &[f64], cutoff2: f64) -> Option<Hit> {
        if cutoff2 < 0.0 || !cutoff2.is_finite() {
            return None;
        }
        let mut best = Hit {
            index: usize::MAX,
            dist2: cutoff2,
        };
        for rec in 0..tree.ndata {
            let mut d2 = 0.0;
            for (d, &qv) in q.iter().enumerate() {
                let delta = qv - tree.coord(rec, d);
                d2 += delta * delta;
            }
            if d2 < best.dist2 || (d2 == best.dist2 && rec < best.index) {
                best = Hit {
                    index: rec,
                    dist2: d2,
                };
            }
        }
        (best.index != usize::MAX).then_some(best)
    }

    /// `nearest_within` must agree with a linear scan for every cutoff from "no
    /// record" to "the whole space", on the installed trees and on the crafted one,
    /// in both element kinds.
    #[test]
    fn nearest_within_matches_brute_force() {
        let (Some(stars), Some(codes)) = (stars_tree(), codes_tree()) else {
            return;
        };
        let crafted = load_crafted("valid", &Spec::valid());
        for tree in [&stars, &codes, &crafted] {
            let mut rng = Lcg(0x5eed);
            for _ in 0..12 {
                let q: Vec<f64> = (0..tree.ndim)
                    .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
                    .collect();
                let free = brute_force_nearest(tree, &q);
                // Cutoffs around the unconstrained nearest: strictly inside it (must
                // find nothing outside), exactly on it (inclusive boundary), one ulp
                // below (must then find nothing).
                let on = free.dist2;
                let below = f64::from_bits(f64::to_bits(on).saturating_sub(1));
                // A cutoff that covers the tree's whole bounding box.
                let span: f64 = (0..tree.ndim)
                    .map(|d| {
                        let s = tree.maxval[d] - tree.minval[d];
                        s * s
                    })
                    .sum();
                for cutoff2 in [0.0, on / 2.0, below, on, on * 2.0, span] {
                    let fast = tree.nearest_within(&q, cutoff2);
                    let slow = brute_force_nearest_within(tree, &q, cutoff2);
                    assert_eq!(
                        fast.map(|h| h.index),
                        slow.map(|h| h.index),
                        "{} nearest_within at cutoff2 {cutoff2}",
                        tree.name
                    );
                    if let (Some(f), Some(s)) = (fast, slow) {
                        assert!(
                            f64::abs(f.dist2 - s.dist2) < 1e-12,
                            "{} dist2 at cutoff2 {cutoff2}",
                            tree.name
                        );
                    }
                }
            }
        }
    }

    /// A record sitting exactly on the cutoff counts as a hit; a negative or
    /// non-finite cutoff asks for nothing.
    #[test]
    fn nearest_within_boundary_and_degenerate_cutoffs() {
        let crafted = load_crafted("valid", &Spec::valid());
        // From (0.5, 0) the nearest record is 0 = (0, 0) at d2 0.25 exactly.
        let q = [0.5, 0.0];
        let on_boundary = crafted
            .nearest_within(&q, 0.25)
            .expect("(0,0) sits exactly on the cutoff");
        assert_eq!(on_boundary.index, 0);
        assert_eq!(on_boundary.dist2.to_bits(), f64::to_bits(0.25));
        assert_eq!(
            crafted.nearest_within(&q, f64::from_bits(f64::to_bits(0.25) - 1)),
            None,
            "one ulp below the boundary must miss"
        );
        // From (1, 0) records 0, 1 and 2 are all at d2 1: an exact tie on the
        // boundary goes to the lowest index, as in `nearest`.
        assert_eq!(
            crafted.nearest_within(&[1.0, 0.0], 1.0).map(|h| h.index),
            Some(0)
        );
        // Record 0 is (0, 0): a zero cutoff still counts as a hit on it.
        assert_eq!(crafted.nearest_within(&q, 0.0), None);
        assert_eq!(
            crafted.nearest_within(&[0.0, 0.0], 0.0).map(|h| h.index),
            Some(0)
        );
        assert_eq!(crafted.nearest_within(&q, -1.0), None);
        assert_eq!(crafted.nearest_within(&q, f64::NAN), None);
        assert_eq!(crafted.nearest_within(&q, f64::INFINITY), None);
        // The zero-radius case resolves to the record itself, like `nearest`.
        let rec = crafted
            .nearest_within(&[3.0, 3.0], 0.0)
            .expect("exact record");
        assert_eq!((rec.index, rec.dist2), (3, 0.0));
    }

    #[test]
    fn ball_search_is_sorted() {
        let Some(tree) = codes_tree() else {
            return;
        };
        let mut rng = Lcg(1);
        let q: Vec<f64> = (0..tree.ndim)
            .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
            .collect();
        // Grow the radius until several records fall inside it: the query point is
        // random, so a fixed radius could land far from every record of an
        // arbitrarily small installed tree.
        let mut tol = 0.05;
        let mut hits = tree.ball_search(&q, tol);
        while hits.len() <= 1 && tol < 16.0 {
            tol *= 2.0;
            hits = tree.ball_search(&q, tol);
        }
        assert!(
            hits.len() > 1,
            "expected several hits at tol {tol}, got {}",
            hits.len()
        );
        assert!(hits.is_sorted_by(|a, b| a.dist2 <= b.dist2));
    }

    /// Drift guard: `ball_search_into` is a wrapper over `ball_search_visit`, so today
    /// this compares a thing to itself; it earns its keep if that ever stops being
    /// true (one entry point changing without the other). The shared traversal ORDER
    /// is pinned by [`ball_search_visit_yields_documented_traversal_order`].
    #[test]
    fn ball_search_visit_matches_ball_search_into() {
        let Some(tree) = codes_tree() else {
            return;
        };
        let mut rng = Lcg(7);
        let q: Vec<f64> = (0..tree.ndim)
            .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
            .collect();
        for tol in [0.0, 0.05, 0.5, 4.0] {
            let mut buffered = Vec::new();
            tree.ball_search_into(&q, tol, &mut buffered);
            let mut visited = Vec::new();
            tree.ball_search_visit(&q, tol, |index, dist2| visited.push(Hit { index, dist2 }));
            assert_eq!(visited.len(), buffered.len(), "visit at tol {tol}");
            for (v, b) in visited.iter().zip(buffered.iter()) {
                assert_eq!((v.index, v.dist2.to_bits()), (b.index, b.dist2.to_bits()));
            }
        }
    }

    /// Pins the FROZEN traversal order of [`KdTree::ball_search_visit`]: a DFS that
    /// takes the FAR child's subtree before the near one's (how `descend` pushes so a
    /// pop takes the far child first), records ascending inside a leaf. `verify`'s
    /// `collect_refs` stable-sorts by distance, so tied reference distances resolve to
    /// THIS order and the printed `refxy` would change if it changed. The reference
    /// walk below mirrors that child order but is written independently of the
    /// iterative stack, so it catches a flip of either the near/far choice or the leaf
    /// record direction.
    #[test]
    fn ball_search_visit_yields_documented_traversal_order() {
        let Some(tree) = codes_tree() else {
            return;
        };
        fn walk(
            tree: &KdTree,
            node: usize,
            query: &[f64],
            tol: f64,
            maxd2: f64,
            out: &mut Vec<(usize, u64)>,
        ) {
            if node >= tree.ninterior {
                let (l, r) = tree.leaf_range(node - tree.ninterior);
                for rec in l..r {
                    let mut d2 = 0.0;
                    for (d, &qv) in query.iter().enumerate() {
                        let delta = qv - tree.coord(rec, d);
                        d2 += delta * delta;
                    }
                    if d2 <= maxd2 {
                        out.push((rec, d2.to_bits()));
                    }
                }
                return;
            }
            let (dim, rsplit) = tree.split_external(node);
            let gap = query[dim] - rsplit;
            let (near, far) = if gap < 0.0 {
                (2 * node + 1, 2 * node + 2)
            } else {
                (2 * node + 2, 2 * node + 1)
            };
            if f64::abs(gap) <= tol {
                walk(tree, far, query, tol, maxd2, out);
            }
            walk(tree, near, query, tol, maxd2, out);
        }
        let mut rng = Lcg(11);
        for _ in 0..4 {
            let q: Vec<f64> = (0..tree.ndim)
                .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
                .collect();
            // Grow the radius until the ball is non-empty: a random point can sit far
            // from every record of a small installed tree, and an empty comparison
            // would pin nothing. Every radius tried checks the order.
            let mut tol = 0.05;
            loop {
                let mut want = Vec::new();
                walk(&tree, 0, &q, tol, tol * tol, &mut want);
                let mut got = Vec::new();
                tree.ball_search_visit(&q, tol, |i, d| got.push((i, d.to_bits())));
                assert_eq!(got.len(), want.len(), "traversal count at tol {tol}");
                for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                    assert_eq!(g, w, "traversal order diverged at {i} (tol {tol})");
                }
                if !got.is_empty() || tol >= 16.0 {
                    assert!(!got.is_empty(), "no hits even at tol {tol}");
                    break;
                }
                tol *= 2.0;
            }
        }
    }

    #[test]
    fn dimmask_matches_the_documented_formula() {
        // dimmask = 2^ceil(log2(ndim)) - 1: the bits needed to number the dims.
        assert_eq!(dimmask_for(1).unwrap(), 0);
        assert_eq!(dimmask_for(2).unwrap(), 1);
        assert_eq!(dimmask_for(3).unwrap(), 3);
        assert_eq!(dimmask_for(4).unwrap(), 3);
        assert_eq!(dimmask_for(5).unwrap(), 7);
        // A corrupt ndim must be rejected, not loop forever.
        assert!(dimmask_for(0).is_err());
        assert!(dimmask_for(MAX_NDIM + 1).is_err());
        assert!(node_counts_err(0).is_err());
    }

    fn node_counts_err(nnodes: usize) -> Result<(usize, usize)> {
        KdTree::node_counts(nnodes)
    }

    /// In the old-format indices the `codes` tree stores its splits as `u16`
    /// (`KDT_INT`): the dimension in the low `dimmask` bits, the split value above
    /// them. That the dimension is inside `ndim` is the property that can fail on data;
    /// a masked value has zero bits inside `dimmask` by construction, asserted as a
    /// guard on the unpacking itself.
    #[test]
    fn split_values_are_masked_multiples() {
        let Some(tree) = codes_tree() else {
            return;
        };
        assert_eq!(tree.int_kind, IntKind::U16);
        for node in 0..tree.ninterior {
            let (dim, value) = tree.split_at(node);
            assert!(dim < tree.ndim, "split dim {dim} out of ndim");
            assert_eq!(value & tree.dimmask, 0, "split value {value} is not masked");
        }
    }

    // --- Hostile kd-tree files, built in the test -------------------------

    const TREE: &str = "stars";

    /// Writer for the format's chunk layout: a primary header plus one BINTABLE per
    /// chunk, each holding a single `A` (character blob) column.
    #[derive(Default)]
    struct FitsBuf(Vec<u8>);

    impl FitsBuf {
        fn block(&mut self, cards: &[String]) {
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
            self.0.extend_from_slice(&block);
        }

        /// A BINTABLE HDU: `cards` describe it, `data` is its padded data area.
        fn table(&mut self, cards: &[String], data: &[u8]) {
            self.block(cards);
            let mut area = data.to_vec();
            let pad =
                usize::div_ceil(area.len(), crate::fits::BLOCK_SIZE) * crate::fits::BLOCK_SIZE;
            area.resize(pad, 0);
            self.0.extend_from_slice(&area);
        }
    }

    fn card(key: &str, value: &str) -> String {
        format!("{key:<8}= {value:<20}")
    }

    fn str_card(key: &str, value: &str) -> String {
        card(key, &format!("'{value}'"))
    }

    fn table_cards(chunk: &str, row_len: usize, nrows: usize) -> Vec<String> {
        vec![
            str_card("XTENSION", "BINTABLE"),
            card("BITPIX", "8"),
            card("NAXIS", "2"),
            card("NAXIS1", &row_len.to_string()),
            card("NAXIS2", &nrows.to_string()),
            card("TFIELDS", "1"),
            str_card("TFORM1", &format!("{row_len}A")),
            str_card("TTYPE1", &format!("kdtree_{chunk}_{TREE}")),
        ]
    }

    /// The header cards of a crafted tree. The lr/split/range/data chunks always
    /// describe a *valid* 2-D, 4-record, 3-node tree, so when a test corrupts a header
    /// card the guard — not a later size mismatch — is what rejects it.
    struct Spec {
        ndim: i64,
        ndata: i64,
        nnodes: i64,
        minval: Vec<f64>,
        maxval: Vec<f64>,
        scale: f64,
        linear_lr: bool,
    }

    impl Spec {
        fn valid() -> Self {
            Spec {
                ndim: 2,
                ndata: 4,
                nnodes: 3,
                minval: vec![0.0, 0.0],
                maxval: vec![4.0, 4.0],
                scale: 1.0,
                linear_lr: false,
            }
        }
    }

    /// `x` is 0, 1 in the left leaf and 2, 3 in the right one, so the single split
    /// plane (dim 0, value 2) agrees with the leaf order and the searches can be
    /// compared against brute force.
    fn crafted_records() -> [[u32; 2]; 4] {
        [[0, 0], [1, 1], [2, 0], [3, 3]]
    }

    fn crafted_bytes(spec: &Spec) -> Vec<u8> {
        let mut w = FitsBuf::default();
        w.block(&[
            card("SIMPLE", "T"),
            card("BITPIX", "8"),
            card("NAXIS", "0"),
            str_card("ENDIAN", "04:03:02:01"),
        ]);

        let mut header = table_cards("header", 0, 0);
        header.extend_from_slice(&[
            str_card("KDT_NAME", TREE),
            card("KDT_NDAT", &spec.ndata.to_string()),
            card("KDT_NDIM", &spec.ndim.to_string()),
            card("KDT_NNOD", &spec.nnodes.to_string()),
            card("KDT_VER", "1"),
            str_card("KDT_EXT", "double"),
            str_card("KDT_INT", "u32"),
            str_card("KDT_DATA", "u32"),
            card("KDT_LINL", if spec.linear_lr { "T" } else { "F" }),
        ]);
        w.block(&header);

        if !spec.linear_lr {
            // Inclusive right ends: leaf 0 owns records 0..=1, leaf 1 2..=3.
            let lr: Vec<u8> = [1u32, 3]
                .iter()
                .flat_map(|v| u32::to_le_bytes(*v))
                .collect();
            w.table(&table_cards("lr", 4, 2), &lr);
        }
        // One interior node: dim = 2 & dimmask(2) = 0, split value 2.
        let split: Vec<u8> = [2u32].iter().flat_map(|v| u32::to_le_bytes(*v)).collect();
        w.table(&table_cards("split", 4, 1), &split);

        let range: Vec<u8> = spec
            .minval
            .iter()
            .chain(spec.maxval.iter())
            .copied()
            .flat_map(f64::to_le_bytes)
            .chain(f64::to_le_bytes(spec.scale))
            .collect();
        let nrows = range.len() / 8;
        w.table(&table_cards("range", 8, nrows), &range);

        let data: Vec<u8> = crafted_records()
            .iter()
            .flatten()
            .flat_map(|v| u32::to_le_bytes(*v))
            .collect();
        w.table(&table_cards("data", 8, 4), &data);
        w.0
    }

    fn load_crafted(tag: &str, spec: &Spec) -> KdTree {
        let fits = Fits::from_bytes(crafted_bytes(spec))
            .unwrap_or_else(|e| panic!("case {tag}: crafted FITS failed to parse: {e:#}"));
        KdTree::load(&fits, TREE)
            .unwrap_or_else(|e| panic!("case {tag}: valid crafted kd-tree was rejected: {e:#}"))
    }

    /// A structurally valid 3-D, 4-record, 3-node tree with the given element kind
    /// (`u16` or `u32`) and records, for the flat little-endian decode fast paths. Its
    /// split plane is never consulted by those tests.
    fn crafted_3d_tree(kind: &str, records: &[[u32; 3]]) -> KdTree {
        let rows: Vec<&[u32]> = records.iter().map(|r| r.as_slice()).collect();
        crafted_flat_tree(kind, 3, &rows)
    }

    /// [`crafted_3d_tree`] generalized to any `ndim`: a structurally valid `ndim`-D,
    /// 4-record, 3-node tree with flat little-endian blobs, one interior node
    /// splitting on dim `2 & dimmask(ndim)` at value 0, and `rows` as records (each
    /// `ndim` long).
    fn crafted_flat_tree(kind: &str, ndim: usize, rows: &[&[u32]]) -> KdTree {
        let elem = if kind == "u16" { 2 } else { 4 };
        let mut w = FitsBuf::default();
        w.block(&[
            card("SIMPLE", "T"),
            card("BITPIX", "8"),
            card("NAXIS", "0"),
            str_card("ENDIAN", "04:03:02:01"),
        ]);
        let mut header = table_cards("header", 0, 0);
        header.extend_from_slice(&[
            str_card("KDT_NAME", TREE),
            card("KDT_NDAT", "4"),
            card("KDT_NDIM", &ndim.to_string()),
            card("KDT_NNOD", "3"),
            card("KDT_VER", "1"),
            str_card("KDT_EXT", "double"),
            str_card("KDT_INT", "u32"),
            str_card("KDT_DATA", kind),
            card("KDT_LINL", "F"),
        ]);
        w.block(&header);
        // Leaf 0 owns records 0..=1, leaf 1 owns 2..=3.
        let lr: Vec<u8> = [1u32, 3].iter().flat_map(|v| v.to_le_bytes()).collect();
        w.table(&table_cards("lr", 4, 2), &lr);
        // One interior node; dim = 2 & dimmask(ndim) < ndim for every ndim these
        // tests build, value 0.
        let split: Vec<u8> = [2u32].iter().flat_map(|v| v.to_le_bytes()).collect();
        w.table(&table_cards("split", 4, 1), &split);
        let range: Vec<u8> = (0..ndim)
            .map(|_| 0.0f64)
            .chain((0..ndim).map(|_| 4.0f64))
            .chain([1.0f64])
            .flat_map(|v| v.to_le_bytes())
            .collect();
        w.table(&table_cards("range", 8, range.len() / 8), &range);
        let data: Vec<u8> = rows
            .iter()
            .flat_map(|r| r.iter().copied())
            .flat_map(|v| {
                if elem == 2 {
                    u16::try_from(v).expect("u16 record").to_le_bytes().to_vec()
                } else {
                    v.to_le_bytes().to_vec()
                }
            })
            .collect();
        w.table(&table_cards("data", ndim * elem, 4), &data);
        let fits = Fits::from_bytes(w.0)
            .unwrap_or_else(|e| panic!("crafted {kind} {ndim}-D tree failed to parse: {e:#}"));
        KdTree::load(&fits, TREE)
            .unwrap_or_else(|e| panic!("crafted {kind} {ndim}-D tree was rejected: {e:#}"))
    }

    fn crafted_3d_records() -> [[u32; 3]; 4] {
        [[0, 0, 0], [1, 2, 3], [65535, 0, 4], [3, 3, 3]]
    }

    /// The batched flat little-endian decode must return exactly what three
    /// `coord` calls return, for both element kinds.
    #[test]
    fn coord3_matches_three_coord_calls() {
        for kind in ["u16", "u32"] {
            let tree = crafted_3d_tree(kind, &crafted_3d_records());
            assert!(tree.data.is_flat(), "crafted {kind} data view is not flat");
            for rec in 0..4 {
                assert_eq!(
                    tree.coord3(rec),
                    [tree.coord(rec, 0), tree.coord(rec, 1), tree.coord(rec, 2)],
                    "coord3 disagrees for kind {kind} record {rec}"
                );
            }
        }
    }

    /// The flat little-endian `dist2_cutoff` fast path must report the same distance -
    /// including the `INFINITY` bail - as the general per-coordinate decode, for both
    /// element kinds.
    #[test]
    fn dist2_cutoff_flat_path_matches_the_general_decode() {
        for kind in ["u16", "u32"] {
            let tree = crafted_3d_tree(kind, &crafted_3d_records());
            for q in [[0.5, 0.5, 0.5], [2.0, 1.0, 3.0], [-1.0, 4.0, 0.0]] {
                for &cutoff in &[100.0, 0.5] {
                    for rec in 0..4 {
                        let d2 = tree.dist2_cutoff(rec, &q, cutoff);
                        let mut e = 0.0;
                        for (d, &qd) in q.iter().enumerate() {
                            let delta = qd - tree.coord(rec, d);
                            e += delta * delta;
                        }
                        if e > cutoff {
                            assert!(d2.is_infinite(), "kind {kind} rec {rec} must bail");
                        } else {
                            assert_eq!(d2.to_bits(), e.to_bits(), "kind {kind} rec {rec}");
                        }
                    }
                }
            }
        }
    }

    /// Which `Scan` legs the crafted trees collectively exercised, so the differential
    /// test can tell a compared leg from one that never ran.
    #[derive(Default, Debug)]
    struct ScanLegs {
        fixed3: usize,
        fixed4: usize,
        general_dist: usize,
        split_fast: usize,
        split_general: usize,
        lr_fast: usize,
        lr_general: usize,
    }

    /// The decode legs `Scan` hoists must return bit-identical values to the general
    /// accessors on every crafted layout: the fixed-`ndim` distance legs and the
    /// general fallback (`ndim` 2), the `split` and `lr` fast legs and their delegating
    /// legs (big-endian, `linear_lr`). The cutoffs include the exact distance,
    /// `INFINITY` and NaN, so the `INFINITY` bail is compared on both paths. The 4-D
    /// leg is pinned at the record level only: that tree's split plane is deliberately
    /// not consistent with its records, so traversal-level pins live on consistent
    /// trees - `crafted_tree_searches_match_brute_force` (3-D) and the installed-tree
    /// `searches_match_brute_force`, whose codes tree is 4-D for a `dimquads=4` index.
    #[test]
    fn scan_paths_match_the_general_accessors() {
        let mut trees: Vec<(&str, KdTree)> = Vec::new();
        for kind in ["u16", "u32"] {
            let tag = if kind == "u16" { "3d_u16" } else { "3d_u32" };
            trees.push((tag, crafted_3d_tree(kind, &crafted_3d_records())));
        }
        let four_d: [[u32; 4]; 4] = [[0, 0, 0, 0], [1, 2, 3, 4], [65535, 0, 4, 1], [3, 3, 3, 3]];
        for kind in ["u16", "u32"] {
            let tag = if kind == "u16" { "4d_u16" } else { "4d_u32" };
            let rows: Vec<&[u32]> = four_d.iter().map(|r| r.as_slice()).collect();
            trees.push((tag, crafted_flat_tree(kind, 4, &rows)));
        }
        trees.push(("ndim2", load_crafted("scan_valid", &Spec::valid())));
        trees.push((
            "linear_lr",
            load_crafted(
                "scan_linl",
                &Spec {
                    linear_lr: true,
                    ..Spec::valid()
                },
            ),
        ));
        trees.push(("be_u16", crafted_big_endian_tree()));

        let mut legs = ScanLegs::default();
        for (tag, tree) in &trees {
            let queries: Vec<Vec<f64>> = vec![
                vec![0.0; tree.ndim],
                vec![1.5; tree.ndim],
                vec![-3.0; tree.ndim],
                vec![f64::NAN; tree.ndim],
            ];
            for q in &queries {
                let scan = Scan::new(tree, q);
                match scan.dist {
                    Dist::Fixed3 { .. } => legs.fixed3 += 1,
                    Dist::Fixed4 { .. } => legs.fixed4 += 1,
                    Dist::General => legs.general_dist += 1,
                }
                if scan.split_raw.is_some() {
                    legs.split_fast += 1;
                } else {
                    legs.split_general += 1;
                }
                if scan.lr_raw.is_some() {
                    legs.lr_fast += 1;
                } else {
                    legs.lr_general += 1;
                }
                for rec in 0..tree.ndata {
                    let mut exact = 0.0;
                    for (d, &qv) in q.iter().enumerate() {
                        let delta = qv - tree.coord(rec, d);
                        exact += delta * delta;
                    }
                    for &cutoff in &[0.0, -1.0, 0.5, exact, f64::INFINITY, f64::NAN] {
                        let fast = scan.dist2(rec, cutoff);
                        let slow = tree.dist2_cutoff(rec, q, cutoff);
                        assert_eq!(
                            fast.to_bits(),
                            slow.to_bits(),
                            "tree {tag} rec {rec} cutoff {cutoff}"
                        );
                    }
                }
            }
            // The descent-side legs do not depend on the query; one scan's worth of
            // nodes and leaves is the whole table.
            let zero_query = vec![0.0; tree.ndim];
            let scan = Scan::new(tree, &zero_query);
            for node in 0..tree.ninterior {
                let (fd, fv) = scan.split_external(node);
                let (sd, sv) = tree.split_external(node);
                assert_eq!(fd, sd, "tree {tag} node {node} split dim");
                assert_eq!(fv.to_bits(), sv.to_bits(), "tree {tag} node {node} value");
            }
            for leaf in 0..tree.nbottom {
                assert_eq!(
                    scan.leaf_range(leaf),
                    tree.leaf_range(leaf),
                    "tree {tag} leaf {leaf}"
                );
            }
        }
        assert!(
            legs.fixed3 > 0 && legs.fixed4 > 0 && legs.general_dist > 0,
            "the distance legs were not all exercised: {legs:?}"
        );
        assert!(
            legs.split_fast > 0 && legs.split_general > 0,
            "the split legs were not all exercised: {legs:?}"
        );
        assert!(
            legs.lr_fast > 0 && legs.lr_general > 0,
            "the lr legs were not all exercised: {legs:?}"
        );
    }

    /// The leaf a record of [`crafted_partition_tree`] lands in: level 0 splits dim 0
    /// at 8, level 1 splits dim 1 at 8 in both halves.
    fn partition_leaf(r: &[u32; 3]) -> usize {
        usize::from(r[0] > 8) * 2 + usize::from(r[1] > 8)
    }

    /// `records` in leaf order (leaf 0 first) with the four leaf sizes, the layout
    /// [`crafted_partition_tree`] writes into `lr`.
    fn partition_into_leaves(records: &[[u32; 3]]) -> (Vec<[u32; 3]>, [usize; 4]) {
        let mut counts = [0usize; 4];
        for r in records {
            counts[partition_leaf(r)] += 1;
        }
        let mut ordered = Vec::with_capacity(records.len());
        for leaf in 0..4 {
            ordered.extend(
                records
                    .iter()
                    .copied()
                    .filter(|r| partition_leaf(r) == leaf),
            );
        }
        (ordered, counts)
    }

    /// A valid 3-D tree over `ordered` (the leaf-ordered records): a perfect tree of 7
    /// nodes - 3 interior, 4 leaves - that splits on dim 0 at 8 and then on dim 1 at
    /// 8, so each half holds exactly the records its range selects. Unlike
    /// [`crafted_3d_tree`] its split planes are consistent with its data, which is what
    /// makes the pruning searches (and anything compared against them) meaningful.
    /// Leaf 0 and leaf 3 must be non-empty: an empty leading or trailing leaf cannot
    /// be expressed in `lr`.
    fn crafted_partition_tree(kind: &str, ordered: &[[u32; 3]], counts: &[usize; 4]) -> KdTree {
        let elem = if kind == "u16" { 2 } else { 4 };
        let mut w = FitsBuf::default();
        w.block(&[
            card("SIMPLE", "T"),
            card("BITPIX", "8"),
            card("NAXIS", "0"),
            str_card("ENDIAN", "04:03:02:01"),
        ]);
        let mut header = table_cards("header", 0, 0);
        header.extend_from_slice(&[
            str_card("KDT_NAME", TREE),
            card("KDT_NDAT", &ordered.len().to_string()),
            card("KDT_NDIM", "3"),
            card("KDT_NNOD", "7"),
            card("KDT_VER", "1"),
            str_card("KDT_EXT", "double"),
            str_card("KDT_INT", "u32"),
            str_card("KDT_DATA", kind),
            card("KDT_LINL", "F"),
        ]);
        w.block(&header);
        // Inclusive right ends; an empty middle leaf repeats the previous end, which
        // `leaf_range` reads back as an empty range.
        let mut lr: Vec<u8> = Vec::new();
        let mut end: i64 = -1;
        for &c in counts {
            end += i64::try_from(c).expect("leaf count fits");
            lr.extend_from_slice(&u32::try_from(end).expect("record index fits").to_le_bytes());
        }
        w.table(&table_cards("lr", 4, 4), &lr);
        // node 0: dim 0, value 8; nodes 1 and 2: dim 1, value 8 (the low dimmask bits
        // carry the dimension, the value stays a multiple of 4).
        let split: Vec<u8> = [8u32, 9, 9].iter().flat_map(|v| v.to_le_bytes()).collect();
        w.table(&table_cards("split", 4, 3), &split);
        let range: Vec<u8> = [0.0f64, 0.0, 0.0, 16.0, 16.0, 16.0, 1.0]
            .iter()
            .flat_map(|&v| v.to_le_bytes())
            .collect();
        let nrows = range.len() / 8;
        w.table(&table_cards("range", 8, nrows), &range);
        let data: Vec<u8> = ordered
            .iter()
            .flat_map(|r| {
                r.iter().flat_map(|&v| {
                    if elem == 2 {
                        u16::try_from(v).expect("u16 record").to_le_bytes().to_vec()
                    } else {
                        v.to_le_bytes().to_vec()
                    }
                })
            })
            .collect();
        w.table(&table_cards("data", 3 * elem, ordered.len()), &data);
        let fits = Fits::from_bytes(w.0)
            .unwrap_or_else(|e| panic!("crafted partition tree failed to parse: {e:#}"));
        KdTree::load(&fits, TREE).expect("crafted partition tree was rejected")
    }

    /// The exactness contract on trees whose split planes agree with their records:
    /// for every probe query, "some star of the footprint ball lies within the lookup
    /// cutoff" must be exactly what `nearest_within(..).is_some()` answers, and both
    /// must match a brute-force scan. Every witness of every probe is also asserted to
    /// sit inside the collected ball (the superset property the batched verdict rests
    /// on), and a tight-frame leg shows a chordless `sqrt(far2)` ball loses a witness
    /// the searches find, so that containment assertion is load-bearing. All over
    /// randomized records and query frames, plus the legs the probe's argument rests
    /// on: the inclusive cutoff boundary, a query with nothing near it, a non-finite
    /// query, an empty ball and a collection that reaches the cap.
    #[test]
    fn batched_probe_ball_matches_nearest_within() {
        let mut rng = Lcg(0xc0ffee);
        let mut stats = BatchedStats::default();
        for trial in 0..40 {
            let mut records = vec![[0u32, 0, 0], [12, 12, 4]];
            for _ in 0..(trial % 9) {
                records.push([
                    usize::try_from(rng.next_u64() % 5).expect("small") as u32 * 4,
                    usize::try_from(rng.next_u64() % 5).expect("small") as u32 * 4,
                    usize::try_from(rng.next_u64() % 5).expect("small") as u32 * 4,
                ]);
            }
            let (ordered, counts) = partition_into_leaves(&records);
            for kind in ["u16", "u32"] {
                let tree = crafted_partition_tree(kind, &ordered, &counts);
                // A tree whose halves did not match their split planes would make
                // every search below wrong in the same direction.
                tree.self_check().expect("crafted partition tree");
                batched_vs_nearest(&tree, &mut rng, &mut stats);
            }
        }
        assert!(
            stats.non_empty_balls >= 30,
            "balls were hardly ever non-empty: {stats:?}"
        );
        assert!(
            stats.cap_legs >= 30,
            "the cap leg hardly ever ran: {stats:?}"
        );
        assert!(
            stats.matches > 0 && stats.misses > 0,
            "the comparison only ever saw one verdict: {stats:?}"
        );
        assert!(
            stats.tight_legs == stats.balls && stats.tight_legs >= 30,
            "the tight-frame leg did not run on every tree: {stats:?}"
        );
    }

    /// How often the differential leg of `batched_vs_nearest` saw each outcome, so the
    /// test can tell a passing comparison from one that never had anything to compare.
    #[derive(Default, Debug)]
    struct BatchedStats {
        balls: usize,
        non_empty_balls: usize,
        cap_legs: usize,
        matches: usize,
        misses: usize,
        tight_legs: usize,
    }

    /// One randomized frame of probe queries against one tree.
    fn batched_vs_nearest(tree: &KdTree, rng: &mut Lcg, stats: &mut BatchedStats) {
        let chord = 1.3;
        let cutoff2 = chord * chord;
        let pick = |rng: &mut Lcg| {
            [
                rng.next_range(-1.0, 17.0),
                rng.next_range(-1.0, 17.0),
                rng.next_range(-1.0, 17.0),
            ]
        };
        let center = pick(rng);
        let mut qs: Vec<[f64; 3]> = (0..8).map(|_| pick(rng)).collect();
        // A query sitting exactly on a record, and one kept aside for the boundary
        // leg below.
        let sampled = tree.sample_records();
        let pick_rec = usize::try_from(rng.next_u64() % u64::try_from(sampled.len()).unwrap())
            .unwrap_or(usize::MAX);
        qs.push(tree.coord3(sampled[pick_rec % sampled.len()]));
        let boundary = pick(rng);
        qs.push(boundary);

        // `probe_ball_radius` is what the probe uses, so this measures the ball the
        // probe would really collect.
        let radius = probe_ball_radius(&center, &qs, chord);
        assert!(radius.is_finite(), "a finite frame must give a finite ball");
        let mut buf = Vec::new();
        assert!(
            probe_ball_coords(tree, &center, radius, usize::MAX, &mut buf),
            "no cap can abort a collection of {} records",
            tree.ndata
        );
        for q in &qs {
            // Every witness of this probe - every star `nearest_within` could hand it
            // - must sit inside the collected footprint ball: the superset property
            // the batched verdict rests on. The `+ chord` term of `probe_ball_radius`
            // is what puts the far witnesses in: a star at the probe's cutoff
            // boundary, on the far side of the probe from the center, is a chord
            // farther out than the probe itself.
            let witnesses: Vec<usize> = (0..tree.ndata)
                .filter(|rec| distsq(&tree.coord3(*rec), q) <= cutoff2)
                .collect();
            for &rec in &witnesses {
                assert!(
                    distsq(&tree.coord3(rec), &center) <= radius * radius,
                    "witness record {rec} of probe {q:?} sits outside the footprint ball \
                     (radius {radius}): the batched path would miss a match the searches find"
                );
            }
            let batched = buf.iter().any(|s| within_chord2(q, s, cutoff2));
            let searched = tree.nearest_within(q, cutoff2).is_some();
            let brute = !witnesses.is_empty();
            assert_eq!(batched, searched, "batched vs nearest_within, {q:?}");
            assert_eq!(searched, brute, "nearest_within vs brute force, {q:?}");
            if batched {
                stats.matches += 1;
            } else {
                stats.misses += 1;
            }
        }
        stats.balls += 1;
        stats.non_empty_balls += usize::from(!buf.is_empty());

        // The inclusive boundary: with the cutoff at the nearest record's own squared
        // distance - inside the lookup ball, so inside the footprint ball - both paths
        // must report the match the `<=` allows.
        let dmin2 = (0..tree.ndata).fold(f64::INFINITY, |m, rec| {
            f64::min(m, distsq(&tree.coord3(rec), &boundary))
        });
        if dmin2 <= cutoff2 {
            assert!(
                buf.iter().any(|s| within_chord2(&boundary, s, dmin2)),
                "a record at exactly the cutoff must match, {boundary:?}"
            );
            assert!(tree.nearest_within(&boundary, dmin2).is_some());
        }

        // A non-finite probe: no match on either path. A ball built from one must be
        // refused outright - a NaN radius silently collects nothing, which would read
        // as a rejection instead of as "no ball".
        let nan = [f64::NAN, 1.0, 1.0];
        assert!(!buf.iter().any(|s| within_chord2(&nan, s, cutoff2)));
        assert!(tree.nearest_within(&nan, cutoff2).is_none());
        let mut nan_qs = qs.clone();
        nan_qs.push(nan);
        assert!(probe_ball_radius(&center, &nan_qs, chord).is_nan());
        let mut nan_buf = Vec::new();
        assert!(probe_ball_coords(
            tree,
            &center,
            f64::NAN,
            usize::MAX,
            &mut nan_buf
        ));
        assert!(nan_buf.is_empty(), "a NaN radius must collect nothing");

        // A ball that holds nothing at all: the empty-collection leg.
        let mut empty = Vec::new();
        assert!(probe_ball_coords(
            tree,
            &[1e3, 1e3, 1e3],
            1e-9,
            usize::MAX,
            &mut empty
        ));
        assert!(empty.is_empty());

        // The cap: what fits is accepted, one more aborts and leaves the buffer empty
        // for the caller's fallback path.
        let n = buf.len();
        if n >= 1 {
            stats.cap_legs += 1;
            let mut fits = Vec::new();
            assert!(probe_ball_coords(tree, &center, radius, n, &mut fits));
            assert_eq!(fits.len(), n);
            let mut over = Vec::new();
            assert!(!probe_ball_coords(tree, &center, radius, n - 1, &mut over));
            assert!(
                over.is_empty(),
                "an aborted collection must come back empty"
            );
        }

        // The tight frame: one probe at `[12, 12, 3]`, whose only witness in every
        // tree built here is the record `[12, 12, 4]` - strictly farther from the
        // frame center than the probe is, so the `+ chord` term of `probe_ball_radius`
        // is the only thing that pulls it into the ball. The control radius
        // `sqrt(far2)` (the same ball without the chord term) must be shown to LOSE
        // that witness while the searches still find it: that is what makes the
        // containment assertion above load-bearing, not luck.
        let corner = [12.0, 12.0, 3.0];
        let tight_center = [0.0, 0.0, 0.0];
        let far2 = distsq(&corner, &tight_center);
        let tight_radius = probe_ball_radius(&tight_center, &[corner], chord);
        assert!(
            distsq(&[12.0, 12.0, 4.0], &tight_center) <= tight_radius * tight_radius,
            "the far witness must sit inside the footprint ball"
        );
        let mut held = Vec::new();
        assert!(probe_ball_coords(
            tree,
            &tight_center,
            tight_radius,
            usize::MAX,
            &mut held
        ));
        assert!(
            held.iter().any(|s| within_chord2(&corner, s, cutoff2)),
            "the footprint ball must hold the corner probe's witness"
        );
        // Without the chord term it does not: the searches find the match, the
        // chordless ball holds nothing that answers it.
        assert!(tree.nearest_within(&corner, cutoff2).is_some());
        let mut lost = Vec::new();
        assert!(probe_ball_coords(
            tree,
            &tight_center,
            f64::sqrt(far2),
            usize::MAX,
            &mut lost
        ));
        assert!(
            !lost.iter().any(|s| within_chord2(&corner, s, cutoff2)),
            "a sqrt(far2) ball must lose the far witness - if this holds, the \
             containment assertion above can never fire and pins nothing"
        );
        stats.tight_legs += 1;
    }

    /// `load` must reject the crafted tree with a clean `Err` mentioning `needle`
    /// (never a panic or a hang).
    fn assert_rejected(tag: &str, spec: &Spec, needle: &str) {
        let fits = Fits::from_bytes(crafted_bytes(spec))
            .unwrap_or_else(|e| panic!("case {tag}: crafted FITS failed to parse: {e:#}"));
        let err = KdTree::load(&fits, TREE)
            .err()
            .unwrap_or_else(|| panic!("case {tag}: crafted kd-tree was accepted"));
        let msg = format!("{err:#}");
        eprintln!("case {tag} rejected: {msg}");
        assert!(
            msg.contains(needle),
            "case {tag}: {msg} does not mention {needle:?}"
        );
    }

    /// The guards must not over-reject: the crafted baseline loads and works.
    #[test]
    fn crafted_valid_tree_loads() {
        let tree = load_crafted("valid", &Spec::valid());
        assert_eq!((tree.ndim, tree.ndata, tree.nnodes), (2, 4, 3));
        assert_eq!((tree.nbottom, tree.ninterior), (2, 1));
        // `lr` stores inclusive right ends; `leaf_range` reports half-open ones.
        assert_eq!(tree.leaf_range(0), (0, 2));
        assert_eq!(tree.leaf_range(1), (2, 4));
        tree.check_structure().expect("crafted structure");
        tree.self_check().expect("crafted searches");
    }

    /// The big-endian crafted file of [`crafted_big_endian_u16_tree`], loaded: the
    /// layout that keeps every `Scan` leg on its general (delegating) path -
    /// big-endian decoding, `u16` splits and a non-flat `lr` column.
    fn crafted_big_endian_tree() -> KdTree {
        let mut w = FitsBuf::default();
        w.block(&[
            card("SIMPLE", "T"),
            card("BITPIX", "8"),
            card("NAXIS", "0"),
            str_card("ENDIAN", "01:02:03:04"),
        ]);
        let mut header = table_cards("header", 0, 0);
        header.extend_from_slice(&[
            str_card("KDT_NAME", TREE),
            card("KDT_NDAT", "4"),
            card("KDT_NDIM", "2"),
            card("KDT_NNOD", "3"),
            card("KDT_VER", "1"),
            str_card("KDT_EXT", "double"),
            str_card("KDT_INT", "u16"),
            str_card("KDT_DATA", "u16"),
            card("KDT_LINL", "F"),
        ]);
        w.block(&header);

        // lr: 2 rows of 8 bytes, the u32 column in the first 4 bytes of each. The
        // filler column is 0xFF: were the element packing flat instead of per-row,
        // `lr[1]` would read it back as 0xFFFFFFFF.
        let mut lr = Vec::new();
        for v in [1u32, 3] {
            lr.extend_from_slice(&u32::to_be_bytes(v));
            lr.extend_from_slice(&[0xFF; 4]);
        }
        w.table(
            &[
                str_card("XTENSION", "BINTABLE"),
                card("BITPIX", "8"),
                card("NAXIS", "2"),
                card("NAXIS1", "8"),
                card("NAXIS2", "2"),
                card("TFIELDS", "2"),
                str_card("TFORM1", "4A"),
                str_card("TTYPE1", &format!("kdtree_lr_{TREE}")),
                str_card("TFORM2", "4A"),
                str_card("TTYPE2", "FILLER"),
            ],
            &lr,
        );

        // One u16 split: dim = 2 & dimmask(2) = 0, value 2.
        let split = u16::to_be_bytes(2).to_vec();
        w.table(&table_cards("split", 2, 1), &split);

        let range: Vec<u8> = [0.0f64, 0.0, 4.0, 4.0, 1.0]
            .iter()
            .flat_map(|v| f64::to_be_bytes(*v))
            .collect();
        w.table(&table_cards("range", 8, 5), &range);

        let data: Vec<u8> = crafted_records()
            .iter()
            .flatten()
            .flat_map(|v| u16::try_from(*v).expect("values fit u16").to_be_bytes())
            .collect();
        w.table(&table_cards("data", 4, 4), &data);

        let fits = Fits::from_bytes(w.0).expect("crafted BE FITS parses");
        KdTree::load(&fits, TREE).expect("crafted BE tree loads")
    }

    /// Big-endian file with `u16` data/split and a padded multi-row `lr` column: covers
    /// the non-flat (div/mod) element packing of `BlobView::at` and big-endian
    /// decoding at all three element reads (`coord`, `lr_entry`, `split_at`).
    #[test]
    fn crafted_big_endian_u16_tree() {
        let tree = crafted_big_endian_tree();
        assert_eq!(tree.data_kind, IntKind::U16);
        assert_eq!(tree.int_kind, IntKind::U16);
        for (rec, want) in crafted_records().iter().enumerate() {
            for (d, &w) in want.iter().enumerate() {
                assert_eq!(tree.coord(rec, d), f64::from(w), "coord({rec},{d})");
            }
        }
        assert_eq!(tree.lr_entry(0), 1);
        assert_eq!(tree.lr_entry(1), 3);
        assert_eq!(tree.leaf_range(1), (2, 4));
        assert_eq!(tree.split_at(0), (0, 2));
        tree.check_structure().expect("BE structure");
        tree.self_check().expect("BE searches");
    }

    #[test]
    fn crafted_linear_lr_ranges_tile_the_records() {
        let tree = load_crafted(
            "linear_lr",
            &Spec {
                linear_lr: true,
                ..Spec::valid()
            },
        );
        assert!(tree.linear_lr);
        assert_eq!(tree.leaf_range(0), (0, 2));
        assert_eq!(tree.leaf_range(1), (2, 4));
        tree.check_structure().expect("linear lr tiling");
        tree.self_check().expect("crafted searches");
    }

    /// `KDT_NDIM` must be rejected before any arithmetic uses it.
    #[test]
    fn crafted_ndim_is_rejected() {
        // -1 must not survive the conversion to usize and 0 is not a usable dimension:
        // both are header-level errors rather than a later blob-size mismatch, and the
        // needles below are those guards' own messages.
        assert_rejected(
            "ndim_neg",
            &Spec {
                ndim: -1,
                ..Spec::valid()
            },
            "negative KDT_NDIM",
        );
        for ndim in [0, MAX_NDIM as i64 + 1, i64::MAX] {
            assert_rejected(
                &format!("ndim_{ndim}"),
                &Spec {
                    ndim,
                    ..Spec::valid()
                },
                "KDT_NDIM",
            );
        }
    }

    #[test]
    fn crafted_negative_counts_are_rejected() {
        assert_rejected(
            "ndata_neg",
            &Spec {
                ndata: -1,
                ..Spec::valid()
            },
            "negative KDT_NDAT",
        );
        assert_rejected(
            "nnodes_neg",
            &Spec {
                nnodes: -1,
                ..Spec::valid()
            },
            "negative KDT_NNOD",
        );
        assert_rejected(
            "ndata_zero",
            &Spec {
                ndata: 0,
                ..Spec::valid()
            },
            "no data records",
        );
        assert_rejected(
            "nnodes_zero",
            &Spec {
                nnodes: 0,
                ..Spec::valid()
            },
            "has no nodes",
        );
        assert_rejected(
            "ndata_huge",
            &Spec {
                ndata: i64::MAX,
                ..Spec::valid()
            },
            "rows, ndata is",
        );
    }

    /// A full binary tree has an odd node count: with an even `KDT_NNOD` the children
    /// of the last interior node step one leaf past `lr` (see `node_counts`), which
    /// would only blow up in `leaf_range` during the first search, so it is rejected at
    /// load.
    #[test]
    fn crafted_even_nnodes_is_rejected() {
        for nnodes in [2i64, 4, 1_000_000] {
            assert_rejected(
                &format!("nnodes_even_{nnodes}"),
                &Spec {
                    nnodes,
                    ..Spec::valid()
                },
                "even",
            );
        }
        assert!(node_counts_err(2).is_err());
        assert!(node_counts_err(4).is_err());
        // Node ids live in `u32` search stacks.
        assert!(KdTree::node_counts(usize::MAX).is_err());
        assert_eq!(KdTree::node_counts(3).expect("odd"), (2, 1));
    }

    /// NaN or inverted range bounds dequantize into NaN coordinates, which match
    /// nothing: left alone they make every query miss, and the solver exits 1 (not
    /// solved) on what is really a corrupt-file 2.
    #[test]
    fn crafted_range_table_bounds_are_rejected() {
        assert_rejected(
            "minval_nan",
            &Spec {
                minval: vec![f64::NAN, 0.0],
                ..Spec::valid()
            },
            "non-finite range bound",
        );
        assert_rejected(
            "maxval_inf",
            &Spec {
                maxval: vec![0.0, f64::INFINITY],
                ..Spec::valid()
            },
            "non-finite range bound",
        );
        assert_rejected(
            "inverted_bounds",
            &Spec {
                minval: vec![0.5, 0.0],
                maxval: vec![0.25, 1.0],
                ..Spec::valid()
            },
            "minval[0]",
        );
        assert_rejected(
            "scale_nan",
            &Spec {
                scale: f64::NAN,
                ..Spec::valid()
            },
            "scale",
        );
        assert_rejected(
            "scale_inf",
            &Spec {
                scale: f64::INFINITY,
                ..Spec::valid()
            },
            "scale",
        );
    }

    /// The bailout sentinel must stay above every cutoff: once `cutoff >= 4.0`,
    /// `cutoff + EPSILON` rounds back to `cutoff`, so a sentinel of that shape would
    /// let a bailed-out record pass the callers' `d2 <= cutoff` test as a strictly
    /// farther "hit".
    #[test]
    fn dist2_bailout_sentinel_excludes_far_records() {
        let tree = load_crafted("bailout", &Spec::valid());
        let cutoff = 4.0;
        assert_eq!(
            cutoff + f64::EPSILON,
            cutoff,
            "why the old sentinel could not work"
        );
        // Record 3 sits at (3, 3): d2 = 18, far outside the cutoff.
        assert_eq!(tree.dist2_cutoff(3, &[0.0, 0.0], cutoff), f64::INFINITY);
        // An infinite cutoff never bails, so the exact distance is reported.
        assert_eq!(tree.dist2_cutoff(3, &[0.0, 0.0], f64::INFINITY), 18.0);
        // A record inside the cutoff keeps its exact distance.
        assert_eq!(tree.dist2_cutoff(1, &[0.0, 0.0], cutoff), 2.0);
        // End to end: at tol 2 (maxd2 = 4) record 3 must not be reported.
        let hits: Vec<usize> = tree
            .ball_search(&[0.0, 0.0], 2.0)
            .into_iter()
            .map(|h| h.index)
            .collect();
        assert_eq!(hits, vec![0, 1, 2]);
    }

    #[test]
    fn linear_lr_end_widens_the_product() {
        assert_eq!(KdTree::linear_lr_end(0, 4, 2), 0);
        assert_eq!(KdTree::linear_lr_end(1, 4, 2), 2);
        assert_eq!(KdTree::linear_lr_end(2, 4, 2), 4);
        // 500_000 * 1_000_000 = 5e11 exceeds u32::MAX: the widened product keeps the
        // linear-lr boundaries correct on a 32-bit target.
        assert_eq!(
            KdTree::linear_lr_end(500_000, 1_000_000, 1_000_000),
            500_000
        );
        // Half-open ends turn an empty leaf into an empty range instead of an
        // underflowed right end (a tree with no records is rejected at load anyway).
        assert_eq!(KdTree::linear_lr_end(0, 0, 1), 0);
    }

    /// `searches_match_brute_force` on a tree built in the test, so the leaf range and
    /// bailout semantics are covered without the index data.
    #[test]
    fn crafted_tree_searches_match_brute_force() {
        for spec in [
            Spec::valid(),
            Spec {
                linear_lr: true,
                ..Spec::valid()
            },
        ] {
            let tree = load_crafted("crafted_search", &spec);
            for q in [[0.0, 0.0], [2.0, 0.0], [2.5, 2.5], [10.0, -3.0]] {
                let fast = tree.nearest(&q).expect("non-empty tree");
                let slow = brute_force_nearest(&tree, &q);
                assert_eq!(fast.index, slow.index, "{q:?} nearest");
                assert!(f64::abs(fast.dist2 - slow.dist2) < 1e-12, "{q:?} dist2");

                for tol in [0.0, 0.5, 2.0, 5.0] {
                    let mut fast: Vec<usize> = tree
                        .ball_search(&q, tol)
                        .into_iter()
                        .map(|h| h.index)
                        .collect();
                    let slow = brute_force_ball(&tree, &q, tol);
                    fast.sort_unstable();
                    assert_eq!(fast, slow, "{q:?} ball search at tol {tol}");
                }
            }
        }
    }

    // ---- The filtered (hint-compact) tree ---------------------------------

    /// A structurally valid source tree over `rows` for [`KdTree::filtered`]: `nbottom`
    /// leaves as uniform `KDT_LINL` ranges and interior nodes carrying the trivial
    /// plane, because the filtered build reads only the data and the quantization,
    /// never the source's own `lr` or splits. `span` is the per-dim `maxval` (with
    /// `minval` 0 and `scale` 1 a stored integer dequantizes to itself, so a test can
    /// put keys anywhere in the element's range).
    fn crafted_source(
        kind: &str,
        ndim: usize,
        rows: &[Vec<u32>],
        nbottom: usize,
        span: f64,
    ) -> KdTree {
        assert!(
            rows.iter().all(|row| row.len() == ndim),
            "crafted source: every row must carry exactly {ndim} coordinates, or the data \
             area is smaller than the header declares and the extra dims silently read \
             padding"
        );
        let elem = if kind == "u16" { 2 } else { 4 };
        let mut w = FitsBuf::default();
        w.block(&[
            card("SIMPLE", "T"),
            card("BITPIX", "8"),
            card("NAXIS", "0"),
            str_card("ENDIAN", "04:03:02:01"),
        ]);
        let mut header = table_cards("header", 0, 0);
        header.extend_from_slice(&[
            str_card("KDT_NAME", TREE),
            card("KDT_NDAT", &rows.len().to_string()),
            card("KDT_NDIM", &ndim.to_string()),
            card("KDT_NNOD", &(2 * nbottom - 1).to_string()),
            card("KDT_VER", "1"),
            str_card("KDT_EXT", "double"),
            str_card("KDT_INT", "u32"),
            str_card("KDT_DATA", kind),
            card("KDT_LINL", "T"),
        ]);
        w.block(&header);
        let ninterior = nbottom - 1;
        if ninterior > 0 {
            // dim 0 (always a legal dimension) at value 0: never consulted.
            let split: Vec<u8> = (0..ninterior).flat_map(|_| u32::to_le_bytes(0)).collect();
            w.table(&table_cards("split", 4, ninterior), &split);
        }
        // Coordinates come out as the stored integers: minval 0, scale 1.
        let range: Vec<u8> = (0..ndim)
            .map(|_| 0.0f64)
            .chain((0..ndim).map(|_| span))
            .chain([1.0f64])
            .flat_map(f64::to_le_bytes)
            .collect();
        w.table(&table_cards("range", 8, range.len() / 8), &range);
        let data: Vec<u8> = rows
            .iter()
            .flatten()
            .copied()
            .flat_map(|v| {
                if elem == 2 {
                    u16::try_from(v).expect("u16 record").to_le_bytes().to_vec()
                } else {
                    v.to_le_bytes().to_vec()
                }
            })
            .collect();
        w.table(&table_cards("data", ndim * elem, rows.len()), &data);
        let fits = Fits::from_bytes(w.0)
            .unwrap_or_else(|e| panic!("crafted {kind} source failed to parse: {e:#}"));
        KdTree::load(&fits, TREE)
            .unwrap_or_else(|e| panic!("crafted {kind} source tree was rejected: {e:#}"))
    }

    /// `base` cut down to `ndim` coordinates per row. Built fresh for each `ndim`:
    /// truncating the same rows in a loop would leave them short for the wider ones.
    fn rows_ndim(base: &[Vec<u32>], ndim: usize) -> Vec<Vec<u32>> {
        base.iter()
            .map(|row| row.iter().copied().take(ndim).collect())
            .collect()
    }

    /// The records of `recs` within `tol` of `q`, as `(record, dist2)` in record
    /// order: the answer a filtered tree over `recs` must give.
    fn ball_over(src: &KdTree, recs: &[u32], q: &[f64], tol: f64) -> Vec<(u32, f64)> {
        let mut hits: Vec<(u32, f64)> = recs
            .iter()
            .filter_map(|&rec| {
                let mut d2 = 0.0;
                for (d, &qv) in q.iter().enumerate().take(src.ndim) {
                    let delta = qv - src.coord(rec as usize, d);
                    d2 += delta * delta;
                }
                (d2 <= tol * tol).then_some((rec, d2))
            })
            .collect();
        hits.sort_unstable_by_key(|&(rec, _)| rec);
        hits
    }

    /// One filtered tree against the linear scan over the same records, queried across
    /// `[0, span]`. Returns the tree's leaf and interior counts so a caller can pin
    /// that the build actually had depth.
    fn assert_filtered_matches(
        kind: &str,
        ndim: usize,
        rows: &[Vec<u32>],
        recs: &[u32],
        span: f64,
    ) -> (usize, usize) {
        let source = crafted_source(kind, ndim, rows, 4, span);
        let (tree, rec_map) = source.filtered(recs).unwrap_or_else(|| {
            panic!(
                "{kind} filtered tree over {} records was refused",
                recs.len()
            )
        });
        assert_eq!(tree.ndata, recs.len(), "filtered ndata");
        assert_eq!(rec_map.len(), recs.len(), "record map length");
        let mut seen = rec_map.clone();
        seen.sort_unstable();
        let mut want = recs.to_vec();
        want.sort_unstable();
        assert_eq!(seen, want, "record map must be a permutation of the subset");

        let mut rng = Lcg(0xf117e5);
        for _ in 0..24 {
            let q: Vec<f64> = (0..ndim)
                .map(|_| rng.next_range(-2.0, span + 3.0))
                .collect();
            for tol in [0.0, 0.5, 1.5, 4.0, 200.0] {
                let mut fast: Vec<(u32, f64)> = tree
                    .ball_search(&q, tol)
                    .into_iter()
                    .map(|h| (rec_map[h.index], h.dist2))
                    .collect();
                fast.sort_unstable_by_key(|&(rec, _)| rec);
                assert_eq!(
                    fast,
                    ball_over(&source, recs, &q, tol),
                    "{kind} {ndim}-D tree over {} records at tol {tol} for {q:?}",
                    recs.len()
                );
            }
        }
        (tree.nbottom, tree.ninterior)
    }

    /// A filtered tree answers every query with exactly the records a linear scan of
    /// the subset finds. The crafted records lean on the cases a median build can get
    /// wrong: values inside one split-value step (so the masked plane cannot land
    /// between them), blocks of identical records (so no dim offers a plane at all),
    /// and a subset smaller than one leaf (so the tree is a lone leaf with no interior
    /// node).
    #[test]
    fn crafted_filtered_tree_matches_a_linear_scan() {
        // 48 records: 16 that vary within a few units per dim, then two blocks of 16
        // that are identical inside each block.
        let base: Vec<Vec<u32>> = (0..16)
            .map(|i| (0..4).map(|d| ((i * 7 + d * 13) % 5) as u32).collect())
            .chain((0..16).map(|_| vec![7u32; 4]))
            .chain((0..16).map(|_| vec![8u32, 7, 7, 7]))
            .collect();
        for ndim in [1, 2, 4] {
            let rows = rows_ndim(&base, ndim);
            for kind in ["u16", "u32"] {
                let all: Vec<u32> = (0..48).collect();
                assert_filtered_matches(kind, ndim, &rows, &all, 63.0);
                // Two identical blocks: the root splits once, then every group is
                // identical, so the deeper nodes have no plane to split on.
                assert_filtered_matches(kind, ndim, &rows, &(16..48).collect::<Vec<u32>>(), 63.0);
                assert_filtered_matches(
                    kind,
                    ndim,
                    &rows,
                    &(0..48).step_by(5).collect::<Vec<u32>>(),
                    63.0,
                );
                // Smaller than one leaf: no interior node at all.
                assert_filtered_matches(kind, ndim, &rows, &[3, 17, 40], 63.0);
                assert_filtered_matches(kind, ndim, &rows, &[16], 63.0);
            }
        }
    }

    /// The same comparison several levels deep, which the 48-record sets above cannot
    /// reach: at `params::HINT_TREE_LEAF` a subset needs a few hundred records before
    /// the tree has more than a root split. Deep builds exercise the per-node dim
    /// choice, the fallback plane below the root, and an `lr` table that tiles many
    /// more leaves than two.
    #[test]
    fn crafted_filtered_tree_deep_subset_matches_a_linear_scan() {
        // 320 records: a spread-out half, then blocks that are identical inside each
        // block, so a deep node is guaranteed to be one of the inseparable ones and to
        // fall back mid-tree rather than at the root.
        let base: Vec<Vec<u32>> = (0..160)
            .map(|i| (0..4).map(|d| ((i * 37 + d * 11) % 64) as u32).collect())
            .chain((0..40).map(|_| vec![9u32; 4]))
            .chain((0..40).map(|_| vec![9u32, 9, 9, 10]))
            .chain((0..40).map(|i| vec![40 + (i % 3) as u32, 5, 5, 5]))
            .chain((0..40).map(|_| vec![63u32; 4]))
            .collect();
        assert_eq!(base.len(), 320);
        for ndim in [2, 4] {
            let rows = rows_ndim(&base, ndim);
            for kind in ["u16", "u32"] {
                let all: Vec<u32> = (0..rows.len() as u32).collect();
                let (nbottom, ninterior) = assert_filtered_matches(kind, ndim, &rows, &all, 64.0);
                assert!(
                    nbottom >= 8 && ninterior >= 7,
                    "{kind} {ndim}-D tree over 320 records is shallow: {nbottom} leaves, \
                     {ninterior} interior nodes - the deep coverage this test exists for is gone"
                );
                // A block whose records are identical in every dim: no node holding
                // them has a plane to split on, all the way down.
                assert_filtered_matches(kind, ndim, &rows, &(200..240).collect::<Vec<u32>>(), 64.0);
                // Inseparable blocks plus separable records interleaved.
                assert_filtered_matches(kind, ndim, &rows, &(150..250).collect::<Vec<u32>>(), 64.0);
                assert_filtered_matches(
                    kind,
                    ndim,
                    &rows,
                    &(0..320).step_by(3).collect::<Vec<u32>>(),
                    64.0,
                );
            }
        }
    }

    /// A node whose records no storable plane can separate still has to hand its
    /// children a plane the searches can prune on: the left half's coordinate must be
    /// at or under it, or a query on the far side of the plane would skip the half
    /// that holds every record. For `KDT_DATA = u32` the records can sit in the top
    /// steps of their range, where the largest storable value is *below* them, so the
    /// build must refuse instead of writing that plane.
    #[test]
    fn crafted_filtered_tree_plane_dominates_its_own_records() {
        let top = u32::MAX as f64;
        // Just inside the corner: `ceil(qmax)` is still representable, so the tree is
        // built and its plane sits at the records' own top coordinate. Queries above
        // the plane - inside and outside the pruning window - must agree with the
        // linear scan, not lose the records to the empty half.
        let near: Vec<Vec<u32>> = (0..40)
            .map(|i| vec![u32::MAX - 8, 5, 5, 5 + (i % 3)])
            .collect();
        let source = crafted_source("u32", 4, &near, 4, top);
        let recs: Vec<u32> = (0..40).collect();
        let (tree, rec_map) = source
            .filtered(&recs)
            .expect("a plane above keys that stop short of the corner exists");
        for q0 in [
            f64::from(u32::MAX) + 40.0,
            f64::from(u32::MAX) + 6.0,
            f64::from(u32::MAX) + 2.0,
            f64::from(u32::MAX) - 3.0,
            f64::from(u32::MAX) - 9.0,
            0.0,
        ] {
            for tol in [0.0, 1.0, 2.0, 4.0, 8.0, 100.0] {
                let q = [q0, 5.0, 5.0, 6.0];
                let mut fast: Vec<(u32, f64)> = tree
                    .ball_search(&q, tol)
                    .into_iter()
                    .map(|h| (rec_map[h.index], h.dist2))
                    .collect();
                fast.sort_unstable_by_key(|&(rec, _)| rec);
                assert_eq!(
                    fast,
                    ball_over(&source, &recs, &q, tol),
                    "u32 keys at {q0:.0} +- {tol} lost or invented a record"
                );
            }
        }
        // Inside the corner: every dim's range is one storage step wide and its top
        // coordinate has no storable value above it, so there is no tree to be had. A
        // plane here would be below the records it dominates, which is the one thing
        // `descend` cannot tolerate.
        let corner: Vec<Vec<u32>> = (0..40).map(|i| vec![u32::MAX - (i % 2), 5, 5, 5]).collect();
        let source = crafted_source("u32", 4, &corner, 4, top);
        assert!(
            source.filtered(&(0..40).collect::<Vec<u32>>()).is_none(),
            "keys in the top steps of the u32 range must refuse a tree, not get a plane \
             below themselves"
        );
    }

    /// The record cap of [`KdTree::filtered`] is a byte budget counted at one row
    /// width, so a wider row must be refused rather than quietly get a bigger tree:
    /// `params::HINT_TREE_MAX_ROW_BYTES` is what keeps a caller with wider rows from
    /// multiplying the anonymous memory of every hinted file.
    #[test]
    fn crafted_filtered_tree_refuses_a_row_over_its_budget() {
        let wide: Vec<Vec<u32>> = (0..40)
            .map(|i| (0..8u32).map(|d| i * 8 + d).collect::<Vec<u32>>())
            .collect();
        let source = crafted_source("u32", 8, &wide, 4, 1024.0);
        assert!(
            source.ndim * source.data_kind.size() > crate::params::HINT_TREE_MAX_ROW_BYTES,
            "the crafted row must be wider than the tree budget"
        );
        assert!(
            source.filtered(&(0..40).collect::<Vec<u32>>()).is_none(),
            "a row of {} bytes got a tree although the budget is {}",
            source.ndim * source.data_kind.size(),
            crate::params::HINT_TREE_MAX_ROW_BYTES
        );
        // The same records at the budgeted row width are still buildable, so the
        // refusal above is the row and not something else about the fixture.
        let budgeted: Vec<Vec<u32>> = wide.iter().map(|row| row[..4].to_vec()).collect();
        let source = crafted_source("u32", 4, &budgeted, 4, 1024.0);
        assert!(
            source.filtered(&(0..40).collect::<Vec<u32>>()).is_some(),
            "a row of exactly the budgeted width was refused"
        );
    }

    /// The filtered codes tree of a real index must return, for a real query, exactly
    /// the records the full tree returns that pass the filter - as multisets, distance
    /// included. A filtered tree has its own shape and its own split planes, so the
    /// pruning it does is only safe if it matches the tree it replaces: that
    /// equivalence holds by test, not by argument. Half the queries are the stored code
    /// of a record of the subset - the distribution the solver queries with, and one
    /// that must return at least that record - half are points drawn from the tree's
    /// range. Three index files spread across the install contribute three tree shapes
    /// and three code densities, and ~1000 queries x 3 radii each is enough that a
    /// prune which only bites on rare geometry still shows up. `ANY_INDEX`, self-skips
    /// without the data.
    #[test]
    fn filtered_codes_tree_returns_the_source_hits_that_pass_the_filter() {
        let Some(files) = testutil::spread_indices(3) else {
            return;
        };
        // Subsets of up to two thousand records: enough that the filtered tree is a
        // real tree of several levels rather than one leaf, shrunk for a smaller file.
        const SUBSET: usize = 2000;
        const NQUERY: usize = 170;
        let mut rng = Lcg(0x9e3_779b);
        let mut compared = 0usize;
        for meta in files {
            let fits = Fits::open(&meta.path).unwrap();
            let tree = KdTree::load(&fits, "codes")
                .unwrap_or_else(|e| panic!("{:?}: codes tree: {e:#}", meta.path));
            let ndata = tree.ndata;
            if ndata < 256 {
                continue;
            }
            let step = usize::max(1, usize::div_ceil(ndata, SUBSET));
            let start = ndata / 4;
            let block = usize::min(ndata - start, usize::min(SUBSET, ndata / 2));
            let subsets: Vec<(String, Vec<u32>)> = vec![
                (
                    format!("every {step}th of {ndata}"),
                    (0..ndata as u32).step_by(step).collect(),
                ),
                (
                    "a contiguous block".to_string(),
                    (start as u32..(start + block) as u32).collect(),
                ),
            ];
            for (what, recs) in subsets {
                let mut in_subset = vec![false; ndata];
                for &r in &recs {
                    in_subset[r as usize] = true;
                }
                let (filter, rec_map) = tree
                    .filtered(&recs)
                    .unwrap_or_else(|| panic!("{what}: filtered tree was refused"));
                for _ in 0..NQUERY {
                    let q: Vec<f64> = if u64::is_multiple_of(rng.next_u64(), 2) {
                        let rec = recs[rng.next_range(0.0, recs.len() as f64) as usize];
                        tree.record(rec as usize)
                    } else {
                        (0..tree.ndim)
                            .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
                            .collect()
                    };
                    for tol in [0.0, crate::params::CODE_TOL, 0.05] {
                        let mut full: Vec<(u32, f64)> = tree
                            .ball_search(&q, tol)
                            .into_iter()
                            .filter(|h| in_subset[h.index])
                            .map(|h| (h.index as u32, h.dist2))
                            .collect();
                        full.sort_unstable_by_key(|&(rec, _)| rec);
                        let mut fast: Vec<(u32, f64)> = filter
                            .ball_search(&q, tol)
                            .into_iter()
                            .map(|h| (rec_map[h.index], h.dist2))
                            .collect();
                        fast.sort_unstable_by_key(|&(rec, _)| rec);
                        assert_eq!(
                            fast.len(),
                            full.len(),
                            "{what} at tol {tol} for {q:?}: {} filtered hits against {} \
                             through the full tree",
                            fast.len(),
                            full.len()
                        );
                        assert_eq!(fast, full, "{what} at tol {tol} for {q:?}");
                        compared += fast.len();
                    }
                }
            }
        }
        assert!(
            compared > 0,
            "no hit to compare: the subsets or the radii match nothing"
        );
    }
}
