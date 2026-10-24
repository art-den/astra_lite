//! Success-block output formatting: the six-significant-digit `%g` renderer,
//! the sexagesimal strings and the five-line block `write_solution` emits into
//! any `Write`.
//!
//! The wording is the summary astrometry.net's `solve-field` prints for a
//! solved field, because the block is a product for console consumers - this
//! crate's own gates are such consumers (see [`write_solution`] for the one
//! line where our text differs).

use crate::sweep::Solution;
use std::io::Write;

/// The `printf` `%g` conversion at its default precision: six significant
/// digits, fixed notation while the value rounded to those digits stays in
/// `[1e-4, 1e6)`, scientific notation outside it, trailing zeros stripped, and
/// lowercase `nan` / `inf`.
#[must_use]
fn fmt_g(v: f64) -> String {
    if !v.is_finite() || v == 0.0 {
        // These print lowercase (`nan`, `inf`, `-inf`); Rust's `{}` spells the
        // first one `NaN`, hence the lowercasing, and it drops a NaN's sign
        // bit. A zero of either sign lands here too and prints `0` / `-0`, as
        // `%g` does.
        return format!("{v}").to_ascii_lowercase();
    }
    // The notation is decided by the exponent of the value ROUNDED to six
    // significant digits - the `e` of `{v:.5e}` - not by the unrounded
    // magnitude: 999999.9 rounds to 1.00000e6 and prints `1e+06`, not
    // `1000000`, while 9.999999e-05 rounds to 1.00000e-4 and prints `0.0001`.
    let sci = format!("{v:.5e}");
    let e: i32 = sci.split('e').nth(1).unwrap_or("0").parse().unwrap_or(0);
    if (-4..6).contains(&e) {
        // Six significant digits in fixed notation = 5 - exponent digits
        // after the decimal point.
        let prec = usize::try_from(5 - e).unwrap_or(0);
        let mut s = format!("{v:.prec$}");
        if s.contains('.') {
            // Strip the padding - but only where the digits really were
            // fractional: at exponent 5 there is no point and trimming an
            // integer like `100000` would leave `1`.
            s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        s
    } else {
        // Scientific notation: mantissa stripped the same way, exponent
        // carrying its sign and written with at least two digits (`1e-05`,
        // `1e+07`, `1e+100`).
        match sci.split_once('e') {
            Some((m, exp)) => {
                let m = m.trim_end_matches('0').trim_end_matches('.');
                let (sign, digits) = match exp.strip_prefix('-') {
                    Some(d) => ("-", d),
                    None => ("+", exp.strip_prefix('+').unwrap_or(exp)),
                };
                let digits = digits.trim_start_matches('0');
                let digits = if digits.is_empty() { "0" } else { digits };
                format!("{m}e{sign}{digits:0>2}")
            }
            None => sci,
        }
    }
}

/// RA in degrees as `hh:mm:ss.SSS`: wrapped into `[0, 360)` first, so an
/// out-of-range angle names a time inside the day instead of printing negative
/// hours, then divided by 15 to turn degrees into hours.
#[must_use]
fn ra_hms_string(ra: f64) -> String {
    let (h, m, ss, ds) = sexagesimal_parts(f64::rem_euclid(ra, 360.0) / 15.0);
    format!("{h:02}:{m:02}:{ss:02}.{ds:03}")
}

/// Dec in degrees as `±dd:mm:ss.SSS`: one sign for the whole string, taken
/// from the value (a negative zero counts as non-negative), with the parts
/// taken from its magnitude.
#[must_use]
fn dec_dms_string(dec: f64) -> String {
    let sign = if dec >= 0.0 { '+' } else { '-' };
    let (d, m, ss, ds) = sexagesimal_parts(f64::abs(dec));
    format!("{sign}{d:02}:{m:02}:{ss:02}.{ds:03}")
}

/// Splits a non-negative angle in `units` (hours for RA, degrees for Dec) into
/// `(unit, min, sec, millisec)`: units, minutes and whole seconds truncate,
/// milliseconds are the rounded remainder.
///
/// Each field wraps once and hands a single carry upward, which is all this
/// chain needs: the rounded millisecond remainder is at most 1000, so the
/// seconds it bumps reach at most 60, and then the minutes at most 60.
/// Minutes and seconds therefore come back below 60 while the largest field
/// stays unwrapped - an angle just short of 24 h or 90 deg prints 24 h / 90
/// deg, not 0. Non-negative input is a precondition (the callers wrap RA and
/// take the absolute value of Dec): for a negative angle the unit field floors
/// further down while its minutes come back positive, printing a misleading
/// string.
#[must_use]
fn sexagesimal_parts(units: f64) -> (i64, i64, i64, i64) {
    let mut u = f64::floor(units) as i64;
    let mf = (units - u as f64) * 60.0;
    let mut m = f64::floor(mf) as i64;
    let s = (mf - m as f64) * 60.0;
    let mut ss = f64::floor(s) as i64;
    let mut ds = f64::round((s - f64::from(ss as i32)) * 1000.0) as i64;
    if ds >= 1000 {
        ss += 1;
        ds -= 1000;
    }
    if ss >= 60 {
        ss -= 60;
        m += 1;
    }
    if m >= 60 {
        m -= 60;
        u += 1; // no wrap here: a carry out of 59 min leaves 24 h / 90 deg
    }
    (u, m, ss, ds)
}

/// The success block: the five lines, in this order, that a solved field puts
/// on stdout. Everything arrives already in degrees (the sweep computes it from
/// the report WCS); this only formats.
///
/// The center line is fixed-point at six decimals, the size and rotation lines
/// use the six-significant-digit form ([`fmt_g`]), and the parity prints `pos`
/// or `neg`.
///
/// One deliberate divergence from astrometry.net's `solve-field`: it switches
/// the size line to arcminutes, or arcseconds, once the smaller dimension is
/// under a degree, and it measures the extent along the image mid-lines instead
/// of multiplying the pixel count by the pixel scale; this clone always prints
/// degrees as `pixels * scale`. Both dimensions share a single unit word, so
/// fixing it to `degrees` is what lets a consumer divide the width back into a
/// pixel scale - the gates do exactly that, because no line prints a scale.
pub(crate) fn write_solution(sol: &Solution, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "Field center: (RA,Dec) = ({:.6}, {:.6}) deg.",
        sol.ra, sol.dec
    )?;
    writeln!(
        out,
        "Field center: (RA H:M:S, Dec D:M:S) = ({}, {}).",
        ra_hms_string(sol.ra),
        dec_dms_string(sol.dec)
    )?;
    writeln!(
        out,
        "Field size: {} x {} degrees",
        fmt_g(sol.width_deg),
        fmt_g(sol.height_deg)
    )?;
    writeln!(
        out,
        "Field rotation angle: up is {} degrees E of N",
        fmt_g(sol.orient_deg)
    )?;
    writeln!(
        out,
        "Field parity: {}",
        if sol.parity_neg { "neg" } else { "pos" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The six-significant-digit form the `Field size` and
    /// `Field rotation angle` lines print their numbers in.
    #[test]
    fn fmt_g_matches_the_reference_strings() {
        // Numbers of the shipped 4656x3522-1 field: the sizes and rotation its
        // reference run published (recorded in `tests/ground_truth.rs`; the
        // full-precision rotation below is the CD-derived one, see
        // `wcs::test_data::FIELD1_WCS`).
        assert_eq!(fmt_g(3.50687), "3.50687");
        assert_eq!(fmt_g(2.65305), "2.65305");
        assert_eq!(fmt_g(-178.9493244882495), "-178.949");
        // Trailing zeros (and a bare point) stripped.
        assert_eq!(fmt_g(0.5), "0.5");
        assert_eq!(fmt_g(1000.0), "1000");
        assert_eq!(fmt_g(123456.789), "123457");
        assert_eq!(fmt_g(0.0001), "0.0001");
        // Scientific notation outside the fixed range, exponent at least two
        // digits.
        assert_eq!(fmt_g(0.00001), "1e-05");
        assert_eq!(fmt_g(10.0f64.powi(7)), "1e+07");
        assert_eq!(fmt_g(0.0), "0");
        // Branch boundaries: the notation follows the ROUNDED value, so
        // 999999.9 -> `1e+06` (not `1000000`) and 9.999999e-05 -> `0.0001`
        // (not `1e-04`).
        assert_eq!(fmt_g(999999.9), "1e+06");
        assert_eq!(fmt_g(9.999999e-05), "0.0001");
        // Non-finite values print lowercase.
        assert_eq!(fmt_g(f64::NAN), "nan");
        assert_eq!(fmt_g(f64::INFINITY), "inf");
        assert_eq!(fmt_g(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn sexagesimal_strings() {
        // Field-1 center: 60.0310375 deg is 4 h 0 min 7.449 s exactly.
        assert_eq!(ra_hms_string(60.03103750), "04:00:07.449");
        assert_eq!(ra_hms_string(60.0), "04:00:00.000");
        assert_eq!(dec_dms_string(36.37597556), "+36:22:33.512");
        assert_eq!(dec_dms_string(-45.5), "-45:30:00.000");
        // Carry: the millisecond remainder rounds to 1000, bumping the seconds
        // to 60 and rolling the whole string up to the next degree.
        assert_eq!(
            dec_dms_string(10.0 + 59.0 / 60.0 + 59.9999 / 3600.0),
            "+11:00:00.000"
        );
        // RA just under 360 deg carries through sec, min and hours to 24 h.
        assert_eq!(ra_hms_string(359.99999999), "24:00:00.000");
    }

    /// Where no field overflows the helper is plain truncation: whole units,
    /// minutes, whole seconds and rounded milliseconds. It backs both printed
    /// strings.
    #[test]
    fn sexagesimal_parts_no_carry() {
        assert_eq!(sexagesimal_parts(4.002069166), (4, 0, 7, 449));
        assert_eq!(sexagesimal_parts(36.375975), (36, 22, 33, 510));
    }

    /// The assembled block pinned whole: literals, line order and the trailing
    /// newlines. The pieces have their own tests above, and the integration
    /// gates only parse the block back (prefixes and suffixes, never its exact
    /// text), so this is where the wording itself is checked.
    #[test]
    fn write_solution_golden_block() {
        use crate::sweep::Winner;
        let sol = Solution {
            ra: 15.0,
            dec: -0.25,
            width_deg: 1.25,
            height_deg: 0.5,
            orient_deg: -178.5,
            parity_neg: true,
            winner: Winner {
                indexid: 0,
                healpix: -1,
                quad: 0,
                log_odds: 0.0,
                obj_start: 0,
                max_objs: 0,
            },
        };
        let mut out = Vec::new();
        write_solution(&sol, &mut out).expect("writing to a Vec works");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Field center: (RA,Dec) = (15.000000, -0.250000) deg.\n\
             Field center: (RA H:M:S, Dec D:M:S) = (01:00:00.000, -00:15:00.000).\n\
             Field size: 1.25 x 0.5 degrees\n\
             Field rotation angle: up is -178.5 degrees E of N\n\
             Field parity: neg\n"
        );
    }
}
