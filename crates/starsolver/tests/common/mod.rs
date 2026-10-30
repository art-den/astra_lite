//! Helpers shared by the `tests/` targets. The gating mirrors
//! `src/testutil.rs`: a case self-skips when the data it needs is absent,
//! and `ASTROMETRY_REQUIRE_TEST_DATA=1` turns a skip into a panic so a
//! silently-empty run cannot pass for a real pass.
//!
//! Index-dir discovery mirrors `src/indexdir.rs` (the first cfg with a live
//! `add_path` wins, else the built-in dirs) because that module is private
//! and the library's own discovery ignores the test-only
//! `$ASTROMETRY_TEST_INDEX_DIR` (which therefore must be pinned via
//! `--index-dir`).
//!
//! Each test target uses only part of this module, hence the blanket
//! `dead_code` allow.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The built-in index directories of the library (`src/indexdir.rs`).
const BUILTIN_DIRS: [&str; 2] = ["/usr/share/astrometry", "/usr/local/astrometry/data"];

const ARCSEC_PER_DEG: f64 = 3600.0;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

pub fn strict_data() -> bool {
    matches!(
        std::env::var("ASTROMETRY_REQUIRE_TEST_DATA").as_deref(),
        Ok(v) if !v.is_empty() && v != "0"
    )
}

/// Reports a skip (a panic under `ASTROMETRY_REQUIRE_TEST_DATA`) so a case
/// can end with `if skip(..) { return; }`.
pub fn skip(msg: &str) -> bool {
    if strict_data() {
        panic!("ASTROMETRY_REQUIRE_TEST_DATA is set: {msg}");
    }
    eprintln!("skipping: {msg}");
    true
}

pub fn testdata_file(name: &str) -> Option<PathBuf> {
    let path = repo_root().join("testdata").join(name);
    if path.is_file() {
        Some(path)
    } else {
        skip(&format!("{} is absent", path.display()));
        None
    }
}

/// A fresh empty scratch directory under the system temp dir.
pub fn empty_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("starsolver-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

pub fn index_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("fits"))
        })
        .collect();
    files.sort();
    files
}

/// Where index files could live: the test-only env list when set, else the
/// existing `add_path` directories of the first config candidate with any
/// (mirroring `src/indexdir.rs`), else the built-in directories. Not every
/// candidate is mirrored - the exe-dir ones are meaningless under
/// `cargo test`.
fn candidate_dirs() -> Vec<PathBuf> {
    if let Some(list) = std::env::var_os("ASTROMETRY_TEST_INDEX_DIR") {
        return std::env::split_paths(&list).collect();
    }
    for cfg in cfg_candidates() {
        let dirs = live_add_paths(&cfg);
        if !dirs.is_empty() {
            return dirs;
        }
    }
    BUILTIN_DIRS.iter().map(PathBuf::from).collect()
}

fn cfg_candidates() -> Vec<PathBuf> {
    let mut cfgs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        cfgs.push(cwd.join("astrometry.cfg"));
    }
    cfgs.push(PathBuf::from("/etc/astrometry.cfg"));
    cfgs
}

/// The existing `add_path` directories of a config file (a word at the line
/// start beginning with `#` is a comment, like in the library).
fn live_add_paths(cfg: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(cfg) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(key), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        if !key.eq_ignore_ascii_case("add_path") {
            continue;
        }
        let dir = expand_tilde(value);
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    dirs
}

fn expand_tilde(word: &str) -> PathBuf {
    if (word == "~" || word.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let rest = word.trim_start_matches('~').trim_start_matches('/');
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(word)
}

/// How a usable index set was found.
pub enum Indices {
    /// Index files were found where the library itself looks; let it use its
    /// own discovery (no `--index-dir`, exactly what the bare binary does).
    System,
    /// `$ASTROMETRY_TEST_INDEX_DIR` named a single usable directory; it must
    /// be pinned with `--index-dir` because the library itself does not read
    /// that test-only variable.
    Pin(PathBuf),
}

/// The index set for the gated cases; `None` after reporting a skip.
pub fn indices() -> Option<Indices> {
    let dirs = candidate_dirs();
    let usable: Vec<PathBuf> = dirs
        .iter()
        .filter(|dir| !index_files_in(dir).is_empty())
        .cloned()
        .collect();
    if std::env::var_os("ASTROMETRY_TEST_INDEX_DIR").is_some() {
        return match usable.as_slice() {
            [one] => Some(Indices::Pin(one.clone())),
            [] => {
                skip("ASTROMETRY_TEST_INDEX_DIR names no directory with index files");
                None
            }
            _ => {
                skip("ASTROMETRY_TEST_INDEX_DIR names several usable dirs; --index-dir pins one");
                None
            }
        };
    }
    if usable.is_empty() {
        skip("no astrometry.net index files found via the config add_path or the built-in dirs");
        return None;
    }
    Some(Indices::System)
}

/// The first candidate directory that holds index files; `None` after reporting
/// a skip. For cases that have to name the directory themselves: a generated
/// field is made from a file inside the returned directory and its run is pinned
/// to that same directory, so a multi-directory `ASTROMETRY_TEST_INDEX_DIR` (which
/// [`indices`] refuses to guess between) is still self-consistent here.
pub fn one_index_dir() -> Option<PathBuf> {
    candidate_dirs()
        .into_iter()
        .find(|dir| !index_files_in(dir).is_empty())
        .or_else(|| {
            skip(
                "no astrometry.net index files found via the config add_path or the built-in dirs",
            );
            None
        })
}

/// The first `.fits` of a usable directory, for `--dump-index` style cases;
/// `None` after reporting a skip.
pub fn one_index_file() -> Option<PathBuf> {
    candidate_dirs()
        .iter()
        .find_map(|dir| index_files_in(dir).into_iter().next())
        .or_else(|| {
            skip("no .fits index file in the tested directories");
            None
        })
}

/// Relative deviation against `want`.
pub fn rel_frac(got: f64, want: f64) -> f64 {
    f64::abs(got - want) / f64::abs(want)
}

/// Normalizes an angle difference to (-180, 180].
pub fn normalize_deg(a: f64) -> f64 {
    let a = f64::rem_euclid(a + 180.0, 360.0) - 180.0;
    if a == -180.0 { a + 360.0 } else { a }
}

/// Great-circle distance in arcsec between a reported center and the truth
/// center. dRA is scaled by `cos(dec)` of the reported point.
pub fn center_off_arcsec(got_ra: f64, got_dec: f64, want_ra: f64, want_dec: f64) -> f64 {
    let dra = normalize_deg(got_ra - want_ra) * f64::cos(f64::to_radians(got_dec));
    f64::hypot(dra, got_dec - want_dec) * ARCSEC_PER_DEG
}

/// The five-line success block as parsed back from the printed text.
pub struct Printed {
    pub ra: f64,
    pub dec: f64,
    pub width_deg: f64,
    pub height_deg: f64,
    pub orient_deg: f64,
    /// `"pos"` or `"neg"`.
    pub parity: String,
}

/// Parses the five success lines; `None` when the block is incomplete or a
/// number does not parse (the gates treat both as "not solved").
pub fn parse_solution(text: &str) -> Option<Printed> {
    let mut center = None;
    let mut size = None;
    let mut orient = None;
    let mut parity = None;
    for line in text.lines() {
        if let Some(body) = line
            .strip_prefix("Field center: (RA,Dec) = (")
            .and_then(|b| b.strip_suffix(") deg."))
        {
            let (ra, dec) = body.split_once(", ")?;
            center = Some((ra.parse().ok()?, dec.parse().ok()?));
        } else if let Some(body) = line
            .strip_prefix("Field size: ")
            .and_then(|b| b.strip_suffix(" degrees"))
        {
            let (w, h) = body.split_once(" x ")?;
            size = Some((w.parse().ok()?, h.parse().ok()?));
        } else if let Some(body) = line
            .strip_prefix("Field rotation angle: up is ")
            .and_then(|b| b.strip_suffix(" degrees E of N"))
        {
            orient = Some(body.parse().ok()?);
        } else if let Some(p) = line.strip_prefix("Field parity: ")
            && (p == "pos" || p == "neg")
        {
            parity = Some(p.to_string());
        }
    }
    let (ra, dec) = center?;
    let (width_deg, height_deg) = size?;
    Some(Printed {
        ra,
        dec,
        width_deg,
        height_deg,
        orient_deg: orient?,
        parity: parity?,
    })
}
