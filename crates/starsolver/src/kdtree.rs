//! A kd-tree over the quantized coordinate arrays stored in an index file.
//!
//! The layout the loader reads and the two searches walk:
//! * heap numbering: node 0 is the root, children of `i` are `2i+1` (left) and
//!   `2i+2` (right), and every node `i >= ninterior` is a leaf - the searches
//!   address it as leaf number `node - ninterior`, and there are exactly
//!   `nnodes - ninterior = nbottom` leaves;
//! * `lr[j]` is the *inclusive* right end of the data range owned by leaf `j`
//!   (read as `u32` regardless of `KDT_INT`), so leaf `j` covers
//!   `[j == 0 ? 0 : lr[j-1] + 1, lr[j]]`; `leaf_range` reports that range
//!   half-open, which keeps empty leaves empty;
//! * `split[i]` packs the split value and the split dimension into one integer:
//!   `dim = split & dimmask`, `split value = split & !dimmask`,
//!   `dimmask = 2^ceil(log2(ndim)) - 1`;
//! * the split plane divides the halves: left child owns `value[dim] <= split`,
//!   right child owns `value[dim] >= split`. Both searches prune on that rule
//!   (see `descend` and the near/far choice in `nearest`), so a tree whose
//!   halves were stored the other way round would silently lose hits - the
//!   brute-force comparisons in the tests are the guard;
//! * dequantization is `value = q * invscale + minval[dim]`, `invscale = 1/scale`.
//!
//! Every distance is L2 in the *external* (double) units: the query point and
//! the tolerance are given in that space (the solver queries the codes tree
//! with a code vector and `params::CODE_TOL`) and each component is
//! dequantized before it is subtracted, never compared in quantized units.

use anyhow::{Context, Result, bail, ensure};

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

/// A search result: index of the record in the tree's data array and the squared
/// distance in external units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub index: usize,
    pub dist2: f64,
}

/// A compact kd-tree over quantized records.
///
/// All arrays (`lr`, `split`, `data`) are read lazily from the index file
/// through [`BlobView`]: the tree holds no index-sized anonymous buffer, only
/// byte views into the mmap, so its memory cost is independent of the index
/// size. It stays that way on purpose: dequantizing eagerly would mean
/// 8-byte doubles where the file holds 2- or 4-byte integers, so 4x the `u16`
/// data blob and 2x the `u32` one of anonymous memory, more than the project's
/// peak-RSS budget for index work allows. Coordinates are therefore
/// dequantized on access - `q * invscale + minval[d]`, a couple of ALU ops -
/// while the cache lines a search touches stay a quarter (`u16`) or a half
/// (`u32`) of what an `f64` array would need.
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

/// Upper bound on `KDT_NDIM`: without it a corrupt header would overflow the
/// doubling in `dimmask_for`, which in release (where the multiply wraps to 0)
/// turns into a loop that never ends.
const MAX_NDIM: usize = 64;

/// The mask that isolates the split dimension from a packed `split` entry:
/// `2^ceil(log2(ndim)) - 1`, the low bits needed to number `ndim` dimensions
/// (`3` for `ndim = 4`, `7` for `ndim = 5`). For `ndim = 1` the mask is `0`:
/// the dimension is always `0` and the entry holds the value alone.
fn dimmask_for(ndim: usize) -> Result<u32> {
    ensure!((1..=MAX_NDIM).contains(&ndim), "kd-tree with ndim={ndim}");
    let mut val: u32 = 1;
    while (val as usize) < ndim {
        val *= 2;
    }
    Ok(val - 1)
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
        // A tree is stored as the chunks header, lr, perm, bb, split, splitdim,
        // data and range; this loader reads header, lr, split, range and data.
        // The three rejected here change how the rest must be read: `perm`
        // renumbers the records (a hit's row index would have to be mapped
        // through it before it names a record the rest of the index
        // understands), `bb` carries per-node bounding boxes a traversal can
        // prune with instead of the split planes, and `splitdim` keeps the
        // split dimension in its own array, which frees (and so invalidates)
        // the low bits of `split`. None is decoded, so its presence is an
        // error rather than something to ignore.
        for forbidden in ["bb", "splitdim", "perm"] {
            let table = format!("kdtree_{forbidden}_{name}");
            ensure!(
                fits.find_table(&table).is_none(),
                "unsupported index feature: kd-tree table {table:?}"
            );
        }

        // The sizes must be validated before any arithmetic uses them: a
        // negative card run through `as usize` would become `usize::MAX` (and
        // `as usize` truncates on 32-bit targets), so the conversion here is
        // `usize::try_from`. `ndim == 0` is not a usable dimension either; left
        // alone it would only show up later, as one of the blob-size checks
        // below, so it is rejected at the header stage.
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

        // lr: the entries are u32 no matter what KDT_INT says for the split
        // array. `KDT_LINL` means the leaves are uniformly sized, so the ranges
        // are recomputed from `ndata` and `nbottom` and any `lr` table the file
        // still carries is never looked up - hence the flag is tested first.
        // Otherwise the entries are read lazily, one u32 at a time, as the
        // searches walk the leaves.
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

        // range table: minval[ndim], maxval[ndim], scale (all f64). Small
        // fixed size (ndim <= MAX_NDIM), so it is copied eagerly.
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
        // Non-finite or inverted bounds poison every dequantized coordinate
        // (NaN coords match nothing, so the solver would quietly find nothing),
        // so they must fail here rather than at search time.
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
        // One view over the whole row area: the integer of record `rec`,
        // dimension `d` is addressed as `rec * ndim + d` and dequantized in
        // `coord`, so no record is ever copied out of the mmap.
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
    /// Trees in this format are full binary trees, so `nnodes` is odd. Only
    /// then do the two children of the last interior node `ninterior - 1`
    /// (nodes `2 * ninterior - 1` and `2 * ninterior`) land on the last two
    /// leaves, `nbottom - 2` and `nbottom - 1` (for `nnodes >= 3`). With an
    /// even count they land on `nbottom - 1` and `nbottom`, so a search that
    /// descends into the second one asks for a leaf index that does not exist;
    /// `leaf_range` asserts before it reads anything. The `u32` bound keeps
    /// node ids representable in the `u32` search stacks.
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

    /// Dequantized coordinate: `q * invscale + minval[d]`, decoded from the
    /// quantized blob on demand (the blob stays in the mmap; the struct docs
    /// give the reason there is no eager `f64` copy).
    #[inline]
    #[must_use]
    pub fn coord(&self, rec: usize, d: usize) -> f64 {
        let q = self
            .data
            .uint_at(self.endian, self.data_kind.size(), rec * self.ndim + d);
        f64::from(q) * self.invscale + self.minval[d]
    }

    /// Dequantized record `rec`.
    pub fn record(&self, rec: usize) -> Vec<f64> {
        (0..self.ndim).map(|d| self.coord(rec, d)).collect()
    }

    /// The inclusive right end stored in leaf `leaf`'s `lr` entry.
    /// Panics when `linear_lr` (no table); callers gate on `leaf_range`.
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
            // (j + 1) * ndata / nbottom), the same rule read half-open. The
            // end of one leaf is the start of the next and the final end is
            // exactly `ndata`, so the ranges tile [0, ndata).
            let l = Self::linear_lr_end(leaf, self.ndata, self.nbottom);
            let r = Self::linear_lr_end(leaf + 1, self.ndata, self.nbottom);
            return (l, r);
        }
        let left = if leaf == 0 {
            0
        } else {
            usize::try_from(self.lr_entry(leaf - 1)).expect("u32 fits in usize") + 1
        };
        // The table stores the *inclusive* right end; +1 never overflows a
        // usize (u32 + 1).
        (
            left,
            usize::try_from(self.lr_entry(leaf)).expect("u32 fits in usize") + 1,
        )
    }

    /// The uniform leaf boundary `leaf * ndata / nbottom` (integer division),
    /// with the product taken in `u128`. The widening is the point: on a
    /// 32-bit target `leaf * ndata` passes `u32::MAX` at about a million
    /// records (500_000 * 1_000_000 = 5e11), and a wrapped product would hand
    /// out wrong, possibly overlapping ranges. `nbottom >= 1` because
    /// `nnodes >= 1`, but it is floored at 1 here so the helper cannot divide
    /// by zero on its own.
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

    /// Structural self-check, run on every tree at load: the blob sizes agree
    /// with the header counts, every packed split dimension is inside `ndim`,
    /// `lr` has one entry per leaf, and the leaf ranges tile `[0, ndata)`
    /// without gaps or overlaps - which for a table-driven tree is what makes
    /// `lr` non-decreasing and its last entry `ndata - 1`. A corrupt index
    /// fails here (exit 2) instead of quietly returning wrong hits later.
    pub fn check_structure(&self) -> Result<()> {
        ensure!(
            self.ndata.checked_mul(self.ndim * self.data_kind.size())
                == Some(self.data.payload_bytes())
                && self.data.nrows() == self.ndata,
            "kd-tree {:?}: data blob size mismatch",
            self.name
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

    /// Squared L2 distance from `query` to record `rec` in external units,
    /// bailing out as soon as the running total exceeds `cutoff`: every caller
    /// only needs to know that the record is outside, so the dimensions past
    /// that point are not worth reading.
    ///
    /// A bailed-out record reports `f64::INFINITY`, not a value just above
    /// `cutoff`: from `cutoff >= 4.0` up the ulp is at least 4 * `f64::EPSILON`,
    /// so `cutoff + eps` rounds back to `cutoff` and such a record would pass
    /// the caller's `d2 <= cutoff` test as a hit. A bail can never be mistaken
    /// for a real distance in `nearest` either: while its best is still the
    /// `INFINITY` sentinel the running total cannot exceed the cutoff, so the
    /// exact distance is what comes back.
    fn dist2_cutoff(&self, rec: usize, query: &[f64], cutoff: f64) -> f64 {
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

    /// Pushes the children of an interior node onto `stack`: the near child
    /// first, then the far one when it can still hold a hit, so a pop takes the
    /// far child first. A range search does not care in which order its hits
    /// arrive, so the order is free. The far child is skipped once the
    /// perpendicular gap along the split dimension exceeds `tol`: everything
    /// below it lies on the far side of the plane, so its distance is at least
    /// `|gap|`. A `gap < 0` puts the query below the plane, and the left child
    /// owns `value[dim] <= split`, so there the left child is the near one.
    fn descend(&self, node: usize, query: &[f64], tol: f64, stack: &mut Vec<u32>) {
        let (dim, rsplit) = self.split_external(node);
        let gap = query[dim] - rsplit;
        let (near, far) = if gap < 0.0 {
            (2 * node + 1, 2 * node + 2)
        } else {
            (2 * node + 2, 2 * node + 1)
        };
        stack.push(near as u32);
        if f64::abs(gap) <= tol {
            stack.push(far as u32);
        }
    }

    /// All records within `tol` (L2, external units) of `query`, appended to
    /// `out` in traversal order; use [`KdTree::ball_search`] when they must be
    /// sorted by distance.
    pub fn ball_search_into(&self, query: &[f64], tol: f64, out: &mut Vec<Hit>) {
        assert_eq!(query.len(), self.ndim, "ball search dimension mismatch");
        if self.ndata == 0 || tol < 0.0 {
            return;
        }
        let maxd2 = tol * tol;
        let mut stack: Vec<u32> = Vec::with_capacity(32);
        stack.push(0);
        while let Some(node) = stack.pop() {
            let node = node as usize;
            if node >= self.ninterior {
                let (l, r) = self.leaf_range(node - self.ninterior);
                for rec in l..r {
                    let d2 = self.dist2_cutoff(rec, query, maxd2);
                    if d2 <= maxd2 {
                        out.push(Hit {
                            index: rec,
                            dist2: d2,
                        });
                    }
                }
                continue;
            }
            self.descend(node, query, tol, &mut stack);
        }
    }

    /// All records within `tol` of `query`, sorted by ascending squared
    /// distance. The per-field-code query uses [`KdTree::ball_search_into`] and
    /// sorts the buffer it reuses across queries; this wrapper is what
    /// [`KdTree::self_check`], the reference-star scan in `verify_hit` and the
    /// tests call.
    pub fn ball_search(&self, query: &[f64], tol: f64) -> Vec<Hit> {
        let mut hits = Vec::new();
        self.ball_search_into(query, tol, &mut hits);
        hits.sort_by(|a, b| f64::total_cmp(&a.dist2, &b.dist2));
        hits
    }

    /// Nearest record to `query` (branch and bound). Every stack entry carries
    /// a lower bound on the distance from `query` to anything below it: `gap^2`
    /// for a far child, `0.0` for a near one, and an entry whose bound has
    /// fallen behind the best distance found so far is dropped on pop. The near
    /// child is visited first, the opposite of `descend`'s push order in the
    /// range search.
    pub fn nearest(&self, query: &[f64]) -> Option<Hit> {
        assert_eq!(query.len(), self.ndim, "nearest dimension mismatch");
        if self.ndata == 0 {
            return None;
        }
        let mut best = Hit {
            index: usize::MAX,
            dist2: f64::INFINITY,
        };
        // (node, lower bound on the distance from `query` to anything below it)
        let mut stack: Vec<(u32, f64)> = Vec::with_capacity(32);
        stack.push((0, 0.0));
        while let Some((node, lb)) = stack.pop() {
            if lb > best.dist2 {
                continue;
            }
            let node = node as usize;
            if node >= self.ninterior {
                let (l, r) = self.leaf_range(node - self.ninterior);
                for rec in l..r {
                    let d2 = self.dist2_cutoff(rec, query, best.dist2);
                    // Deterministic tie-break: the format permits duplicate
                    // records (equidistant to every query), so among equal
                    // distances keep the LOWEST index - the same winner the
                    // strict `<` of the brute-force scan in the tests picks.
                    if d2 < best.dist2 || (d2 == best.dist2 && rec < best.index) {
                        best = Hit {
                            index: rec,
                            dist2: d2,
                        };
                    }
                }
                continue;
            }
            let (dim, rsplit) = self.split_external(node);
            let gap = query[dim] - rsplit;
            let (near, far) = if gap < 0.0 {
                (2 * node + 1, 2 * node + 2)
            } else {
                (2 * node + 2, 2 * node + 1)
            };
            let far_d2 = gap * gap;
            if far_d2 <= best.dist2 {
                stack.push((far as u32, far_d2));
            }
            stack.push((near as u32, 0.0));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits::Fits;
    use crate::testutil;

    /// These tests are `ANY_INDEX`: any old-format installed index works, so
    /// they take the smallest installed file, which keeps the brute-force
    /// comparisons fast. They self-skip (via `testutil::smallest_index`) when
    /// no index is installed.
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

    /// Deterministic pseudo-random generator: the same query points every
    /// run, and no rng dependency.
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

    /// Results of `ball_search` are sorted by ascending distance.
    #[test]
    fn ball_search_is_sorted() {
        let Some(tree) = codes_tree() else {
            return;
        };
        let mut rng = Lcg(1);
        let q: Vec<f64> = (0..tree.ndim)
            .map(|d| rng.next_range(tree.minval[d], tree.maxval[d]))
            .collect();
        // Grow the radius until several records fall inside it: the query
        // point is random, so a fixed radius could land far from every record
        // of an arbitrarily small installed tree.
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
    /// (`KDT_INT`): the dimension in the low `dimmask` bits, the split value in
    /// the bits above them. The dimension being inside `ndim` is the property
    /// that can fail on data; a masked value has zero bits inside `dimmask` by
    /// construction, asserted as a guard on the unpacking itself.
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

    /// Writer for the format's chunk layout: a primary header plus one BINTABLE
    /// per chunk, each holding a single `A` (character blob) column.
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
    /// describe a *valid* 2-D, 4-record, 3-node tree, so when a test corrupts a
    /// header card the guard — not a later size mismatch — is what rejects it.
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

    /// `x` is 0, 1 in the left leaf and 2, 3 in the right one, so the single
    /// split plane (dim 0, value 2) agrees with the leaf order and the searches
    /// can be compared against brute force.
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

    /// `load` must reject the crafted tree with a clean `Err` mentioning
    /// `needle` (never a panic or a hang).
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

    /// Big-endian file with `u16` data/split and a padded multi-row `lr`
    /// column: covers the non-flat (div/mod) element packing of
    /// `BlobView::at` and big-endian decoding at all three element reads
    /// (`coord`, `lr_entry`, `split_at`).
    #[test]
    fn crafted_big_endian_u16_tree() {
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

        // lr: 2 rows of 8 bytes, the u32 column occupying the first 4 bytes
        // of each row. The filler column is 0xFF: were the element packing
        // flat instead of per-row, `lr[1]` would read it back as 0xFFFFFFFF.
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
        let tree = KdTree::load(&fits, TREE).expect("crafted BE tree loads");
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

    /// A `KDT_LINL` tree has no lr table and uniform leaf ranges.
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
        // -1 must not survive the conversion to usize and 0 is not a usable
        // dimension: both have to be header-level errors rather than a later
        // blob-size mismatch; the needles below are those guards' own messages.
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

    /// A full binary tree has an odd node count: with an even `KDT_NNOD` the
    /// children of the last interior node step one leaf past `lr` (see
    /// `node_counts`), which would only blow up in `leaf_range` during the
    /// first search, so it is rejected at load.
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

    /// NaN or inverted range bounds dequantize into NaN coordinates, which
    /// match nothing: left alone they make every query miss, and the solver
    /// exits 1 (not solved) on what is really a corrupt-file 2.
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
    /// `cutoff + EPSILON` rounds back to `cutoff`, so a sentinel of that shape
    /// would let a bailed-out record pass the callers' `d2 <= cutoff` test as a
    /// strictly farther "hit".
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
        // 500_000 * 1_000_000 = 5e11 exceeds u32::MAX: the widened product is
        // what keeps the linear-lr boundaries correct on a 32-bit target.
        assert_eq!(
            KdTree::linear_lr_end(500_000, 1_000_000, 1_000_000),
            500_000
        );
        // Half-open ends turn an empty leaf into an empty range instead of an
        // underflowed right end (a tree with no records is rejected at load
        // anyway).
        assert_eq!(KdTree::linear_lr_end(0, 0, 1), 0);
    }

    /// `searches_match_brute_force` on a tree built in the test, so the leaf
    /// range and bailout semantics are covered without the index data.
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
}
