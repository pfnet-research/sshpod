pub fn get_bundle(arch: &str) -> Option<&'static [u8]> {
    match arch {
        "linux/amd64" => Some(include_bytes!("../bundles/sshd_amd64.xz")),
        "linux/arm64" => Some(include_bytes!("../bundles/sshd_arm64.xz")),
        _ => None,
    }
}
