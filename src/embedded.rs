pub fn get_bundle(arch: &str) -> Option<&'static [u8]> {
    match arch {
        "linux/amd64" => Some(include_bytes!("../bundles/sshd_amd64.xz")),
        "linux/arm64" => Some(include_bytes!("../bundles/sshd_arm64.xz")),
        _ => None,
    }
}

pub fn get_bundle_sha256(arch: &str) -> Option<&'static str> {
    match arch {
        "linux/amd64" => Some(env!("SSHPOD_BUNDLE_SHA256_AMD64")),
        "linux/arm64" => Some(env!("SSHPOD_BUNDLE_SHA256_ARM64")),
        _ => None,
    }
}
