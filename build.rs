use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn resolve_dir(dir: &str) -> PathBuf {
    let p = Path::new(dir);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        Path::new(&manifest_dir).join(p)
    }
}

fn main() -> io::Result<()> {
    #[cfg(all(target_os = "windows", not(debug_assertions)))] {
        embed_resource::compile("mswin_stuff/win_resource.rc");
        println!("cargo:rerun-if-changed=mswin_stuff/win_resource.rc");
    }

    // Emitting `rerun-if-changed` disables cargo's default "rerun on any package
    // change", so every input of the generated values must be listed here.
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        // HEAD is worktree-local (branch switch); refs are updated in the common
        // dir even by linked worktrees and on commit (loose file or repacking).
        let gd = resolve_dir(&git_dir);
        println!("cargo:rerun-if-changed={}", gd.join("HEAD").display());
        let common_dir = match git(&["rev-parse", "--git-common-dir"]) {
            Some(d) => resolve_dir(&d),
            None => gd,
        };
        // Watched paths must exist: a `rerun-if-changed` path that never exists
        // makes cargo re-run the script on every build.
        for watch in [common_dir.join("refs").join("heads"), common_dir.join("packed-refs")] {
            if watch.exists() {
                println!("cargo:rerun-if-changed={}", watch.display());
            }
        }
    }

    if let Some(hash) = git(&["rev-parse", "--short", "HEAD"]) {
        let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
            .is_some_and(|s| !s.is_empty());
        println!("cargo:rustc-env=GIT_HASH={}{}", hash, if dirty { "-dirty" } else { "" });
    }
    if let Some(branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        println!("cargo:rustc-env=GIT_BRANCH={branch}");
    }

    Ok(())
}
