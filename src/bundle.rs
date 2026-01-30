use crate::embedded;
use crate::kubectl::{self, RemoteTarget};
use anyhow::{anyhow, bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use log::info;
use std::borrow::Cow;
use std::collections::HashSet;
use std::env;
use std::io::{Read, Write};
use std::path::PathBuf;
use xz2::read::XzDecoder;

pub const BUNDLE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+sshd1");

pub async fn detect_remote_arch(target: &RemoteTarget) -> Result<String> {
    let machine = kubectl::exec_capture_target(target, &["uname", "-m"])
        .await
        .context("failed to detect remote arch via uname -m")?;
    let arch = match machine.trim() {
        "x86_64" | "amd64" => "linux/amd64",
        "aarch64" | "arm64" => "linux/arm64",
        other => {
            bail!("unsupported remote architecture: {}", other);
        }
    };
    Ok(arch.to_string())
}

pub async fn ensure_bundle(target: &RemoteTarget, base: &str, arch: &str) -> Result<()> {
    let version_path = format!("{}/bundle/VERSION", base);
    let arch_path = format!("{}/bundle/ARCH", base);
    let remote_version =
        kubectl::exec_capture_optional_target(target, &["cat", &version_path]).await?;
    let remote_arch = kubectl::exec_capture_optional_target(target, &["cat", &arch_path]).await?;

    info!(
        "[sshpod] checking bundle (remote version={:?}, remote arch={:?}, expected version={}, expected arch={})",
        remote_version, remote_arch, BUNDLE_VERSION, arch
    );
    if remote_version.as_deref() == Some(BUNDLE_VERSION) && remote_arch.as_deref() == Some(arch) {
        info!("[sshpod] bundle already up to date");
        return Ok(());
    }

    let bundle_data = load_bundle_data(arch).await?;

    let meta = format!(
        "printf '%s\\n' \"{BUNDLE_VERSION}\" > \"{base}/bundle/VERSION\"; \
         printf '%s\\n' \"{arch}\" > \"{base}/bundle/ARCH\"; \
         chmod 600 \"{base}/bundle/VERSION\" \"{base}/bundle/ARCH\";"
    );

    let install_xz = format!(
        "set -eu; umask 077; mkdir -p \"{base}/bundle\"; chmod 700 \"{base}\" \"{base}/bundle\"; \
         xz -dc > \"{base}/bundle/sshd\"; chmod 700 \"{base}/bundle/sshd\"; {meta}"
    );
    let install_gz = format!(
        "set -eu; umask 077; mkdir -p \"{base}/bundle\"; chmod 700 \"{base}\" \"{base}/bundle\"; \
         gzip -dc > \"{base}/bundle/sshd\"; chmod 700 \"{base}/bundle/sshd\"; {meta}"
    );
    let install_plain = format!(
        "set -eu; umask 077; mkdir -p \"{base}/bundle\"; chmod 700 \"{base}\" \"{base}/bundle\"; \
         cat > \"{base}/bundle/sshd\"; chmod 700 \"{base}/bundle/sshd\"; {meta}"
    );
    let mut sshd_data: Option<Vec<u8>> = None;

    let xz_err = match try_install_xz(target, &bundle_data, &install_xz).await {
        Ok(_) => {
            info!("[sshpod] bundle install completed");
            return Ok(());
        }
        Err(e) => e,
    };

    let gzip_err = match try_install_gzip(target, &bundle_data, &install_gz, &mut sshd_data).await {
        Ok(_) => {
            info!("[sshpod] bundle install completed");
            return Ok(());
        }
        Err(e) => e,
    };

    let sshd_data = ensure_plain_data(&bundle_data, &mut sshd_data)
        .context("failed to prepare sshd payload for plain install")?;
    install_bundle_with_command(target, &install_plain, sshd_data, "plain")
        .await
        .with_context(|| {
            format!(
                "failed to install bundle into {} (xz: {}; gzip: {})",
                base, xz_err, gzip_err
            )
        })?;

    info!("[sshpod] bundle install completed");
    Ok(())
}

async fn load_bundle_data(arch: &str) -> Result<Cow<'static, [u8]>> {
    if let Some(data) = embedded::get_bundle(arch) {
        info!("[sshpod] using embedded bundle for {}", arch);
        Ok(Cow::from(data))
    } else {
        let bundle_path = locate_bundle(arch)?;
        info!("[sshpod] using local bundle file {}", bundle_path.display());
        let bytes = tokio::fs::read(&bundle_path)
            .await
            .with_context(|| format!("failed to read bundle {}", bundle_path.display()))?;
        Ok(Cow::from(bytes))
    }
}

async fn tool_available(target: &RemoteTarget, tool: &str) -> Result<bool> {
    Ok(kubectl::exec_capture_optional_target(
        target,
        &["sh", "-c", &format!("command -v {}", tool)],
    )
    .await?
    .is_some())
}

fn ensure_plain_data<'a>(
    bundle_data: &'a [u8],
    cache: &'a mut Option<Vec<u8>>,
) -> Result<&'a [u8]> {
    if cache.is_none() {
        *cache = Some(decompress_xz(bundle_data)?);
    }
    Ok(cache.as_ref().unwrap())
}

fn gzip_payload(data: &[u8]) -> Result<Vec<u8>> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(data).context("failed to write gzip payload")?;
    gz.finish().context("failed to finalize gzip payload")
}

async fn try_install_xz(
    target: &RemoteTarget,
    bundle_data: &[u8],
    install_cmd: &str,
) -> Result<()> {
    if !tool_available(target, "xz").await? {
        info!("[sshpod] skipping xz install (xz not available)");
        return Err(anyhow!("xz not available in container"));
    }
    install_bundle_with_command(target, install_cmd, bundle_data, "xz").await
}

async fn try_install_gzip(
    target: &RemoteTarget,
    bundle_data: &[u8],
    install_cmd: &str,
    sshd_cache: &mut Option<Vec<u8>>,
) -> Result<()> {
    if !tool_available(target, "gzip").await? {
        info!("[sshpod] skipping gzip install (gzip not available)");
        return Err(anyhow!("gzip not available in container"));
    }
    let sshd_data_ref = ensure_plain_data(bundle_data, sshd_cache)?;
    let gz_data = gzip_payload(sshd_data_ref)?;
    install_bundle_with_command(target, install_cmd, &gz_data, "gzip").await
}

async fn install_bundle_with_command(
    target: &RemoteTarget,
    install_cmd: &str,
    payload: &[u8],
    label: &str,
) -> Result<()> {
    info!("[sshpod] installing bundle via {}", label);
    kubectl::exec_with_input_target(target, &["sh", "-c", install_cmd], payload).await?;
    Ok(())
}

fn locate_bundle(arch: &str) -> Result<PathBuf> {
    let filename = match arch {
        "linux/amd64" => "sshd_amd64.xz".to_string(),
        "linux/arm64" => "sshd_arm64.xz".to_string(),
        _ => format!("sshd_{}.xz", arch.replace('/', "_")),
    };
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    candidates.push(PathBuf::from(&filename));
    candidates.push(PathBuf::from("bundles").join(&filename));
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(&filename));
            candidates.push(dir.join("bundles").join(&filename));
            if let Some(root) = dir.parent() {
                candidates.push(root.join("bundles").join(&filename));
            }
        }
    }

    for candidate in candidates.into_iter().filter(|p| seen.insert(p.clone())) {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!(
        "bundle file {} not found; place it alongside the binary or in ./bundles",
        filename
    );
}

fn decompress_xz(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = XzDecoder::new(data);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .context("failed to decompress xz")?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::{decompress_xz, ensure_plain_data, gzip_payload, load_bundle_data};
    use flate2::read::GzDecoder;
    use std::io::{Read, Write};
    use std::{fs, path::PathBuf};
    use tokio::runtime::Runtime;
    use xz2::write::XzEncoder;

    #[test]
    fn decompress_smoke() {
        let mut encoder = XzEncoder::new(Vec::new(), 6);
        encoder.write_all(b"hello world").unwrap();
        let data = encoder.finish().unwrap();
        let out = decompress_xz(&data).expect("decompress");
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn ensure_plain_data_caches_decompression() {
        let mut encoder = XzEncoder::new(Vec::new(), 6);
        encoder.write_all(b"cache me").unwrap();
        let data = encoder.finish().unwrap();

        let mut cache = None;
        let first = ensure_plain_data(&data, &mut cache).expect("first decode");
        assert_eq!(first, b"cache me");
        let first_ptr = first.as_ptr();

        let second = ensure_plain_data(&data, &mut cache).expect("second decode");
        assert_eq!(second, b"cache me");
        assert_eq!(first_ptr, second.as_ptr(), "cache should be reused");
    }

    #[test]
    fn gzip_payload_round_trip() {
        let gz = gzip_payload(b"ping").expect("gzip");
        let mut decoder = GzDecoder::new(&gz[..]);
        let mut out = String::new();
        decoder.read_to_string(&mut out).expect("gunzip");
        assert_eq!(out.as_bytes(), b"ping");
    }

    #[test]
    fn load_bundle_data_reads_filesystem() {
        let rt = Runtime::new().unwrap();
        let path = PathBuf::from("sshd_test.xz");

        let mut encoder = XzEncoder::new(Vec::new(), 6);
        encoder.write_all(b"from file").unwrap();
        let data = encoder.finish().unwrap();
        fs::write(&path, &data).expect("write test bundle");

        let loaded = rt
            .block_on(load_bundle_data("test"))
            .expect("load bundle data");
        assert_eq!(&*loaded, data.as_slice());

        fs::remove_file(&path).ok();
    }
}
