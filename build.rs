use sha2::{Digest, Sha256};
use std::fs;

// Expose the digest of each embedded sshd bundle, so the binary can tell
// whether the bundle already installed in a container is the one it carries.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for arch in ["amd64", "arm64"] {
        let path = format!("bundles/sshd_{arch}.xz");
        println!("cargo:rerun-if-changed={path}");
        let data = fs::read(&path).unwrap_or_else(|err| panic!("failed to read {path}: {err}"));
        let digest: String = Sha256::digest(&data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        println!(
            "cargo:rustc-env=SSHPOD_BUNDLE_SHA256_{}={digest}",
            arch.to_uppercase()
        );
    }
}
