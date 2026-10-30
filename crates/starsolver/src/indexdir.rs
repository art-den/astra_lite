//! Index directories of the astrometry.net install found on the system, used
//! when `--index-dir` is absent.
//!
//! The first config that exists and supplies an existing `add_path` directory
//! wins; its directories are scanned in full (every regular `*.fits`, the cfg
//! `autoindex` behavior) while any `index`/`indexset`/`multiindex` names are
//! ignored with a warning. With no usable config, the built-in dirs are used.

use anyhow::{Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::{Unusable, path_list};

/// The config file name sought when `--index-dir` is absent (order: `cfg_candidates`).
const CFG_NAME: &str = "astrometry.cfg";
/// The conventional system-wide config location, tried unconditionally.
const SYSTEM_CFG: &str = "/etc/astrometry.cfg";

/// Fallback index directories when no config supplies one (package and source-install defaults).
const BUILTIN_DIRS: [&str; 2] = ["/usr/share/astrometry", "/usr/local/astrometry/data"];

/// The index directories to scan and how they were found.
pub struct IndexDirs {
    /// Directories to scan, in scan order.
    pub dirs: Vec<PathBuf>,
    /// Unusable index files fail the run for an explicit `--index-dir` but are only
    /// skipped in a discovered system directory, which also holds index formats we do not read.
    pub unusable: Unusable,
    /// Directories plus their origin, for messages.
    origin: String,
}

impl IndexDirs {
    /// Resolves the index directories; `explicit` is the `--index-dir` value.
    pub fn resolve(explicit: Option<&Path>) -> Result<Self> {
        let builtins: Vec<PathBuf> = BUILTIN_DIRS.iter().map(PathBuf::from).collect();
        Self::resolve_with(explicit, &cfg_candidates(), &builtins)
    }

    /// `resolve` with the candidate lists injected, so tests do not depend on this
    /// machine's `/etc` or on where the test binary lives.
    fn resolve_with(
        explicit: Option<&Path>,
        cfgs: &[PathBuf],
        builtins: &[PathBuf],
    ) -> Result<Self> {
        if let Some(dir) = explicit {
            return Ok(Self {
                dirs: vec![dir.to_path_buf()],
                unusable: Unusable::Fail,
                origin: format!("{}", dir.display()),
            });
        }
        // A cfg naming index files but no directory earns a warning once we fall back.
        let mut listing_cfg: Option<PathBuf> = None;
        for cfg in cfgs {
            if !cfg.is_file() {
                continue;
            }
            // An unreadable config must not hide the candidates after it, so only warn;
            // lossy decode lets a non-UTF-8 file still parse.
            let Ok(bytes) = fs::read(cfg) else {
                eprintln!("Warning: cannot read config file {}", cfg.display());
                continue;
            };
            let text = String::from_utf8_lossy(&bytes);
            let dirs = keep_existing(add_paths(&text));
            let listed = lists_indices(&text);
            if dirs.is_empty() {
                if listed {
                    listing_cfg = Some(cfg.clone());
                }
                continue;
            }
            if listed {
                eprintln!(
                    "Warning: {} lists index files; only its add_path directories are used and \
                     every index file in them is scanned",
                    cfg.display()
                );
            }
            return Ok(Self {
                origin: format!("{} (from {})", path_list(&dirs), cfg.display()),
                dirs,
                unusable: Unusable::Skip,
            });
        }
        let dirs = keep_existing(builtins.to_vec());
        if dirs.is_empty() {
            bail!(
                "no astrometry.net indices found on the system: pass --index-dir (looked in {})",
                path_list(cfgs)
            );
        }
        if let Some(cfg) = listing_cfg {
            eprintln!(
                "Warning: {} names index files but no add_path directory, so the built-in \
                 directories are used",
                cfg.display()
            );
        }
        Ok(Self {
            origin: format!("{} (the built-in install directories)", path_list(&dirs)),
            dirs,
            unusable: Unusable::Skip,
        })
    }

    /// The scanned directories and their origin, for messages.
    pub fn describe(&self) -> String {
        self.origin.clone()
    }
}

/// Config files tried when `--index-dir` is absent, in order: cwd (the most
/// local override), the system-wide config, next to our binary, then `etc/` of its prefix.
fn cfg_candidates() -> Vec<PathBuf> {
    let mut cfgs = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        cfgs.push(cwd.join(CFG_NAME));
    }
    cfgs.push(PathBuf::from(SYSTEM_CFG));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    if let Some(dir) = &exe_dir {
        cfgs.push(dir.join(CFG_NAME));
        // The exe directory's parent is treated as an install prefix.
        if let Some(prefix) = dir.parent() {
            cfgs.push(prefix.join("etc").join(CFG_NAME));
        }
    }
    cfgs
}

/// The words of a cfg line: a leading `#` comments the whole line, otherwise a
/// word starting with `#` ends its data, so `#` inside a word is ordinary text.
fn words(line: &str) -> impl Iterator<Item = &str> {
    let commented = line
        .split_whitespace()
        .next()
        .is_some_and(|w| w.starts_with('#'));
    line.split_whitespace()
        .take_while(move |w| !commented && !w.starts_with('#'))
}

/// The `add_path` directories of a cfg. Relative paths stay relative (scanned against cwd);
/// `~`/`~/` expands from `$HOME`, a convenience this solver adds on top of the cfg format.
fn add_paths(cfg: &str) -> Vec<PathBuf> {
    cfg.lines()
        .filter_map(|line| {
            let mut words = words(line);
            let key = words.next()?;
            let value = words.next()?;
            key.eq_ignore_ascii_case("add_path")
                .then(|| expand_tilde(value))
        })
        .collect()
}

/// Whether a cfg names index files itself (`index`/`indexset`/`multiindex` with
/// a value); we ignore those names and scan `add_path` dirs, so the user is warned.
fn lists_indices(cfg: &str) -> bool {
    cfg.lines().any(|line| {
        let mut words = words(line);
        words.next().is_some_and(|key| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "index" | "indexset" | "multiindex"
            )
        }) && words.next().is_some()
    })
}

/// Expands a leading `~`/`~/` from `$HOME`; without it the word is kept, so it
/// normally fails the existing-directory check later.
fn expand_tilde(word: &str) -> PathBuf {
    if (word == "~" || word.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let rest = word.trim_start_matches('~').trim_start_matches('/');
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(word)
}

/// Drops duplicates, non-directories and missing directories, keeping the order.
fn keep_existing(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if dir.is_dir() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory for one test, removed before and after it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("ansolve_indexdir_{tag}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }

        fn dir(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            fs::create_dir_all(&dir).expect("create scratch subdir");
            dir
        }

        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, contents).expect("write scratch file");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn cfg_text(dir: &Path) -> String {
        format!("# site config\nadd_path {}\nautoindex\n", dir.display())
    }

    #[test]
    fn add_paths_reads_the_directories_and_ignores_other_keys() {
        let cfg = "# astrometry.cfg\n\
                   #inparallel\n\
                   cpulimit 300\n\
                   \tadd_path /usr/share/astrometry\n\
                   # add_path /commented/out\n\
                   add_path /data/indices   # trailing comment\n\
                   autoindex\n";
        assert_eq!(
            add_paths(cfg),
            vec![
                PathBuf::from("/usr/share/astrometry"),
                PathBuf::from("/data/indices")
            ]
        );
        assert!(!lists_indices(cfg));
    }

    #[test]
    fn add_path_needs_a_value_and_may_contain_a_hash() {
        assert!(add_paths("add_path\nadd_path   \n").is_empty());
        assert_eq!(
            add_paths("add_path /data#v1"),
            vec![PathBuf::from("/data#v1")]
        );
    }

    #[test]
    fn index_lists_are_detected_in_every_cfg_spelling() {
        assert!(lists_indices("add_path /data\nindex index-4119\n"));
        assert!(lists_indices("indexset deep\n"));
        assert!(lists_indices("multiindex 4110 4111\n"));
        // Commented-out entries and `autoindex` are not listings.
        assert!(!lists_indices("#index index-4118\nautoindex\n"));
    }

    #[test]
    fn add_path_words_are_expanded_or_kept() {
        assert_eq!(add_paths("add_path data/x"), vec![PathBuf::from("data/x")]);
        let home = std::env::var_os("HOME");
        let expanded = add_paths("add_path ~/astrometry/data");
        if let Some(home) = home {
            assert_eq!(expanded, vec![PathBuf::from(home).join("astrometry/data")]);
        } else {
            assert_eq!(expanded, vec![PathBuf::from("~/astrometry/data")]);
        }
    }

    #[test]
    fn cfg_candidates_are_tried_cwd_then_etc_then_prefix() {
        let cfgs = cfg_candidates();
        let cwd = std::env::current_dir().unwrap();
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(cfgs[0], cwd.join(CFG_NAME));
        assert_eq!(cfgs[1], PathBuf::from(SYSTEM_CFG));
        assert_eq!(cfgs[2], exe_dir.join(CFG_NAME));
        assert_eq!(
            cfgs[3],
            exe_dir.parent().unwrap().join("etc").join(CFG_NAME)
        );
        assert_eq!(cfgs.len(), 4);
    }

    /// Only real directories survive: a plain file is not a directory, so a missing
    /// candidate never reaches the scan (and never surfaces as a confusing error).
    #[test]
    fn keep_existing_drops_missing_paths_and_duplicates() {
        let scratch = Scratch::new("keep");
        let dir = scratch.dir("indices");
        let file = scratch.file("not-a-dir.fits", b"x");
        let out = keep_existing(vec![
            dir.clone(),
            dir.clone(),
            file,
            PathBuf::from("/no/such/dir"),
        ]);
        assert_eq!(out, vec![dir]);
    }

    /// An explicit `--index-dir` is used verbatim even when missing: the user must
    /// hear about it, so the directory is not filtered and an unusable index is not skipped.
    #[test]
    fn explicit_dir_is_kept_verbatim_and_strict() {
        let scratch = Scratch::new("explicit");
        let missing = PathBuf::from("/no/such/index-dir");
        let dirs = IndexDirs::resolve_with(Some(&missing), &[], &[scratch.dir("b")]).unwrap();
        assert_eq!(dirs.dirs, vec![missing]);
        assert_eq!(dirs.unusable, Unusable::Fail);
        // The candidate lists are not consulted at all.
        assert!(!dirs.describe().contains("built-in"));
    }

    #[test]
    fn cfg_add_path_directories_are_used() {
        let scratch = Scratch::new("cfg");
        let indices = scratch.dir("indices");
        let cfg = scratch.file(CFG_NAME, cfg_text(&indices).as_bytes());
        let dirs =
            IndexDirs::resolve_with(None, std::slice::from_ref(&cfg), &[scratch.dir("builtin")])
                .unwrap();
        assert_eq!(dirs.dirs, vec![indices]);
        assert_eq!(dirs.unusable, Unusable::Skip);
        assert!(dirs.describe().contains(&cfg.display().to_string()));
        assert!(dirs.describe().contains("from"));
    }

    /// The first config with a surviving `add_path` wins; a commented-only one yields to the next.
    #[test]
    fn first_config_with_a_directory_wins() {
        let scratch = Scratch::new("order");
        let a = scratch.dir("a");
        let b = scratch.dir("b");
        let empty = scratch.file("empty.cfg", b"#add_path /commented\nautoindex\n");
        let full = scratch.file("full.cfg", cfg_text(&a).as_bytes());
        let dirs = IndexDirs::resolve_with(None, &[empty, full.clone()], &[b]).unwrap();
        assert_eq!(dirs.dirs, vec![a]);
        assert!(dirs.describe().contains(&full.display().to_string()));
    }

    /// A cfg without directories falls back to the built-ins, warning if it names index
    /// files: that is where the user expected the indices to come from.
    #[test]
    fn config_without_add_path_falls_back_to_the_builtin_dirs() {
        let scratch = Scratch::new("fallback");
        let builtin = scratch.dir("builtin");
        let cfg = scratch.file(CFG_NAME, b"index index-4119\n");
        let dirs =
            IndexDirs::resolve_with(None, &[cfg], &[PathBuf::from("/no/such"), builtin.clone()])
                .unwrap();
        assert_eq!(dirs.dirs, vec![builtin]);
        assert!(dirs.describe().contains("built-in"));
    }

    /// Non-UTF-8 config bytes are parsed anyway (invalid ones replaced), not treated as absent.
    #[test]
    fn non_utf8_config_is_still_parsed() {
        let scratch = Scratch::new("utf8");
        let indices = scratch.dir("indices");
        let mut bytes = b"# \xff\xfe garbage\n".to_vec();
        bytes.extend_from_slice(cfg_text(&indices).as_bytes());
        let cfg = scratch.file(CFG_NAME, &bytes);
        let dirs = IndexDirs::resolve_with(None, &[cfg], &[]).unwrap();
        assert_eq!(dirs.dirs, vec![indices]);
    }

    #[test]
    fn nothing_found_asks_for_index_dir() {
        let err = IndexDirs::resolve_with(
            None,
            &[PathBuf::from("/no/astrometry.cfg")],
            &[PathBuf::from("/no/such/indices")],
        )
        .err()
        .expect("no candidates exist, so the resolution must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("--index-dir"), "{msg}");
        assert!(msg.contains("/no/astrometry.cfg"), "{msg}");
    }

    /// `resolve`, the real candidate-list reader, must return a self-consistent answer.
    #[test]
    fn resolve_reports_its_directories() {
        let dirs = IndexDirs::resolve(None);
        match dirs {
            Ok(dirs) => {
                assert!(!dirs.dirs.is_empty());
                assert_eq!(dirs.unusable, Unusable::Skip);
                assert!(
                    dirs.describe()
                        .contains(&dirs.dirs[0].display().to_string())
                );
            }
            // No install and no config is legitimate: an error naming what was tried.
            Err(e) => assert!(format!("{e:#}").contains("--index-dir")),
        }
    }
}
