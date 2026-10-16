//! Build metadata: mark a development build so `jjfx version` cannot pass for
//! a release.
//!
//! The version of record is a tag, and a binary built from `main` after that
//! tag carries work the release does not - while `CARGO_PKG_VERSION` is
//! identical. `jjfx version` therefore reports a `-dev` suffix for a
//! development build, so a bug report says which build is in hand.
//!
//! A build is a release in two cases: the release task declares its version
//! through `JJFX_RELEASE_VERSION`, or the crate is a published tarball, which
//! carries no VCS metadata. Every other build - a local `cargo build`, CI, a
//! `cargo install --git` - is a development build.

use std::env;
use std::path::PathBuf;

fn main() {
    // The marker is read through `option_env!` in the crate, so cargo must
    // rebuild when the release task sets or clears it.
    println!("cargo:rerun-if-env-changed=JJFX_RELEASE_VERSION");
    if is_development_build() {
        println!("cargo:rustc-env=JJFX_DEV_BUILD=1");
    }
}

/// Whether this build is a development build rather than a release.
fn is_development_build() -> bool {
    if env::var_os("JJFX_RELEASE_VERSION").is_some() {
        return false;
    }
    let Some(root) = env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from) else {
        return false;
    };
    // A checkout has VCS metadata; a crate unpacked from crates.io does not.
    root.join(".jj").exists() || root.join(".git").exists()
}
