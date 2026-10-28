//! Gnomonic (TAN) projection in both directions, the WCS fit from a matched
//! quad, and the recentring of a fitted WCS onto a new tangent point (the
//! field-center frame the printed solution is reported from).
//!
//! The fit is a Procrustes-like similarity (rotation + uniform scale, no
//! shear); the translation is not free but is *solved as a centroid*: `crval`
//! is the normalized sum of the star `xyz` vectors and `crpix` the centroid
//! of the pixels. `CD = R * scale` with `R` orthogonal - the orthogonal
//! Procrustes factor of the 2x2 star/pixel covariance, taken analytically as
//! the complex similarity `z -> a*z` (rotation, `det CD > 0`) or
//! `z -> a*conj(z)` (reflection, `det CD < 0`); picking the branch IS the
//! parity - and `scale = sqrt(sum|p|^2 / sum|w|^2)` over the centered std
//! coords `p` and the centered pixels `w` (rad/px, stored as deg/px).

use crate::coords::{deg2rad, rad2deg};

/// CD-based TAN WCS (FITS convention): intermediate world coords in degrees,
/// `[xi, eta] = CD * ([x, y] - crpix)`, where `xi` grows with RA and `eta`
/// grows north. Pixel coordinates are 0-based.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wcs {
    /// Reference pixel (0-based): the pixel that maps to `crval_deg`.
    pub crpix: [f64; 2],
    /// Reference sky position, degrees: the tangent point.
    pub crval_deg: [f64; 2],
    /// CD matrix, deg/pixel: row 0 = xi (RA direction), row 1 = eta.
    pub cd: [[f64; 2]; 2],
}

impl Wcs {
    /// Pixel -> `(ra, dec)` in degrees.
    #[must_use]
    pub fn pix_to_radec(&self, x: f64, y: f64) -> (f64, f64) {
        let dx = x - self.crpix[0];
        let dy = y - self.crpix[1];
        let xi = (self.cd[0][0] * dx + self.cd[0][1] * dy).to_radians();
        let eta = (self.cd[1][0] * dx + self.cd[1][1] * dy).to_radians();
        std_to_radec([xi, eta], self.crval_deg)
    }

    /// `(ra, dec)` in degrees -> pixel. Inverts `CD` by dividing through
    /// `cd_determinant()`; a zero-scale (singular-CD) fit is impossible
    /// because [`fit_tan_wcs`] rejects it, so the determinant is never zero.
    #[must_use]
    pub fn radec_to_pix(&self, ra: f64, dec: f64) -> (f64, f64) {
        let [xi, eta] = radec_to_std(ra, dec, self.crval_deg);
        let xi = rad2deg(xi);
        let eta = rad2deg(eta);
        let det = self.cd_determinant();
        let dx = (self.cd[1][1] * xi - self.cd[0][1] * eta) / det;
        let dy = (-self.cd[1][0] * xi + self.cd[0][0] * eta) / det;
        (self.crpix[0] + dx, self.crpix[1] + dy)
    }

    #[must_use]
    pub fn cd_determinant(&self) -> f64 {
        self.cd[0][0] * self.cd[1][1] - self.cd[0][1] * self.cd[1][0]
    }

    /// Image scale, arcsec/pixel: `3600 * sqrt(|det CD|)`. For the similarity
    /// fits this module produces (`CD = R * scale` with `R` orthogonal) that
    /// square root is the fit's uniform scale `scale`, in deg/px before the
    /// 3600.
    #[must_use]
    pub fn arcsec_per_pix(&self) -> f64 {
        3600.0 * f64::sqrt(f64::abs(self.cd_determinant()))
    }

    /// Reported parity: `neg` when `det(CD) > 0`, `pos` otherwise. The stdout
    /// `Field parity:` word keeps the reference solver's wording; the sign
    /// itself is set by the branch [`fit_tan_wcs`] picks - its rotation branch
    /// has `det = +scale^2`, its reflection branch `det = -scale^2`.
    #[must_use]
    pub fn parity_is_neg(&self) -> bool {
        self.cd_determinant() > 0.0
    }

    /// Field rotation angle: the position angle on the sky of the image +y
    /// direction (stdout's "up is X degrees E of N"). Near the tangent point
    /// `dRA*cos(dec) = xi` and `dDec = eta` to first order, so that PA is
    /// `atan2(CD1_2, CD2_2)`; folding the parity sign into the two `atan2`
    /// arguments keeps that answer for both branches of [`fit_tan_wcs`]
    /// (the reflection branch negates the second CD row). The plain
    /// `atan2(CD1_2, CD1_1)` agrees only while `det(CD) >= 0`; with
    /// `det(CD) < 0` it answers 180 degrees minus this angle, so a mirrored
    /// fit would read up to ~180 degrees wrong. The printed summary applies it
    /// to the report WCS (refined, then recentered), not to the raw fit.
    #[must_use]
    pub fn orientation_deg(&self) -> f64 {
        let parity = if self.cd_determinant() >= 0.0 {
            1.0
        } else {
            -1.0
        };
        let t = parity * self.cd[0][0] + self.cd[1][1];
        let a = parity * self.cd[1][0] - self.cd[0][1];
        -f64::atan2(a, t).to_degrees()
    }

    /// Reparameterize this TAN WCS with the tangent point moved to `pixel`:
    /// new `crval = pix_to_radec(pixel)`, new `crpix = pixel`, `CD' = J · CD`,
    /// where `J` is the Jacobian of the reprojection between the two tangent
    /// frames at the new tangent point:
    /// `J_ij = e_i' · (e_j - s0_j·u) / D` with `s0 = CD·(pixel - crpix)` in
    /// radians, `u = (s0_x e_1 + s0_y e_2 + w_0) / D`, `D = sqrt(1 + |s0|^2)`,
    /// `e_1/e_2` the tangent basis at the old tangent point `w_0` and
    /// `e_1'/e_2'` the one at `w_1`. It is the gradient of
    /// `xi' = (u·e_1')/(u·w_1)` there, so it is exact at `pixel` and only
    /// first-order right away from it.
    ///
    /// Why the report needs it: TAN cannot move its tangent point exactly
    /// without extra distortion terms, so the two WCS agree at `pixel` and
    /// drift apart elsewhere - the drift is linear in the tangent-point move
    /// and quadratic in the distance from the new tangent point. Anchoring
    /// every reported solution on the field center is what turns the printed
    /// rotation/scale/center into one shared frame instead of one frame per
    /// quad fit.
    ///
    /// Parity is invariant: `det J = (w_0·w_1)/D^2`, because the 2x2 minor of
    /// the rotation between the two right-handed tangent frames has that
    /// determinant; positive for any two tangent points under 90 degrees
    /// apart. On any non-finite intermediate `*self` is returned: reporting
    /// must never fail a solve.
    #[must_use]
    pub fn recentered_at(&self, pixel: [f64; 2]) -> Wcs {
        let (ra1, dec1) = self.pix_to_radec(pixel[0], pixel[1]);
        let w1 = crate::coords::radec_to_xyz(ra1, dec1);
        let w0 = crate::coords::radec_to_xyz(self.crval_deg[0], self.crval_deg[1]);
        let (e1, e2) = tangent_basis(w0);
        let dx = pixel[0] - self.crpix[0];
        let dy = pixel[1] - self.crpix[1];
        let s0 = [
            (self.cd[0][0] * dx + self.cd[0][1] * dy).to_radians(),
            (self.cd[1][0] * dx + self.cd[1][1] * dy).to_radians(),
        ];
        let mut n = [0.0f64; 3];
        for (c, v) in n.iter_mut().enumerate() {
            *v = s0[0] * e1[c] + s0[1] * e2[c] + w0[c];
        }
        let d = f64::sqrt(n[0] * n[0] + n[1] * n[1] + n[2] * n[2]);
        if !(d.is_finite() && d > 0.0) {
            return *self;
        }
        let u = [n[0] / d, n[1] / d, n[2] / d];
        let (e1p, e2p) = tangent_basis(w1);
        let mut j = [[0.0f64; 2]; 2];
        for (i, ep) in [e1p, e2p].iter().enumerate() {
            for (k, e) in [e1, e2].iter().enumerate() {
                let dud = [
                    (e[0] - s0[k] * u[0]) / d,
                    (e[1] - s0[k] * u[1]) / d,
                    (e[2] - s0[k] * u[2]) / d,
                ];
                j[i][k] = ep[0] * dud[0] + ep[1] * dud[1] + ep[2] * dud[2];
            }
        }
        let cd = [
            [
                j[0][0] * self.cd[0][0] + j[0][1] * self.cd[1][0],
                j[0][0] * self.cd[0][1] + j[0][1] * self.cd[1][1],
            ],
            [
                j[1][0] * self.cd[0][0] + j[1][1] * self.cd[1][0],
                j[1][0] * self.cd[0][1] + j[1][1] * self.cd[1][1],
            ],
        ];
        let ok = f64::is_finite(ra1)
            && f64::is_finite(dec1)
            && w1.iter().all(|v| f64::is_finite(*v))
            && j.iter().flatten().all(|v| f64::is_finite(*v))
            && cd.iter().flatten().all(|v| f64::is_finite(*v));
        if !ok {
            return *self;
        }
        Wcs {
            crpix: pixel,
            crval_deg: [ra1, dec1],
            cd,
        }
    }
}

/// The tangent-plane basis [`xyz_to_std`] uses at the tangent point `v0`
/// (normalized here, so any nonzero scalar multiple gives the same basis):
/// `e1 = (-sin ra, cos ra, 0)`, the increasing-RA direction, and
/// `e2 = v0 x e1`, the northward one (`d v0 / d dec`); both are unit vectors
/// orthogonal to `v0`. At an exact pole the RA direction is undefined
/// (`rho = hypot(x, y) = 0`) and the fixed `[1, 0, 0]` stands in - see
/// [`xyz_to_std`] for what that does to the projected axes.
fn tangent_basis(v0: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let norm = f64::hypot(f64::hypot(v0[0], v0[1]), v0[2]);
    let v0 = [v0[0] / norm, v0[1] / norm, v0[2] / norm];
    let rho = f64::hypot(v0[0], v0[1]);
    let e1 = if rho > f64::EPSILON {
        [-v0[1] / rho, v0[0] / rho, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let e2 = [
        v0[1] * e1[2] - v0[2] * e1[1],
        v0[2] * e1[0] - v0[0] * e1[2],
        v0[0] * e1[1] - v0[1] * e1[0],
    ];
    (e1, e2)
}

/// Field-center pixel of an image `size` pixels wide/tall: `size/2 + 0.5`.
/// Every reported field center uses this anchor: the verbose hit line in
/// `solver`, the recentring in `sweep` and the tests' published-center
/// checks. Evaluating at the naive `size/2` instead shifts the result half a
/// pixel in each axis - a third of the 1.5 px center budget of the solution
/// gate, spent before any solver error is counted - so it must not be
/// substituted.
#[must_use]
pub fn pixel_center_of_size(size: f64) -> f64 {
    0.5 + size / 2.0
}

/// Standard (tangent-plane) coordinates of `(ra, dec)`, radians, relative to
/// `crval_deg` (`xi` along increasing RA, `eta` north):
///
/// ```text
/// dra = ra - ra0
/// den = cos(dec)cos(dec0)cos(dra) + sin(dec)sin(dec0)
/// xi  = cos(dec) sin(dra) / den
/// eta = (cos(dec0) sin(dec) - sin(dec0) cos(dec) cos(dra)) / den
/// ```
///
/// `den` is the dot product of the two unit sky vectors - hence `cos(dra)`
/// on the cos-cos term - the gnomonic depth along the tangent point: the
/// numerators are the projections of the star's unit vector onto the
/// tangent-plane axes, so this is [`xyz_to_std`] with the basis written out
/// in `(ra, dec)`.
///
/// At the exact pole (`dec0 = ±90`) the `ra0` of `crval_deg` still fixes the
/// basis, which makes it a 90-degree rotation of [`xyz_to_std`]'s fixed-pole
/// basis there; each is self-consistent and round-trips with its own inverse
/// (pinned by the tests below).
#[must_use]
pub fn radec_to_std(ra: f64, dec: f64, crval_deg: [f64; 2]) -> [f64; 2] {
    let (ra0, dec0) = (crval_deg[0].to_radians(), crval_deg[1].to_radians());
    let (ra, dec) = (ra.to_radians(), dec.to_radians());
    let dra = ra - ra0;
    let (sin_dec, cos_dec) = (f64::sin(dec), f64::cos(dec));
    let den = cos_dec * f64::cos(dec0) * f64::cos(dra) + sin_dec * f64::sin(dec0);
    let xi = cos_dec * f64::sin(dra) / den;
    let eta = (f64::cos(dec0) * sin_dec - f64::sin(dec0) * cos_dec * f64::cos(dra)) / den;
    [xi, eta]
}

/// Inverse gnomonic: standard coords (radians) -> `(ra, dec)` degrees. The
/// exact scalar inverse of [`radec_to_std`]: both follow the `ra0` of
/// `crval_deg`, so they round-trip even at `dec0 = ±90` (where
/// [`xyz_to_std`]'s fixed-pole basis uses a different, equally consistent
/// frame).
#[must_use]
pub fn std_to_radec(std: [f64; 2], crval_deg: [f64; 2]) -> (f64, f64) {
    let ra0 = crval_deg[0].to_radians();
    let dec0 = crval_deg[1].to_radians();
    let (xi, eta) = (std[0], std[1]);
    // Closed form of "normalize w0 + xi*e1 + eta*e2, read it back as
    // (ra, dec)": r is that vector's length, sqrt(1 + xi^2 + eta^2).
    let r = f64::sqrt(1.0 + xi * xi + eta * eta);
    let (sin_dec0, cos_dec0) = (f64::sin(dec0), f64::cos(dec0));
    let dec = f64::asin((sin_dec0 + eta * cos_dec0) / r);
    let ra = ra0 + f64::atan2(xi, cos_dec0 - eta * sin_dec0);
    let mut ra = rad2deg(ra);
    if ra < 0.0 {
        ra += 360.0;
    }
    (ra % 360.0, rad2deg(dec))
}

/// Procrustes similarity fit of index stars to field pixels.
///
/// `starxyz` are unit-sphere positions, `fieldxy` 0-based pixels; both slices
/// must be the same length of >= 3 (the solver passes one quad's stars - 3 or
/// 4, the index's quad size; `refine::fit_pairs` passes the whole matched-pair
/// set). The returned WCS uses `crval = normalize(sum xyz)` and
/// `crpix = mean(fieldxy)`: those two centroids are the anchors by
/// construction. The mean of the projected stars (`pcm`) only centers the
/// covariance below; it is deliberately not folded back into the reference
/// position, so `crval` stays the star-centroid direction.
#[must_use]
pub fn fit_tan_wcs(starxyz: &[[f64; 3]], fieldxy: &[[f64; 2]]) -> Option<Wcs> {
    let n = starxyz.len();
    // Contract: >= 3 points. A 2-star fit is rank-1, so
    // |z_rot| == |z_ref| and the parity tie-break silently forces "neg".
    if n < 3 || fieldxy.len() != n {
        return None;
    }

    // (a) tangent point: normalized sum of the star vectors.
    let mut cm = [0.0f64; 3];
    for v in starxyz {
        for (c, &x) in cm.iter_mut().zip(v) {
            *c += x;
        }
    }
    let norm = f64::sqrt(cm[0] * cm[0] + cm[1] * cm[1] + cm[2] * cm[2]);
    if norm <= f64::EPSILON {
        return None;
    }
    let star_cm = [cm[0] / norm, cm[1] / norm, cm[2] / norm];
    let (ra0, dec0) = crate::coords::xyz_to_radec(star_cm);
    let crval_deg = [ra0, dec0];

    // (b) standard coords of the stars (radians) and (c) centering of both sets.
    let mut p = Vec::with_capacity(n * 2);
    for v in starxyz {
        let s = xyz_to_std(*v, star_cm);
        p.extend_from_slice(&s);
    }
    let mut field_cm = [0.0; 2];
    for f in fieldxy {
        field_cm[0] += f[0];
        field_cm[1] += f[1];
    }
    field_cm[0] /= n as f64;
    field_cm[1] /= n as f64;
    let mut pcm = [0.0; 2];
    for i in 0..n {
        pcm[0] += p[2 * i];
        pcm[1] += p[2 * i + 1];
    }
    pcm[0] /= n as f64;
    pcm[1] /= n as f64;
    for i in 0..n {
        p[2 * i] -= pcm[0];
        p[2 * i + 1] -= pcm[1];
    }

    // (d,e) complex Procrustes == the orthogonal factor of the 2x2
    // star/pixel covariance SVD, in closed form.
    // z_i = centered std coords, w_i = centered pixels. Two orientation
    // classes, each with its optimal complex coefficient a in closed form:
    //   z ~= a*w        (rotation, det > 0)  -> a = sum z*conj(w) / sum|w|^2
    //   z ~= a*conj(w)  (reflection, det < 0)-> a = sum z*w        / sum|w|^2
    // Each leaves residual sum|z|^2 - |a|^2 * sum|w|^2 over the same sum|w|^2,
    // so the larger |a| is the better-fitting model and wins the branch.
    let mut z_rot = [0.0f64; 2]; // complex (re, im)
    let mut z_ref = [0.0f64; 2];
    let mut pvar = 0.0;
    let mut fvar = 0.0;
    for i in 0..n {
        let (z0, z1) = (p[2 * i], p[2 * i + 1]);
        let w0 = fieldxy[i][0] - field_cm[0];
        let w1 = fieldxy[i][1] - field_cm[1];
        // z * conj(w)
        z_rot[0] += z0 * w0 + z1 * w1;
        z_rot[1] += z1 * w0 - z0 * w1;
        // z * w
        z_ref[0] += z0 * w0 - z1 * w1;
        z_ref[1] += z1 * w0 + z0 * w1;
        pvar += z0 * z0 + z1 * z1;
        fvar += w0 * w0 + w1 * w1;
    }
    if fvar <= 0.0 || pvar <= 0.0 {
        return None;
    }
    let rot_mag = f64::hypot(z_rot[0], z_rot[1]);
    let ref_mag = f64::hypot(z_ref[0], z_ref[1]);
    if rot_mag <= f64::EPSILON && ref_mag <= f64::EPSILON {
        return None;
    }

    // scale = sqrt(pvar / fvar) in rad/px, converted to deg/px.
    let scale_deg = rad2deg(f64::sqrt(pvar / fvar));

    // (f) CD = R * scale, R from the winning branch.
    let cd = if rot_mag >= ref_mag {
        // rotation: a/|a| = (cos + i sin); CD = [[c,-s],[s,c]] * scale, so
        // det = +scale^2 and parity_is_neg() is true for this branch always.
        let (c, s) = (z_rot[0] / rot_mag, z_rot[1] / rot_mag);
        [
            [c * scale_deg, -s * scale_deg],
            [s * scale_deg, c * scale_deg],
        ]
    } else {
        // reflection: a conj(w) with a = |a|(cos + i sin)
        // -> CD = [[c,s],[s,-c]] * scale, det = -scale^2.
        let (c, s) = (z_ref[0] / ref_mag, z_ref[1] / ref_mag);
        [
            [c * scale_deg, s * scale_deg],
            [s * scale_deg, -c * scale_deg],
        ]
    };

    // Defense-in-depth: every guard above fails open on NaN, so a poisoned fit
    // (e.g. NaN in a corrupt index star's xyz) can reach here with a non-finite
    // CD. Reject it rather than return a NaN WCS.
    if !cd.iter().flatten().all(|v| f64::is_finite(*v)) {
        return None;
    }

    Some(Wcs {
        crpix: field_cm,
        crval_deg,
        cd,
    })
}

/// Standard coords (radians) of a unit-sphere vector relative to the unit
/// tangent point `v0` — the vector form of [`radec_to_std`]:
/// `xi = (v·e1)/(v·v0)`, `eta = (v·e2)/(v·v0)` on the `tangent_basis` at `v0`.
///
/// Callers must keep inputs within one hemisphere: a vector with `v·v0 <= 0`
/// has no TAN projection (at `v·v0 = 0` the divide blows up, below it the
/// signs flip into the far hemisphere) and nothing here rejects one. The only
/// production caller projects one fit's stars about their own centroid (a
/// quad's stars, or a refine step's matched pairs), so `v·v0` stays near 1.
///
/// Exact-pole exception: at `v0 = (0, 0, ±1)` the increasing-RA direction is
/// undefined (`rho = hypot(x, y) = 0`), so a fixed basis `[1, 0, 0]` is used,
/// giving `xi = ±x/z`, `eta = y/z`. That is a 90-degree rotation of
/// [`radec_to_std`]'s `ra0`-based basis at the same pole; both bases are
/// internally consistent and round-trip with their own inverse.
#[must_use]
pub fn xyz_to_std(v: [f64; 3], v0: [f64; 3]) -> [f64; 2] {
    let norm = f64::hypot(f64::hypot(v0[0], v0[1]), v0[2]);
    let v0 = [v0[0] / norm, v0[1] / norm, v0[2] / norm];
    // e1/e2: the tangent basis at the normalized v0 (increasing-RA and
    // north; the pole exception lives in `tangent_basis`).
    let (e1, e2) = tangent_basis(v0);
    let den = v[0] * v0[0] + v[1] * v0[1] + v[2] * v0[2];
    let xi = (v[0] * e1[0] + v[1] * e1[1] + v[2] * e1[2]) / den;
    let eta = (v[0] * e2[0] + v[1] * e2[1] + v[2] * e2[2]) / den;
    [xi, eta]
}

/// An `xyz -> pixel` map precomputed once per WCS: the gnomonic projection of
/// a unit-sphere vector onto this WCS's pixels by dot products, with no
/// per-star trigonometry.
///
/// The tangent frame is the one [`radec_to_std`] uses - `ra0`/`dec0` of
/// `crval_deg` fix `e1`/`e2`/`w0` there, so this map agrees with
/// `radec_to_pix(xyz_to_radec(v))` everywhere, including at an exact pole
/// (unlike [`xyz_to_std`]'s vector basis, which rotates 90 degrees there).
/// The algebra differs (expanded dot products instead of nested trig), so the
/// two paths agree to rounding, not bit for bit. Like every TAN map it is only
/// meaningful within the hemisphere around `crval_deg`: a vector with
/// `v . w0 <= 0` has no projection and nothing here rejects one (the
/// `verify_hit` caller discards the resulting wild pixel coordinates with its
/// in-frame test, as before).
pub struct XyzToPix {
    /// `rad2deg / det(CD) * CD^-1 * [e1; e2]` written out: one row per axis.
    rows: [[f64; 3]; 2],
    /// Gnomonic depth: the dot product `v . w0` dividing both rows.
    w0: [f64; 3],
    crpix: [f64; 2],
}

impl XyzToPix {
    /// Project a unit-sphere vector to pixel coordinates.
    #[inline]
    #[must_use]
    pub fn project(&self, v: [f64; 3]) -> (f64, f64) {
        let den = self.w0[0] * v[0] + self.w0[1] * v[1] + self.w0[2] * v[2];
        let r0 = &self.rows[0];
        let r1 = &self.rows[1];
        (
            (r0[0] * v[0] + r0[1] * v[1] + r0[2] * v[2]) / den + self.crpix[0],
            (r1[0] * v[0] + r1[1] * v[1] + r1[2] * v[2]) / den + self.crpix[1],
        )
    }
}

impl Wcs {
    /// Build the [`XyzToPix`] map of this WCS. The four `sin`/`cos` of
    /// `crval_deg` and the CD inversion are paid once per call site instead of
    /// once per projected star.
    #[must_use]
    pub fn xyz_to_pix_map(&self) -> XyzToPix {
        let (ra0, dec0) = (
            self.crval_deg[0].to_radians(),
            self.crval_deg[1].to_radians(),
        );
        let (sin_ra0, cos_ra0) = (f64::sin(ra0), f64::cos(ra0));
        let (sin_dec0, cos_dec0) = (f64::sin(dec0), f64::cos(dec0));
        // The (ra0, dec0) tangent frame of `radec_to_std`: e1 along increasing
        // RA, e2 north, w0 the tangent point itself.
        let e1 = [-sin_ra0, cos_ra0, 0.0];
        let e2 = [-sin_dec0 * cos_ra0, -sin_dec0 * sin_ra0, cos_dec0];
        let w0 = [cos_dec0 * cos_ra0, cos_dec0 * sin_ra0, sin_dec0];
        // `radec_to_pix` reads dx = (CD11*xi - CD01*eta)/det, dy =
        // (-CD10*xi + CD00*eta)/det with xi/eta in degrees; fold the CD
        // inverse and the radian->degree scale into the two dot-product rows.
        let k = rad2deg(1.0) / self.cd_determinant();
        let rows = [
            std::array::from_fn(|d| k * (self.cd[1][1] * e1[d] - self.cd[0][1] * e2[d])),
            std::array::from_fn(|d| k * (-self.cd[1][0] * e1[d] + self.cd[0][0] * e2[d])),
        ];
        XyzToPix {
            rows,
            w0,
            crpix: self.crpix,
        }
    }

    /// Build the [`PixToXyz`] map of this WCS: the same tangent frame as
    /// [`Wcs::xyz_to_pix_map`], read in the other direction.
    #[must_use]
    pub fn pix_to_xyz_map(&self) -> PixToXyz {
        let (ra0, dec0) = (
            self.crval_deg[0].to_radians(),
            self.crval_deg[1].to_radians(),
        );
        let (sin_ra0, cos_ra0) = (f64::sin(ra0), f64::cos(ra0));
        let (sin_dec0, cos_dec0) = (f64::sin(dec0), f64::cos(dec0));
        let e1 = [-sin_ra0, cos_ra0, 0.0];
        let e2 = [-sin_dec0 * cos_ra0, -sin_dec0 * sin_ra0, cos_dec0];
        let w0 = [cos_dec0 * cos_ra0, cos_dec0 * sin_ra0, sin_dec0];
        // `std_to_radec` takes std = CD * (pixel - crpix) in radians, so the
        // per-pixel work collapses onto two sky-direction columns.
        let k = deg2rad(1.0);
        let cols = [
            std::array::from_fn(|d| k * (self.cd[0][0] * e1[d] + self.cd[1][0] * e2[d])),
            std::array::from_fn(|d| k * (self.cd[0][1] * e1[d] + self.cd[1][1] * e2[d])),
        ];
        PixToXyz {
            cols,
            w0,
            crpix: self.crpix,
        }
    }
}

/// A `pixel -> unit-sphere vector` map precomputed once per WCS: the TAN
/// deprojection written as `u = normalize(w0 + dx*col_x + dy*col_y)`, which is
/// `radec_to_xyz(pix_to_radec(x, y))` with the tangent basis and the four
/// `sin`/`cos` of `crval_deg` pulled out of the per-pixel path.
///
/// Per pixel it costs two multiplies per axis plus one normalization, against
/// the `atan2`/`asin`/`sin`/`cos` round trip of the two-function path; the two
/// agree to rounding, not bit for bit (see the note on [`XyzToPix`]).
///
/// Unlike the `xyz -> pixel` direction this one has no excluded hemisphere:
/// every pixel deprojects to `w0 + dx*col_x + dy*col_y`, whose dot product with
/// `w0` is always positive, so no pixel is degenerate.
pub struct PixToXyz {
    /// `deg2rad * CD * [e1; e2]`: the sky direction of a step along each axis.
    cols: [[f64; 3]; 2],
    /// The tangent point: the vector a pixel at `crpix` deprojects to.
    w0: [f64; 3],
    crpix: [f64; 2],
}

impl PixToXyz {
    /// Deproject a pixel to a unit-sphere vector.
    #[inline]
    #[must_use]
    pub fn project(&self, x: f64, y: f64) -> [f64; 3] {
        let dx = x - self.crpix[0];
        let dy = y - self.crpix[1];
        let mut v = [0.0f64; 3];
        for (c, out) in v.iter_mut().enumerate() {
            *out = self.w0[c] + dx * self.cols[0][c] + dy * self.cols[1][c];
        }
        let n = f64::sqrt(v[0] * v[0] + v[1] * v[1] + v[2] * v[2]);
        // A real pixel always has `n >= 1`; anything else arrives only with a
        // non-finite coordinate, and returning `v` keeps that NaN visible to the
        // caller (the probe tests its components) instead of hiding it behind a
        // division.
        if n > 0.0 && n.is_finite() {
            [v[0] / n, v[1] / n, v[2] / n]
        } else {
            v
        }
    }
}

#[cfg(test)]
pub(crate) mod test_data {
    use super::Wcs;
    use crate::testutil;

    /// Field `4656x3522-1`'s published WCS: the CRVAL/CRPIX/CD its reference run
    /// left behind (recorded in `tests/ground_truth.rs`).
    pub const FIELD1_WCS: Wcs = Wcs {
        crpix: [2651.20111448, 2327.06991884],
        crval_deg: [59.7194055429, 35.9547818436],
        cd: [
            [-0.000753336628101, -1.38160452186e-05],
            [1.38160452186e-05, -0.000753336628101],
        ],
    };
    /// Field 1's published center/scale, shorthands for the tests; the single
    /// source is `testutil::FIELD1_TRUTH`.
    pub const FIELD1_CENTER: (f64, f64) = (
        testutil::FIELD1_TRUTH.ra_deg,
        testutil::FIELD1_TRUTH.dec_deg,
    );
    pub const FIELD1_SCALE_ARCSEC: f64 = testutil::FIELD1_TRUTH.scale_arcsec;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::radec_to_xyz;
    use crate::solver::QuadHit;
    use crate::testutil;
    use test_data::*;

    #[test]
    fn std_roundtrip() {
        let crval = [60.0, 36.0];
        for (ra, dec) in [(60.0, 36.0), (59.5, 36.4), (60.9, 35.2), (61.2, 37.1)] {
            let s = radec_to_std(ra, dec, crval);
            let (ra2, dec2) = std_to_radec(s, crval);
            assert!(f64::abs(ra2 - ra) < 1e-9, "{ra} -> {ra2}");
            assert!(f64::abs(dec2 - dec) < 1e-9);
        }
    }

    /// Off-pole, `xyz_to_std` and `radec_to_std` agree to round-off. At the
    /// exact pole their tangent-plane bases differ by a 90-degree rotation
    /// (each self-consistent); pin that so a future basis edit is caught.
    #[test]
    fn xyz_std_matches_radec_std() {
        let crval = [60.0, 36.0];
        for (ra, dec) in [(59.5, 36.4), (60.9, 35.2)] {
            let a = radec_to_std(ra, dec, crval);
            let b = xyz_to_std(radec_to_xyz(ra, dec), radec_to_xyz(crval[0], crval[1]));
            assert!(f64::abs(a[0] - b[0]) < 1e-12);
            assert!(f64::abs(a[1] - b[1]) < 1e-12);
        }

        // Exact-pole: `xyz_to_std` uses the fixed pole basis
        // (north: xi=x/z, eta=y/z; south: xi=-x/z, eta=y/z); `radec_to_std`
        // follows the ra0 it is given, a 90-degree rotation of that basis.
        // Both round-trip with their own inverse.
        for (ra, dec) in [(30.0, 80.0), (120.0, 45.0)] {
            let v = radec_to_xyz(ra, dec);
            let b = xyz_to_std(v, radec_to_xyz(0.0, 90.0));
            assert!(f64::abs(b[0] - v[0] / v[2]) < 1e-9, "N pole {b:?}");
            assert!(f64::abs(b[1] - v[1] / v[2]) < 1e-9, "N pole {b:?}");
            let a = radec_to_std(ra, dec, [0.0, 90.0]);
            assert!(f64::abs(a[0] - b[1]) < 1e-9, "N rot90 {a:?} {b:?}");
            assert!(f64::abs(a[1] + b[0]) < 1e-9, "N rot90 {a:?} {b:?}");
            let (ra2, dec2) = std_to_radec(a, [0.0, 90.0]);
            assert!(f64::abs(ra2 - ra) < 1e-9 && f64::abs(dec2 - dec) < 1e-9);
        }
        for (ra, dec) in [(30.0, -80.0), (120.0, -45.0)] {
            let v = radec_to_xyz(ra, dec);
            let b = xyz_to_std(v, radec_to_xyz(0.0, -90.0));
            assert!(f64::abs(b[0] + v[0] / v[2]) < 1e-9, "S pole {b:?}");
            assert!(f64::abs(b[1] - v[1] / v[2]) < 1e-9, "S pole {b:?}");
            let a = radec_to_std(ra, dec, [0.0, -90.0]);
            assert!(f64::abs(a[0] + b[1]) < 1e-9, "S rot90 {a:?} {b:?}");
            assert!(f64::abs(a[1] - b[0]) < 1e-9, "S rot90 {a:?} {b:?}");
            let (ra2, dec2) = std_to_radec(a, [0.0, -90.0]);
            assert!(f64::abs(ra2 - ra) < 1e-9 && f64::abs(dec2 - dec) < 1e-9);
        }
    }

    #[test]
    fn wcs_pix_roundtrip() {
        let w = FIELD1_WCS;
        for (x, y) in [
            (0.0, 0.0),
            (4656.0, 3522.0),
            (2328.0, 1761.0),
            (100.0, 3000.0),
        ] {
            let (ra, dec) = w.pix_to_radec(x, y);
            let (x2, y2) = w.radec_to_pix(ra, dec);
            assert!(f64::abs(x2 - x) < 1e-7, "({x},{y}) -> ({x2},{y2})");
            assert!(f64::abs(y2 - y) < 1e-7);
        }
    }

    /// XyzToPix projects exactly where `radec_to_pix(xyz_to_radec(..))` does,
    /// to rounding: the dot-product map and the trig path must stay
    /// interchangeable for `verify_hit`'s reference-star loop, including for a
    /// tangent point at an exact pole (the `ra0`-fixed basis of
    /// `radec_to_std`, not the vector basis of `xyz_to_std`).
    #[test]
    fn xyz_to_pix_map_matches_radec_to_pix() {
        for w in [
            FIELD1_WCS,
            // A mirrored wide-field WCS and a pole tangent point, alongside
            // the shipped-field one.
            Wcs {
                crpix: [1000.0, 800.0],
                crval_deg: [310.0, -66.5],
                cd: [[8e-4, 3e-5], [3e-5, -8e-4]],
            },
            Wcs {
                crpix: [400.0, 300.0],
                crval_deg: [42.0, 90.0],
                cd: [[-4e-4, 0.0], [0.0, 4e-4]],
            },
        ] {
            let m = w.xyz_to_pix_map();
            for (x, y) in [
                (0.0, 0.0),
                (800.0, 600.0),
                (400.5, 300.5),
                (1234.0, 45.0),
                (777.0, 1500.0),
            ] {
                let (ra, dec) = w.pix_to_radec(x, y);
                let (x0, y0) = w.radec_to_pix(ra, dec);
                let (x1, y1) = m.project(radec_to_xyz(ra, dec));
                assert!(f64::abs(x1 - x0) < 1e-9, "({x},{y}): {x0} vs {x1}");
                assert!(f64::abs(y1 - y0) < 1e-9, "({x},{y}): {y0} vs {y1}");
            }
        }
    }

    /// The `pixel -> unit-sphere` deprojection the probe's whole verdict rests
    /// on must stay interchangeable with `radec_to_xyz(pix_to_radec(x, y))`:
    /// the same tangent frame, agreeing to rounding, for a mirrored wide field
    /// and a tangent point at an exact pole alongside the shipped-field WCS.
    #[test]
    fn pix_to_xyz_map_matches_pix_to_radec() {
        for w in [
            FIELD1_WCS,
            // A mirrored wide-field WCS and a pole tangent point, the same leg
            // pair as `xyz_to_pix_map_matches_radec_to_pix`.
            Wcs {
                crpix: [1000.0, 800.0],
                crval_deg: [310.0, -66.5],
                cd: [[8e-4, 3e-5], [3e-5, -8e-4]],
            },
            Wcs {
                crpix: [400.0, 300.0],
                crval_deg: [42.0, 90.0],
                cd: [[-4e-4, 0.0], [0.0, 4e-4]],
            },
        ] {
            let m = w.pix_to_xyz_map();
            for (x, y) in [
                (0.0, 0.0),
                (800.0, 600.0),
                (400.5, 300.5),
                (1234.0, 45.0),
                (777.0, 1500.0),
            ] {
                let (ra, dec) = w.pix_to_radec(x, y);
                let want = radec_to_xyz(ra, dec);
                let got = m.project(x, y);
                for d in 0..3 {
                    assert!(
                        f64::abs(got[d] - want[d]) < 1e-12,
                        "({x},{y}) axis {d}: {want:?} vs {got:?}"
                    );
                }
            }
        }
    }

    /// The reference WCS of field 1 reproduces its published center, scale,
    /// orientation and parity.
    ///
    /// NB: the published center is the sky position at the field-center pixel
    /// `W/2 + 0.5, H/2 + 0.5` ([`pixel_center_of_size`]), in the same 0-based
    /// pixel system [`Wcs::crpix`] uses. Evaluating at `(W/2, H/2)` instead
    /// moves it half a pixel per axis (~1.4 arcsec at this field's scale),
    /// four times the 1e-4 deg checked here.
    #[test]
    fn reference_wcs_derived_fields() {
        let w = FIELD1_WCS;
        let (ra, dec) = w.pix_to_radec(pixel_center_of_size(4656.0), pixel_center_of_size(3522.0));
        assert!(f64::abs(ra - FIELD1_CENTER.0) < 1e-4, "ra={ra}");
        assert!(f64::abs(dec - FIELD1_CENTER.1) < 1e-4, "dec={dec}");
        assert!(f64::abs(w.arcsec_per_pix() - FIELD1_SCALE_ARCSEC) < 1e-4);
        assert!((w.orientation_deg() - (-178.9493244882495)).abs() < 1e-6);
        assert!(w.parity_is_neg()); // det(CD) > 0 -> "neg"
    }

    /// Gate: fitting the stars of in-field quads of the index that solves
    /// field 1 recovers the reference WCS: its scale to within 2.7125 +/- 0.01
    /// arcsec/px, a field-center position within ~1 deg of the published
    /// center, and a CD matrix that matches the reference WCS where the fit
    /// lives.
    ///
    /// Quads are selected by a predicate, not by id: stars that all project
    /// inside the field frame through `FIELD1_WCS`, with a cheap dot guard
    /// (within 4 deg of the published center) first so the projection never
    /// sees a far-hemisphere star, plus a triangle-shape guard (see below).
    /// Pixels come from `FIELD1_WCS` itself, so the fit inverts the projection.
    ///
    /// CD is tangent-point dependent under TAN, so it is compared against the
    /// reference WCS recentred at the quad's own pixel centroid (the analytic
    /// [`Wcs::recentered_at`] Jacobian, pinned by its own tests); comparing
    /// against the raw reference CD instead would add a reprojection term that
    /// grows with the distance to the reference tangent point - up to ~2e-5
    /// deg/px at the corners of this frame, well over the 1e-6 asserted here.
    #[test]
    fn fit_from_in_field_quads_matches_reference() {
        let Some(run) = testutil::field_run("4656x3522-1.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let index = testutil::load(run.meta);
        let d = index.meta.dimquads;
        let center = crate::coords::radec_to_xyz(FIELD1_CENTER.0, FIELD1_CENTER.1);
        let cos_guard = f64::cos(4.0f64.to_radians());
        const MAX_QUADS: usize = 50;

        let mut fitted = 0usize;
        for q in 0..index.meta.nquads {
            if fitted >= MAX_QUADS {
                break;
            }
            let quad = index.quad(q);
            let mut starxyz = [[0.0f64; 3]; 4];
            let mut fieldxy = [[0.0f64; 2]; 4];
            let mut inside = true;
            for j in 0..d {
                let xyz = index.star_xyz(quad[j] as usize);
                let dot = xyz[0] * center[0] + xyz[1] * center[1] + xyz[2] * center[2];
                if dot <= cos_guard {
                    inside = false;
                    break;
                }
                let (ra, dec) = crate::coords::xyz_to_radec(xyz);
                let (x, y) = FIELD1_WCS.radec_to_pix(ra, dec);
                if !(0.0..=run.width).contains(&x) || !(0.0..=run.height).contains(&y) {
                    inside = false;
                    break;
                }
                starxyz[j] = xyz;
                fieldxy[j] = [x, y];
            }
            if !inside {
                continue;
            }
            // Keep the third star off the A-B line. At exact collinearity the
            // centered configuration is rank-1: both orientation classes fit
            // it equally well (|z_rot| == |z_ref|), so the branch tie-break -
            // not the data - would decide the parity, and only the backbone
            // direction constrains the fit. Require min altitude >= 5% of the
            // longest side, which for twice the triangle area means
            // area2 >= 0.05 * longest^2.
            let (ax, ay) = (fieldxy[0][0], fieldxy[0][1]);
            let (bx, by) = (fieldxy[1][0], fieldxy[1][1]);
            let (cx, cy) = (fieldxy[2][0], fieldxy[2][1]);
            let longest = f64::max(
                f64::hypot(bx - ax, by - ay),
                f64::max(f64::hypot(cx - ax, cy - ay), f64::hypot(cx - bx, cy - by)),
            );
            let area2 = f64::abs((bx - ax) * (cy - ay) - (by - ay) * (cx - ax));
            if area2 < 0.05 * longest * longest {
                continue;
            }
            fitted += 1;
            let wcs = fit_tan_wcs(&starxyz[..d], &fieldxy[..d]).expect("fit failed");
            let scale = wcs.arcsec_per_pix();
            assert!(
                f64::abs(scale - 2.7125) <= 0.01,
                "quad {q}: scale {scale} not within 2.7125 +/- 0.01"
            );
            let (ra, dec) = wcs.pix_to_radec(
                pixel_center_of_size(run.width),
                pixel_center_of_size(run.height),
            );
            let dr = f64::abs(ra - FIELD1_CENTER.0).min(360.0 - f64::abs(ra - FIELD1_CENTER.0));
            assert!(
                dr < 1.0 && f64::abs(dec - FIELD1_CENTER.1) < 1.0,
                "quad {q}: center {ra},{dec}"
            );

            // The fit's CD, compared at the quad's own tangent point (see
            // the doc comment).
            let cenx = fieldxy[..d].iter().map(|p| p[0]).sum::<f64>() / d as f64;
            let ceny = fieldxy[..d].iter().map(|p| p[1]).sum::<f64>() / d as f64;
            let refw = FIELD1_WCS.recentered_at([cenx, ceny]);
            for (r, g) in wcs.cd.iter().zip(refw.cd) {
                for (a, b) in r.iter().zip(g) {
                    assert!(f64::abs(a - b) < 1e-6, "quad {q}: cd {a} vs {b}");
                }
            }
            assert_eq!(wcs.parity_is_neg(), FIELD1_WCS.parity_is_neg());
        }
        assert!(fitted > 0, "no in-field quad exercised the fit");
    }

    /// Synthetic round trip for both parities and both star counts: a WCS
    /// projected to pixels must be recovered by the fit.
    #[test]
    fn fit_recovers_synthetic_similarity() {
        // A field-like WCS and 4 field positions near its center.
        let base = FIELD1_WCS;
        let offs = [
            [-400.0, -300.0],
            [900.0, -120.0],
            [150.0, 700.0],
            [-60.0, 55.0],
        ];
        for &scale_dir in &[1.0f64, -1.0] {
            // scale_dir = +1 keeps CD (det > 0, "neg"); -1 flips one axis (det < 0).
            let mut cd = base.cd;
            if scale_dir < 0.0 {
                cd[1][0] = -cd[1][0];
                cd[1][1] = -cd[1][1];
            }
            let w = Wcs {
                crpix: [2328.0, 1761.0],
                crval_deg: base.crval_deg,
                cd,
            };
            for n in 3..=4 {
                let fieldxy: Vec<[f64; 2]> = offs[..n]
                    .iter()
                    .map(|[dx, dy]| [w.crpix[0] + dx, w.crpix[1] + dy])
                    .collect();
                let starxyz: Vec<[f64; 3]> = fieldxy
                    .iter()
                    .map(|[x, y]| {
                        let (ra, dec) = w.pix_to_radec(*x, *y);
                        radec_to_xyz(ra, dec)
                    })
                    .collect();
                let fit = fit_tan_wcs(&starxyz, &fieldxy).expect("fit failed");
                assert_eq!(fit.parity_is_neg(), w.parity_is_neg());
                assert!(
                    // TAN is not an exact similarity, so a 3-4 star fit can
                    // rotate slightly relative to the base CD.
                    f64::abs(fit.orientation_deg() - w.orientation_deg()) < 0.3,
                    "orient {} vs {}",
                    fit.orientation_deg(),
                    w.orientation_deg()
                );
                assert!(
                    f64::abs(fit.arcsec_per_pix() - w.arcsec_per_pix()) < 1e-3 * w.arcsec_per_pix(),
                    "scale {} vs {}",
                    fit.arcsec_per_pix(),
                    w.arcsec_per_pix()
                );
                // All input stars must land back within 0.05 px: TAN is not an
                // exact similarity, but the mismatch grows as the cube of the
                // star set's spread (8x per doubling), so over these
                // +-1000 px offsets it only reaches ~0.01 px.
                for (i, [x, y]) in fieldxy.iter().enumerate() {
                    let (ra, dec) = crate::coords::xyz_to_radec(starxyz[i]);
                    let (x2, y2) = fit.radec_to_pix(ra, dec);
                    assert!(
                        f64::hypot(x2 - x, y2 - y) < 0.05,
                        "star {i}: ({x2},{y2}) vs ({x},{y})"
                    );
                }
            }
        }
    }

    /// `orientation_deg` is the position angle (E of N) of the image "up"
    /// (+y) direction for both parities; the naive `atan2(CD1_2, CD1_1)`
    /// returns 180 degrees minus that angle once `det(CD) < 0`.
    #[test]
    fn orientation_is_up_position_angle_for_both_parities() {
        for flip in [1.0f64, -1.0] {
            let mut cd = FIELD1_WCS.cd;
            if flip < 0.0 {
                cd[1][0] = -cd[1][0];
                cd[1][1] = -cd[1][1];
            }
            let w = Wcs {
                crpix: FIELD1_WCS.crpix,
                crval_deg: FIELD1_WCS.crval_deg,
                cd,
            };
            // Independent measurement at the tangent point, where the CD
            // orientation is defined: PA of "one pixel up" on the sphere.
            let (ra0, dec0) = w.pix_to_radec(w.crpix[0], w.crpix[1]);
            let (ra1, dec1) = w.pix_to_radec(w.crpix[0], w.crpix[1] + 1.0);
            let pa = f64::atan2(
                f64::to_radians(ra1 - ra0) * f64::cos(f64::to_radians(dec0)),
                f64::to_radians(dec1 - dec0),
            )
            .to_degrees();
            let diff = (w.orientation_deg() - pa + 180.0).rem_euclid(360.0) - 180.0;
            assert!(
                f64::abs(diff) < 0.01,
                "parity_neg={} orient={} measured={}",
                w.parity_is_neg(),
                w.orientation_deg(),
                pa
            );
        }
    }

    /// Well-conditioned 3-star fit inputs near the tangent point (shared by the
    /// guard tests).
    fn guard_fit_inputs() -> (Vec<[f64; 3]>, Vec<[f64; 2]>) {
        let base = FIELD1_WCS;
        let offs = [[-400.0, -300.0], [900.0, -120.0], [150.0, 700.0]];
        let fieldxy: Vec<[f64; 2]> = offs
            .iter()
            .map(|[dx, dy]| [base.crpix[0] + dx, base.crpix[1] + dy])
            .collect();
        let starxyz: Vec<[f64; 3]> = fieldxy
            .iter()
            .map(|[x, y]| {
                let (ra, dec) = base.pix_to_radec(*x, *y);
                radec_to_xyz(ra, dec)
            })
            .collect();
        (starxyz, fieldxy)
    }

    /// A fit needs >= 3 points (the `dimquads=3/4` contract); fewer return
    /// `None` rather than a rank-1 fit with a silently forced "neg" parity.
    #[test]
    fn fit_rejects_fewer_than_three_points() {
        let (starxyz, fieldxy) = guard_fit_inputs();
        assert!(fit_tan_wcs(&starxyz, &fieldxy).is_some());
        for n in 0..3 {
            assert!(
                fit_tan_wcs(&starxyz[..n], &fieldxy[..n]).is_none(),
                "fit with {n} points must be rejected"
            );
        }
    }

    /// Defense-in-depth: NaN in a reference star's xyz or a field pixel yields
    /// `None`, not a `Some(Wcs)` carrying a non-finite CD.
    #[test]
    fn fit_rejects_nan_poisoned_inputs() {
        let (starxyz, fieldxy) = guard_fit_inputs();
        assert!(fit_tan_wcs(&starxyz, &fieldxy).is_some());

        // NaN in a reference star's xyz (the corrupt-index path).
        let mut bad_star = starxyz.clone();
        bad_star[1] = [f64::NAN, 0.5, 0.5];
        assert!(fit_tan_wcs(&bad_star, &fieldxy).is_none());

        // NaN in a field pixel.
        let mut bad_field = fieldxy.clone();
        bad_field[2] = [f64::NAN, 800.0];
        assert!(fit_tan_wcs(&starxyz, &bad_field).is_none());
    }

    // ---- `Wcs::recentered_at` ------------------------------------------------

    /// The recentred WCS maps the recentring pixel to the new `crval` exactly
    /// (it becomes the tangent point), and `crpix` is that pixel.
    #[test]
    fn recentered_at_is_exact_at_the_new_pixel() {
        let p = [pixel_center_of_size(4656.0), pixel_center_of_size(3522.0)];
        let w2 = FIELD1_WCS.recentered_at(p);
        assert_eq!(w2.crpix, p);
        let (ra, dec) = w2.pix_to_radec(p[0], p[1]);
        assert!(
            f64::abs(ra - w2.crval_deg[0]) < 1e-9 && f64::abs(dec - w2.crval_deg[1]) < 1e-9,
            "({ra},{dec}) vs {:?}",
            w2.crval_deg
        );
        // The new crval is the old WCS's sky position at the pixel.
        let (ra0, dec0) = FIELD1_WCS.pix_to_radec(p[0], p[1]);
        assert!(f64::abs(ra - ra0) < 1e-9 && f64::abs(dec - dec0) < 1e-9);
    }

    /// TAN cannot change the tangent point exactly without extra distortion
    /// terms, so the recentring agrees with the old WCS at the anchor and
    /// drifts elsewhere: linearly in the tangent-point move, quadratically in
    /// the distance from it. Here the anchor moves ~650 px on a 3.5 deg wide
    /// frame, which puts the worst corner at 0.88 px - well inside the 2 px
    /// budget asserted below.
    #[test]
    fn recentered_at_drift_within_budget_on_3_5deg_field() {
        let p = [pixel_center_of_size(4656.0), pixel_center_of_size(3522.0)];
        let w2 = FIELD1_WCS.recentered_at(p);
        for (x, y) in [(0.0, 0.0), (4656.0, 0.0), (0.0, 3522.0), (4656.0, 3522.0)] {
            let (ra, dec) = FIELD1_WCS.pix_to_radec(x, y);
            let (x2, y2) = w2.radec_to_pix(ra, dec);
            let drift = f64::hypot(x2 - x, y2 - y);
            assert!(drift <= 2.0, "corner ({x},{y}) drift {drift} px");
        }
    }

    /// Parity is invariant under recentring (`det J > 0`) and the scale moves
    /// only in the 4th digit: the relative scale drift is `sqrt(det J) - 1`,
    /// second order in the tangent-point move (5e-4 for this fixture's
    /// ~1950 px anchor move).
    #[test]
    fn recentered_at_preserves_parity_and_scale() {
        let p = [1234.5, 987.25];
        let w2 = FIELD1_WCS.recentered_at(p);
        assert_eq!(w2.parity_is_neg(), FIELD1_WCS.parity_is_neg());
        let rel = f64::abs(w2.arcsec_per_pix() / FIELD1_WCS.arcsec_per_pix() - 1.0);
        assert!(rel < 1e-3, "relative scale drift {rel}");
    }

    /// A NaN-poisoned WCS must be returned unchanged: reporting must never
    /// fail a solve, and a recentring of garbage is garbage.
    #[test]
    fn recentered_at_returns_self_on_non_finite() {
        let mut bad = FIELD1_WCS;
        bad.cd[0][0] = f64::NAN;
        let p = [100.0, 100.0];
        let w = bad.recentered_at(p);
        // NaN-poisoned input must come back unchanged field-by-field (NaN
        // never equals NaN, so compare the parts explicitly).
        assert_eq!(w.crpix, bad.crpix);
        assert_eq!(w.crval_deg, bad.crval_deg);
        assert!(w.cd[0][0].is_nan());
        assert_eq!(w.cd[0][1], bad.cd[0][1]);
        assert_eq!(w.cd[1][0], bad.cd[1][0]);
        assert_eq!(w.cd[1][1], bad.cd[1][1]);
    }

    /// The raw per-quad fits of a solving round disagree mostly because each
    /// one puts its tangent point on its own star centroid; moving them to the
    /// field-center tangent frame with [`Wcs::recentered_at`] removes that
    /// off-center divergence, which is what makes the printed rotation one
    /// well-defined number rather than one per quad.
    ///
    /// Nothing here is pinned to a quad id: a quad's raw fit is a property of
    /// one index file, not of the solver. The comparison peers are the solved
    /// hits of the round that really solved field 2, which
    /// `testutil::solved_hits_with_peers` extends with the following depth
    /// rounds whenever the winning round verifies fewer than two of them -
    /// their fits cover different field-star sets, which is exactly the
    /// material this test needs. Asserted is the property itself: the
    /// recentred raw fits agree with the winner's recentred frame within the
    /// budgets below (parity is checked exactly: invariance under recentring).
    #[test]
    fn recentered_field2_hits_agree_at_field_center() {
        // Budgets for the recentred raw fits of a solving round, each measured
        // against the winner's recentred frame. Parity is not budgeted: each
        // hit is compared with its own raw fit, exactly.
        const RECENTRE_ROT_DEG: f64 = 0.2;
        const RECENTRE_SCALE_FRAC: f64 = 0.005;
        const RECENTRE_CENTER_DEG: f64 = 0.05;

        let Some(run) = testutil::field_run("4656x3522-2.xyls", 4656.0, 3522.0, None) else {
            return;
        };
        let mut hits: Vec<(QuadHit, f64)> = testutil::solved_hits_with_peers(run, 2)
            .into_iter()
            .map(|h| (h.hit, h.verify.bestlogodds))
            .collect();
        let winner = hits
            .iter()
            .position(|(h, _)| h.quad == run.quad)
            .unwrap_or_else(|| {
                panic!(
                    "the round's reported winner quad {} is not among its solved hits",
                    run.quad
                )
            });
        hits.swap(0, winner);

        let p = [
            pixel_center_of_size(run.width),
            pixel_center_of_size(run.height),
        ];
        let (ref_hit, ref_odds) = (&hits[0].0, hits[0].1);
        let ref_w = ref_hit.wcs.recentered_at(p);
        for (hit, odds) in &hits[1..] {
            let w = hit.wcs.recentered_at(p);
            // Rotation agreement, wrapped to (-180, 180].
            let drot = testutil::wrap_deg(w.orientation_deg() - ref_w.orientation_deg());
            let drel = f64::abs(w.arcsec_per_pix() / ref_w.arcsec_per_pix() - 1.0);
            let dcen = f64::hypot(
                (w.crval_deg[0] - ref_w.crval_deg[0]) * f64::cos(w.crval_deg[1].to_radians()),
                w.crval_deg[1] - ref_w.crval_deg[1],
            );
            assert!(
                f64::abs(drot) < RECENTRE_ROT_DEG,
                "quad {} (odds {odds:.1}) vs winner {} (odds {ref_odds:.1}): recentred rotation \
                 differs by {drot} deg (raw fits {} vs {})",
                hit.quad,
                ref_hit.quad,
                hit.wcs.orientation_deg(),
                ref_hit.wcs.orientation_deg()
            );
            assert!(
                drel < RECENTRE_SCALE_FRAC,
                "quad {} vs {}: recentred scale differs by {drel}",
                hit.quad,
                ref_hit.quad
            );
            assert!(
                dcen < RECENTRE_CENTER_DEG,
                "quad {} vs {}: recentred center differs by {dcen} deg",
                hit.quad,
                ref_hit.quad
            );
            assert_eq!(
                w.parity_is_neg(),
                hit.wcs.parity_is_neg(),
                "quad {} parity under recentring",
                hit.quad
            );
        }
    }
}
