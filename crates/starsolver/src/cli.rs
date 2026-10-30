//! CLI surface: the `clap` `Args`, the scale-unit and angle parsing behind the
//! flags, and `SolveOptions` - CLI units converted into the solver's
//! (arcsec/pixel, degrees, seconds). Flag names and value forms stay compatible
//! with `solve-field` wherever we have the matching knob, so the rules below
//! record what a compatible command line may contain.
//!
//! `Args` is re-exported with crate-private fields, so callers build it by
//! parsing (`Args::parse()`, `Args::try_parse_from(..)`).

use anyhow::{Result, bail, ensure};
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

use crate::coords::{arcmin2arcsec, deg2arcsec};
use crate::params::{CPU_LIMIT_SECS, DEFAULT_RADECS_RADIUS_DEG, MAX_WIDTH_DEG, MIN_WIDTH_DEG};
use crate::solver::PositionHint;
use crate::xyls;

/// What the `--scale-low` / `--scale-high` numbers mean, as named by
/// `--scale-units`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScaleUnits {
    /// Field width in degrees (the default).
    DegWidth,
    /// Field width in arcminutes.
    ArcminWidth,
    /// Image scale, arcsec/pixel.
    ArcsecPerPix,
    /// 35mm-equivalent focal length; not supported.
    FocalMm,
}

/// Parses a `--scale-units` name, ignoring surrounding whitespace and letter
/// case; every unit but `focalmm` also has short aliases (`degw`/`dw`,
/// `amw`/`aw`, `app`).
fn parse_scale_units(s: &str) -> Result<ScaleUnits> {
    match s.trim().to_ascii_lowercase().as_str() {
        "degwidth" | "degw" | "dw" => Ok(ScaleUnits::DegWidth),
        "arcminwidth" | "amw" | "aw" => Ok(ScaleUnits::ArcminWidth),
        "arcsecperpix" | "app" => Ok(ScaleUnits::ArcsecPerPix),
        "focalmm" => Ok(ScaleUnits::FocalMm),
        other => bail!(
            "unknown --scale-units {other:?} (expected degwidth/degw/dw, arcminwidth/amw/aw, \
             arcsecperpix/app or focalmm)"
        ),
    }
}

/// Converts one `--scale-*` bound into arcsec/pixel: a field-width unit divided
/// by `width_px`, `arcsecperpix` the bound itself, `focalmm` refused. The converted
/// value must be positive and finite, so a zero, negative or non-finite bound fails.
fn to_arcsec_per_pix(units: ScaleUnits, value: f64, width_px: f64) -> Result<f64> {
    let app = match units {
        ScaleUnits::DegWidth => deg2arcsec(value) / width_px,
        ScaleUnits::ArcminWidth => arcmin2arcsec(value) / width_px,
        ScaleUnits::ArcsecPerPix => value,
        ScaleUnits::FocalMm => bail!("--scale-units focalmm is not supported"),
    };
    ensure!(
        app.is_finite() && app > 0.0,
        "scale must be positive and finite, got {app} arcsec/pixel"
    );
    Ok(app)
}

/// Parses a loose sexagesimal `[+-]dd:mm:ss`: optional sign, two colon-separated
/// components of one or two digits, then seconds whose digits are optional
/// (`digits* ('.' digits*)?`) - so `"1:2:"` and `"1:2:."` mean zero seconds. The
/// whole string must match: one colon, three digits in a component or trailing
/// junk returns `None`, leaving the caller to try another form.
fn parse_sexagesimal(s: &str, is_ra: bool) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut sign = 1.0;
    if let Some(&c) = b.first()
        && (c == b'+' || c == b'-')
    {
        sign = if c == b'-' { -1.0 } else { 1.0 };
        i = 1;
    }
    let mut comp = [0.0f64; 2];
    for slot in comp.iter_mut() {
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let digits = i - start;
        if digits == 0 || digits > 2 {
            return None;
        }
        *slot = s[start..i].parse::<f64>().ok()?;
        if i >= b.len() || b[i] != b':' {
            return None;
        }
        i += 1;
    }
    let start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i != b.len() {
        return None;
    }
    let sec_text = &s[start..i];
    let sec = match sec_text {
        "" | "." => 0.0,
        t => t.parse::<f64>().ok()?,
    };
    // Fold from the finest term up: deg (or hours) + min/60 + sec/3600, scaled
    // to degrees for RA, with the sign on the finished angle.
    let value = comp[0] + (comp[1] + sec / 60.0) / 60.0;
    Some(sign * if is_ra { 15.0 * value } else { value })
}

/// Parses one `--ra`/`--dec` value as degrees (whitespace trimmed): sexagesimal
/// first (`hh:mm:ss` for RA - so hours, `[+-]dd:mm:ss` for Dec), else decimal
/// degrees. Empty or non-finite is an error. A finished RA is folded into
/// [0, 360); a Dec is kept as written (no latitude range check).
fn parse_angle(s: &str, is_ra: bool) -> Result<f64> {
    let s = s.trim();
    ensure!(!s.is_empty(), "empty angle");
    if let Some(deg) = parse_sexagesimal(s, is_ra) {
        ensure!(deg.is_finite(), "angle {s:?} is not finite");
        return Ok(if is_ra { deg.rem_euclid(360.0) } else { deg });
    }
    let deg: f64 = s
        .parse()
        .map_err(|_| anyhow::anyhow!("cannot parse angle {s:?} (expected degrees or dd:mm:ss)"))?;
    ensure!(deg.is_finite(), "angle {s:?} is not finite");
    Ok(if is_ra { deg.rem_euclid(360.0) } else { deg })
}

#[derive(Parser, Debug)]
#[command(
    name = "starsolver",
    about = "A small astrometry.net solver: fits a WCS to a star list (.xyls).",
    override_usage = "starsolver [OPTIONS] [--width <N> --height <N>] <file.xyls>",
    term_width = 100
)]
pub struct Args {
    /// Directory of index files. Default: system astrometry.net indices.
    #[arg(short = 'I', long = "index-dir", value_name = "DIR")]
    pub(crate) index_dir: Option<PathBuf>,

    /// Width of the image, pixels. Default: the .xyls IMAGEW header.
    #[arg(short = 'w', long = "width", value_name = "N")]
    width: Option<u32>,

    /// Height of the image, pixels. Default: the .xyls IMAGEH header.
    #[arg(short = 'e', long = "height", value_name = "N")]
    height: Option<u32>,

    /// Lower bound on the image scale, in --scale-units.
    #[arg(short = 'L', long = "scale-low", value_name = "N")]
    scale_low: Option<f64>,

    /// Upper bound on the image scale, in --scale-units.
    #[arg(short = 'H', long = "scale-high", value_name = "N")]
    scale_high: Option<f64>,

    /// Units of --scale-low / --scale-high.
    #[arg(
        short = 'u',
        long = "scale-units",
        value_name = "U",
        default_value = "degwidth"
    )]
    scale_units: String,

    // allow_hyphen_values: for CLI compatibility a negative hint may be its own
    // token (`-3 -4 -4 -60` reads RA -4, Dec -60). Only --ra/--dec accept that -
    // elsewhere a `-` token is a missing value, not a value.
    /// Assuming RA of the field centre, degrees or hh:mm:ss.
    #[arg(
        short = '3',
        long = "ra",
        value_name = "RA",
        allow_hyphen_values = true
    )]
    ra: Option<String>,

    /// Assuming Dec of the field centre, degrees or [+-]dd:mm:ss.
    #[arg(
        short = '4',
        long = "dec",
        value_name = "DEC",
        allow_hyphen_values = true
    )]
    dec: Option<String>,

    /// Search radius around --ra/--dec, degrees.
    #[arg(short = '5', long = "radius", value_name = "DEG")]
    radius: Option<f64>,

    /// Wall-clock time limit, seconds.
    #[arg(short = 'l', long = "cpulimit", value_name = "SEC")]
    cpulimit: Option<f64>,

    /// Print progress to stderr.
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,

    /// Debug aid for stages 1-2: dump the tables of one index file and exit.
    #[arg(long = "dump-index", value_name = "FITS", hide = true)]
    pub(crate) dump_index: Option<PathBuf>,

    /// Input star list (.xyls).
    #[arg(value_name = "file.xyls")]
    file: Option<PathBuf>,
}

/// Everything the solver needs, with CLI units already converted.
pub struct SolveOptions {
    pub file: PathBuf,
    pub width: f64,
    pub height: f64,
    /// Image-scale bounds, arcsec/pixel.
    pub app_min: f64,
    pub app_max: f64,
    /// `(ra, dec, radius)` in degrees, when the caller gave a field position.
    pub radecs: Option<PositionHint>,
    /// CPU limit, seconds.
    pub cpulimit: f64,
    pub verbose: bool,
    /// Units the `--scale-*` bounds came in, kept for the verbose warning so
    /// `--scale-units` is parsed only once.
    pub(crate) units: ScaleUnits,
}

impl SolveOptions {
    pub(crate) fn from_args(args: &Args) -> Result<Self> {
        let file = args
            .file
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no input .xyls file given"))?;
        // The frame size: the CLI values win, but only as a pair - a missing or
        // zero flag is absent, and `xyls::frame_size` is a pair too (both keys
        // >= 1), so the two sides never come from different sources.
        let cli = |v: Option<u32>| v.filter(|n| *n >= 1);
        let (width, height) = match (cli(args.width), cli(args.height)) {
            (Some(w), Some(h)) => (w, h),
            _ => xyls::frame_size(&file)?.ok_or_else(|| {
                anyhow::anyhow!(
                    "image width and height must be given (-w/-e) or stored in the \
                     .xyls headers (IMAGEW/IMAGEH)"
                )
            })?,
        };
        let (width, height) = (f64::from(width), f64::from(height));

        let units = parse_scale_units(&args.scale_units)?;
        // One bound given means an exact scale, deliberately unlike solve-field,
        // which leaves the missing side open (widening to 0.1-180 deg).
        let given = match (args.scale_low, args.scale_high) {
            (Some(lo), Some(hi)) => Some((lo, hi)),
            (Some(lo), None) => Some((lo, lo)),
            (None, Some(hi)) => Some((hi, hi)),
            (None, None) => None,
        };
        let (app_min, app_max) = match given {
            Some((lo, hi)) => {
                let lo = to_arcsec_per_pix(units, lo, width)?;
                let hi = to_arcsec_per_pix(units, hi, width)?;
                ensure!(
                    lo <= hi,
                    "--scale-low ({lo} arcsec/px) is above --scale-high ({hi} arcsec/px)"
                );
                (lo, hi)
            }
            // Blind mode: MIN_WIDTH_DEG..MAX_WIDTH_DEG read like an explicit
            // width bound (divided by the frame width), always in degrees since
            // --scale-units has no bound to apply to.
            None => (
                deg2arcsec(MIN_WIDTH_DEG) / width,
                deg2arcsec(MAX_WIDTH_DEG) / width,
            ),
        };

        let radecs = match (&args.ra, &args.dec) {
            (Some(ra), Some(dec)) => Some(PositionHint {
                ra_deg: parse_angle(ra, true)?,
                dec_deg: parse_angle(dec, false)?,
                radius_deg: args.radius.unwrap_or(DEFAULT_RADECS_RADIUS_DEG),
            }),
            (None, None) => {
                ensure!(
                    args.radius.is_none(),
                    "--radius is only meaningful together with --ra and --dec"
                );
                None
            }
            _ => bail!("--ra and --dec must be given together"),
        };
        if let Some(h) = radecs {
            ensure!(
                h.radius_deg.is_finite() && h.radius_deg > 0.0,
                "--radius must be positive and finite"
            );
        }

        let cpulimit = args.cpulimit.unwrap_or(CPU_LIMIT_SECS);
        // Positive and Duration-representable: `Duration::from_secs_f64` panics
        // on `inf` or a huge value - exit 101 where an input error should exit 2.
        ensure!(
            cpulimit > 0.0 && Duration::try_from_secs_f64(cpulimit).is_ok(),
            "--cpulimit must be a positive, finite number of seconds"
        );

        Ok(Self {
            file,
            width,
            height,
            app_min,
            app_max,
            radecs,
            cpulimit,
            verbose: args.verbose,
            units,
        })
    }
}

/// True when a non-default `--scale-units` was named with no bound to apply to
/// (`run_into` warns under `-v`). `units` is pre-parsed, avoiding a second parse.
pub(crate) fn given_scale_missing(units: ScaleUnits, args: &Args) -> bool {
    units != ScaleUnits::DegWidth && args.scale_low.is_none() && args.scale_high.is_none()
}

/// Test helper: parses `extra` on top of `-I /tmp/indices`, a 1000x1000 frame
/// and `img.xyls`; `from_args` opens none of those and the CLI frame means no
/// header fallback. Also used by the sweep tests.
#[cfg(test)]
pub(crate) fn parse_options(extra: &[&str]) -> Result<SolveOptions> {
    let mut argv = vec![
        "starsolver",
        "-I",
        "/tmp/indices",
        "-w",
        "1000",
        "-e",
        "1000",
        "img.xyls",
    ];
    argv.extend_from_slice(extra);
    let args = Args::try_parse_from(argv).map_err(|e| anyhow::anyhow!("{e}"))?;
    SolveOptions::from_args(&args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_units_aliases() {
        assert_eq!(parse_scale_units("degwidth").unwrap(), ScaleUnits::DegWidth);
        assert_eq!(parse_scale_units("dw").unwrap(), ScaleUnits::DegWidth);
        assert_eq!(parse_scale_units("DW").unwrap(), ScaleUnits::DegWidth);
        assert_eq!(parse_scale_units("AMW").unwrap(), ScaleUnits::ArcminWidth);
        assert_eq!(
            parse_scale_units("arcsecperpix").unwrap(),
            ScaleUnits::ArcsecPerPix
        );
        assert!(parse_scale_units("bogus").is_err());
    }

    #[test]
    fn scale_unit_conversions() {
        // 3.5 degrees across 4656 px is 12600 / 4656 arcsec/pixel.
        let expected = 12600.0 / 4656.0;
        let app = to_arcsec_per_pix(ScaleUnits::DegWidth, 3.5, 4656.0).unwrap();
        assert!((app - expected).abs() < 1e-12);
        let app = to_arcsec_per_pix(ScaleUnits::ArcminWidth, 210.0, 4656.0).unwrap();
        assert!((app - expected).abs() < 1e-12);
        assert_eq!(
            to_arcsec_per_pix(ScaleUnits::ArcsecPerPix, 2.7, 4656.0).unwrap(),
            2.7
        );
        assert!(to_arcsec_per_pix(ScaleUnits::FocalMm, 50.0, 4656.0).is_err());
        assert!(to_arcsec_per_pix(ScaleUnits::DegWidth, 0.0, 4656.0).is_err());
    }

    #[test]
    fn angle_parsing() {
        // A plain number is degrees, for RA and Dec alike.
        assert!((parse_angle("60.031038", true).unwrap() - 60.031038).abs() < 1e-9);
        assert!((parse_angle("36.375976", false).unwrap() - 36.375976).abs() < 1e-9);
        // Sexagesimal RA is hours.
        assert!((parse_angle("04:00:07.449", true).unwrap() - 60.0310375).abs() < 1e-6);
        assert!((parse_angle("+36:22:33.512", false).unwrap() - 36.375976).abs() < 1e-6);
        // The sign applies to every component of a Dec.
        assert!((parse_angle("-36:22:33.512", false).unwrap() + 36.375976).abs() < 1e-6);
        // An empty seconds field counts as zero.
        assert!((parse_angle("1:2:", false).unwrap() - 1.0333333).abs() < 1e-6);
        // Negative RA is folded into [0, 360).
        assert!((parse_angle("-10", true).unwrap() - 350.0).abs() < 1e-9);
        // One colon is neither sexagesimal (two colons required) nor a number, so
        // it errors; solve-field would quietly take the numeric prefix instead.
        assert!(parse_angle("23:00", true).is_err());
        assert!(parse_angle("1:2:3:4", true).is_err());
        assert!(parse_angle("123:00:00", true).is_err());
        assert!(parse_angle("", true).is_err());
        assert!(parse_angle("N", false).is_err());
    }

    /// `--index-dir` is optional; the index files are located later, by
    /// `indexdir::IndexDirs`.
    #[test]
    fn options_parse_without_index_dir() {
        let args = Args::try_parse_from(["starsolver", "-w", "1000", "-e", "1000", "i.xyls"])
            .map_err(|e| anyhow::anyhow!("{e}"))
            .unwrap();
        assert!(args.index_dir.is_none());
        let opt = SolveOptions::from_args(&args).unwrap();
        assert_eq!(opt.file, PathBuf::from("i.xyls"));
    }

    /// The CLI frame counts only as a pair, else the size comes from the file:
    /// with no readable file this errors even though `-w` was given. A
    /// zero-valued flag counts as missing (a frame side must be >= 1 px).
    #[test]
    fn a_lone_frame_flag_defers_to_the_file() {
        let missing = "no-such-file-xyz.xyls";
        for argv in [
            &["-w", "1000", missing][..],
            &["-e", "1000", missing][..],
            &["-w", "0", "-e", "1000", missing][..],
        ] {
            let mut expected = vec!["starsolver", "-I", "/tmp/indices"];
            expected.extend_from_slice(argv);
            let args = Args::try_parse_from(expected)
                .map_err(|e| anyhow::anyhow!("{e}"))
                .unwrap();
            assert!(
                SolveOptions::from_args(&args).is_err(),
                "{argv:?} must fall back to the (unreadable) file"
            );
        }
    }

    /// A `.xyls` carrying the frame in its primary header: `from_args` reads
    /// headers only, so that is all the fallback needs.
    fn frame_xyls_bytes(w: u32, h: u32) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        for card in [
            "SIMPLE  =                    T",
            "BITPIX  =                    8",
            "NAXIS   =                    0",
            &format!("IMAGEW  = {w:>20}"),
            &format!("IMAGEH  = {h:>20}"),
            "END",
        ] {
            assert!(card.len() <= 80);
            buf.extend_from_slice(format!("{card:<80}").as_bytes());
        }
        buf.resize(2880.max(buf.len()), b' ');
        buf
    }

    /// With no `-w`/`-e` the frame comes from the file; a full CLI pair beats it.
    #[test]
    fn frame_falls_back_to_the_file_in_solve_options() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "starsolver_opts_frame_{}_{}.fits",
            std::process::id(),
            unique
        ));
        std::fs::write(&path, frame_xyls_bytes(1920, 1080)).unwrap();
        let file = path.to_str().unwrap();
        let parse = |extra: &[&str]| {
            let mut argv = vec!["starsolver", "-I", "/tmp/indices"];
            argv.extend_from_slice(extra);
            argv.push(file);
            Args::try_parse_from(argv)
                .map_err(|e| anyhow::anyhow!("{e}"))
                .and_then(|args| SolveOptions::from_args(&args))
        };
        let opt = parse(&[]).unwrap();
        assert_eq!((opt.width, opt.height), (1920.0, 1080.0));
        // The default blind scale range follows the file's width.
        assert_eq!(opt.app_max, deg2arcsec(MAX_WIDTH_DEG) / 1920.0);
        let opt = parse(&["-w", "640", "-e", "480"]).unwrap();
        assert_eq!((opt.width, opt.height), (640.0, 480.0));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn options_default_scale_range() {
        let opt = parse_options(&[]).unwrap();
        assert_eq!(opt.app_min, deg2arcsec(MIN_WIDTH_DEG) / 1000.0);
        assert_eq!(opt.app_max, deg2arcsec(MAX_WIDTH_DEG) / 1000.0);
        assert_eq!(opt.cpulimit, CPU_LIMIT_SECS);
        assert!(opt.radecs.is_none());
        assert!(!opt.verbose);
    }

    #[test]
    fn single_scale_bound_becomes_an_exact_range() {
        let opt = parse_options(&["--scale-low", "1.2"]).unwrap();
        assert_eq!(opt.app_min, deg2arcsec(1.2) / 1000.0);
        assert_eq!(opt.app_max, opt.app_min);
        let opt = parse_options(&["--scale-high", "1.2"]).unwrap();
        assert_eq!(opt.app_min, opt.app_max);
    }

    #[test]
    fn scale_units_apply_to_the_bounds() {
        let opt = parse_options(&["--scale-low", "210", "--scale-units", "arcminwidth"]).unwrap();
        assert!((opt.app_min - 12.6).abs() < 1e-12);
        assert!(parse_options(&["--scale-low", "1", "--scale-units", "focalmm"]).is_err());
        assert!(parse_options(&["--scale-low", "1", "--scale-units", "fmm"]).is_err());
    }

    #[test]
    fn reversed_scale_range_is_error() {
        assert!(parse_options(&["--scale-low", "2", "--scale-high", "1"]).is_err());
    }

    /// `--cpulimit inf` or a huge value is an input error (exit 2), not a
    /// `Duration` panic.
    #[test]
    fn cpulimit_rejects_non_finite_or_huge() {
        assert!(parse_options(&["--cpulimit", "inf"]).is_err());
        assert!(parse_options(&["--cpulimit", "1e300"]).is_err());
        assert!(parse_options(&["--cpulimit", "nan"]).is_err());
        // The guard must not over-reject a large-but-reasonable value.
        assert!(parse_options(&["--cpulimit", "1e9"]).is_ok());
    }

    /// Non-finite scale bounds and radius fail at the CLI, before the solver's
    /// own gate ever sees them.
    #[test]
    fn non_finite_scale_and_radius_are_errors() {
        assert!(parse_options(&["--scale-low", "inf"]).is_err());
        assert!(parse_options(&["--scale-high", "inf"]).is_err());
        assert!(parse_options(&["--scale-low", "nan"]).is_err());
        assert!(parse_options(&["--ra", "10", "--dec", "5", "--radius", "inf"]).is_err());
    }

    /// A negative hint may be a separate token (`-3 -4 -4 -60`), sexagesimal
    /// form included.
    #[test]
    fn negative_hints_parse_as_separate_tokens() {
        let opt = parse_options(&["--ra", "10", "--dec", "-60"]).unwrap();
        let h = opt.radecs.unwrap();
        assert_eq!((h.ra_deg, h.dec_deg), (10.0, -60.0));
        // Negative RA wraps into [0, 360).
        let opt = parse_options(&["-3", "-4", "-4", "-60:00:00"]).unwrap();
        let h = opt.radecs.unwrap();
        assert!((h.ra_deg - 356.0).abs() < 1e-12);
        assert!((h.dec_deg + 60.0).abs() < 1e-12);
    }

    /// Flags that must be positive do not swallow a following `-...` token.
    #[test]
    fn positive_only_flags_reject_hyphen_tokens() {
        assert!(parse_options(&["--cpulimit", "-1"]).is_err());
        assert!(parse_options(&["--scale-low", "-2"]).is_err());
        assert!(parse_options(&["--radius", "-5"]).is_err());
    }

    #[test]
    fn positional_hint_must_be_a_complete_pair() {
        assert!(parse_options(&["--ra", "10"]).is_err());
        assert!(parse_options(&["--dec", "10"]).is_err());
        assert!(parse_options(&["--radius", "5"]).is_err());
        // A radius of 0 is rejected, but a default one is filled in.
        assert!(parse_options(&["--ra", "10", "--dec", "5", "--radius", "0"]).is_err());
        let opt = parse_options(&["--ra", "10", "--dec", "5"]).unwrap();
        let h = opt.radecs.unwrap();
        assert_eq!((h.ra_deg, h.dec_deg), (10.0, 5.0));
        assert_eq!(h.radius_deg, DEFAULT_RADECS_RADIUS_DEG);
    }
}
