use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

// The commit the binary was built from. SSHPOD_BUILD_COMMIT takes precedence
// because builds inside a container (cross) cannot always run git. Builds from
// a source archive have no commit and report the bare version.
fn build_commit() -> Option<String> {
    println!("cargo:rerun-if-env-changed=SSHPOD_BUILD_COMMIT");
    if let Ok(commit) = env::var("SSHPOD_BUILD_COMMIT") {
        let commit = commit.trim();
        if !commit.is_empty() {
            return Some(commit.chars().take(7).collect());
        }
    }

    // Rebuild when HEAD moves, including a new commit on the current branch.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &branch]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    git(&["rev-parse", "--short=7", "HEAD"])
}

// Expose the version shown by `--version`, and the digest of each embedded
// sshd bundle, so the binary can tell whether the bundle already installed in
// a container is the one it carries.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let version = match build_commit() {
        Some(commit) => format!("{version} ({commit})"),
        None => version,
    };
    println!("cargo:rustc-env=SSHPOD_VERSION={version}");

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
