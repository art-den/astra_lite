//! Build-time git metadata, provided by `build.rs` via `cargo:rustc-env`.
//! In non-git builds (e.g. a source tarball) the env vars are absent and the
//! constants are empty.

pub const GIT_HASH: &str = match option_env!("GIT_HASH") {
    Some(h) => h,
    None => "",
};
pub const GIT_BRANCH: &str = match option_env!("GIT_BRANCH") {
    Some(b) => b,
    None => "",
};

/// Package version with git branch and commit hash appended when available.
pub fn full_version() -> String {
    format_version(env!("CARGO_PKG_VERSION"), GIT_BRANCH, GIT_HASH)
}

fn format_version(ver: &str, branch: &str, hash: &str) -> String {
    if hash.is_empty() {
        ver.to_string()
    } else if branch.is_empty() {
        format!("{ver} ({hash})")
    } else {
        format!("{ver} ({branch} {hash})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_version_variants() {
        assert_eq!(
            format_version("1.2.3", "main", "abc1234-dirty"),
            "1.2.3 (main abc1234-dirty)"
        );
        assert_eq!(format_version("1.2.3", "", "abc1234"), "1.2.3 (abc1234)");
        assert_eq!(format_version("1.2.3", "", ""), "1.2.3");
    }
}
