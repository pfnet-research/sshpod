use crate::embedded;
use crate::kubectl::{self, RemoteTarget};
use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use log::info;
use std::borrow::Cow;
use std::collections::HashSet;
use std::env;
use std::io::{Read, Write};
use std::path::PathBuf;
use xz2::read::XzDecoder;

pub const BUNDLE_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+sshd3");
#[cfg(test)]
const BUNDLE_FILE_NAMES: [&str; 3] = ["sshd", "sshd-session", "sshd-auth"];
const TAR_BLOCK_SIZE: usize = 512;

struct BundleFiles {
    sshd: Vec<u8>,
    sshd_session: Vec<u8>,
    sshd_auth: Vec<u8>,
}

impl BundleFiles {
    fn entries(&self) -> [(&'static str, &[u8]); 3] {
        [
            ("sshd", self.sshd.as_slice()),
            ("sshd-session", self.sshd_session.as_slice()),
            ("sshd-auth", self.sshd_auth.as_slice()),
        ]
    }
}

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

    let has_tar = tool_available(target, "tar").await?;
    if has_tar && tool_available(target, "xz").await? {
        match install_bundle_archive(target, base, arch, &bundle_data, ArchiveCompression::Xz).await
        {
            Ok(()) => {
                stop_existing_sshd(target, base).await?;
                info!("[sshpod] bundle install completed");
                return Ok(());
            }
            Err(err) => {
                info!("[sshpod] xz/tar bundle install failed; falling back: {err}");
            }
        }
    }

    let mut tar_data = None;
    if has_tar {
        let data = decompress_xz(&bundle_data).context("failed to decompress bundle archive")?;

        if tool_available(target, "gzip").await? {
            let gzip_data = gzip_payload(&data).context("failed to prepare gzip bundle archive")?;
            match install_bundle_archive(target, base, arch, &gzip_data, ArchiveCompression::Gzip)
                .await
            {
                Ok(()) => {
                    stop_existing_sshd(target, base).await?;
                    info!("[sshpod] bundle install completed");
                    return Ok(());
                }
                Err(err) => {
                    info!("[sshpod] gzip/tar bundle install failed; falling back: {err}");
                }
            }
        }

        tar_data = Some(data);
    }

    let files = if let Some(data) = tar_data {
        extract_bundle_files_from_tar(&data)
    } else {
        extract_bundle_files(&bundle_data)
    }
    .with_context(|| format!("failed to unpack {} bundle locally", arch))?;
    install_bundle_files(target, base, arch, &files)
        .await
        .with_context(|| format!("failed to install bundle into {}", base))?;
    stop_existing_sshd(target, base).await?;

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

#[derive(Clone, Copy)]
enum ArchiveCompression {
    Xz,
    Gzip,
}

async fn install_bundle_archive(
    target: &RemoteTarget,
    base: &str,
    arch: &str,
    payload: &[u8],
    compression: ArchiveCompression,
) -> Result<()> {
    let tmp = format!("{}/bundle.new.{}", base, std::process::id());
    let prepare = prepare_bundle_command(base, &tmp);
    run_remote_shell(target, &prepare, &[], "prepare bundle directory").await?;

    let extract = extract_bundle_archive_command(&tmp, compression);
    run_remote_shell(target, &extract, payload, "extract bundle archive").await?;

    let finalize = finalize_bundle_command(base, &tmp, arch);
    run_remote_shell(target, &finalize, &[], "finalize bundle install").await?;
    Ok(())
}

fn extract_bundle_archive_command(tmp: &str, compression: ArchiveCompression) -> String {
    let extract = match compression {
        ArchiveCompression::Xz => format!("xz -dc | tar xf - -C \"{tmp}\""),
        ArchiveCompression::Gzip => format!("gzip -dc | tar xf - -C \"{tmp}\""),
    };
    format!("set -eu; {extract}")
}

async fn install_bundle_files(
    target: &RemoteTarget,
    base: &str,
    arch: &str,
    files: &BundleFiles,
) -> Result<()> {
    let tmp = format!("{}/bundle.new.{}", base, std::process::id());
    let prepare = prepare_bundle_command(base, &tmp);
    run_remote_shell(target, &prepare, &[], "prepare bundle directory").await?;

    for (name, data) in files.entries() {
        upload_bundle_file(target, &tmp, name, data).await?;
    }

    let finalize = finalize_bundle_command(base, &tmp, arch);
    run_remote_shell(target, &finalize, &[], "finalize bundle install").await?;
    Ok(())
}

fn prepare_bundle_command(base: &str, tmp: &str) -> String {
    format!(
        "set -eu; umask 077; mkdir -p \"{base}\"; rm -rf \"{tmp}\"; mkdir -p \"{tmp}\"; \
         chmod 700 \"{base}\" \"{tmp}\""
    )
}

fn finalize_bundle_command(base: &str, tmp: &str, arch: &str) -> String {
    let meta = format!(
        "printf '%s\\n' \"{BUNDLE_VERSION}\" > \"{base}/bundle/VERSION\"; \
         printf '%s\\n' \"{arch}\" > \"{base}/bundle/ARCH\"; \
         chmod 600 \"{base}/bundle/VERSION\" \"{base}/bundle/ARCH\""
    );
    format!(
        "set -eu; \
         test -x \"{tmp}/sshd\" -a -x \"{tmp}/sshd-session\" -a -x \"{tmp}/sshd-auth\"; \
         chmod 700 \"{tmp}\" \"{tmp}/sshd\" \"{tmp}/sshd-session\" \"{tmp}/sshd-auth\"; \
         rm -rf \"{base}/bundle\"; mv \"{tmp}\" \"{base}/bundle\"; {meta}"
    )
}

async fn upload_bundle_file(
    target: &RemoteTarget,
    tmp: &str,
    name: &'static str,
    data: &[u8],
) -> Result<()> {
    let path = format!("{}/{}", tmp, name);
    let cmd = upload_bundle_file_command(&path);
    run_remote_shell(target, &cmd, data, &format!("upload {}", name))
        .await
        .with_context(|| format!("failed to upload bundled {}", name))?;
    Ok(())
}

fn upload_bundle_file_command(path: &str) -> String {
    format!(
        "set -eu; umask 077; cat > \"{path}.tmp\"; mv \"{path}.tmp\" \"{path}\"; chmod 700 \"{path}\""
    )
}

async fn run_remote_shell(
    target: &RemoteTarget,
    command: &str,
    input: &[u8],
    label: &str,
) -> Result<()> {
    info!("[sshpod] {}", label);
    kubectl::exec_with_input_target(target, &["sh", "-c", command], input).await?;
    Ok(())
}

async fn stop_existing_sshd(target: &RemoteTarget, base: &str) -> Result<()> {
    let command = format!(
        "set -eu; \
         pid=\"$(cat \"{base}/sshd.pid\" 2>/dev/null || true)\"; \
         if [ -n \"$pid\" ]; then kill \"$pid\" 2>/dev/null || true; fi; \
         rm -f \"{base}/sshd.pid\" \"{base}/sshd.port\""
    );
    run_remote_shell(
        target,
        &command,
        &[],
        "stop existing sshd after bundle update",
    )
    .await
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

fn gzip_payload(data: &[u8]) -> Result<Vec<u8>> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(data).context("failed to write gzip payload")?;
    gz.finish().context("failed to finalize gzip payload")
}

fn extract_bundle_files(bundle_data: &[u8]) -> Result<BundleFiles> {
    let tar_data = decompress_xz(bundle_data)?;
    extract_bundle_files_from_tar(&tar_data)
}

fn extract_bundle_files_from_tar(data: &[u8]) -> Result<BundleFiles> {
    let mut offset = 0usize;
    let mut sshd = None;
    let mut sshd_session = None;
    let mut sshd_auth = None;

    while offset + TAR_BLOCK_SIZE <= data.len() {
        let header = &data[offset..offset + TAR_BLOCK_SIZE];
        if header.iter().all(|b| *b == 0) {
            break;
        }

        let name = tar_header_name(header)?;
        let normalized = name.strip_prefix("./").unwrap_or(&name);
        let size = tar_header_size(header)?;
        let data_start = offset + TAR_BLOCK_SIZE;
        let data_end = data_start
            .checked_add(size)
            .context("tar entry size overflow")?;
        if data_end > data.len() {
            bail!("tar entry {} extends past end of archive", name);
        }

        if matches!(header[156], 0 | b'0') {
            match normalized {
                "sshd" => set_bundle_file(&mut sshd, normalized, &data[data_start..data_end])?,
                "sshd-session" => {
                    set_bundle_file(&mut sshd_session, normalized, &data[data_start..data_end])?
                }
                "sshd-auth" => {
                    set_bundle_file(&mut sshd_auth, normalized, &data[data_start..data_end])?
                }
                _ => {}
            }
        }

        let padded_size = size.div_ceil(TAR_BLOCK_SIZE) * TAR_BLOCK_SIZE;
        offset = data_start
            .checked_add(padded_size)
            .context("tar entry offset overflow")?;
    }

    Ok(BundleFiles {
        sshd: take_bundle_file(sshd, "sshd")?,
        sshd_session: take_bundle_file(sshd_session, "sshd-session")?,
        sshd_auth: take_bundle_file(sshd_auth, "sshd-auth")?,
    })
}

fn set_bundle_file(slot: &mut Option<Vec<u8>>, name: &str, data: &[u8]) -> Result<()> {
    if data.is_empty() {
        bail!("bundle archive contains empty {}", name);
    }
    if slot.replace(data.to_vec()).is_some() {
        bail!("bundle archive contains duplicate {}", name);
    }
    Ok(())
}

fn take_bundle_file(file: Option<Vec<u8>>, name: &str) -> Result<Vec<u8>> {
    file.with_context(|| format!("bundle archive is missing {}", name))
}

fn tar_header_name(header: &[u8]) -> Result<String> {
    let name = tar_header_string(&header[0..100]).context("invalid tar entry name")?;
    let prefix = tar_header_string(&header[345..500]).context("invalid tar entry prefix")?;
    if name.is_empty() {
        bail!("tar entry has an empty name");
    }
    if prefix.is_empty() {
        Ok(name)
    } else {
        Ok(format!("{}/{}", prefix, name))
    }
}

fn tar_header_string(field: &[u8]) -> Result<String> {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    let value = std::str::from_utf8(&field[..end])
        .context("tar header field is not utf-8")?
        .trim_end_matches(' ')
        .to_string();
    Ok(value)
}

fn tar_header_size(header: &[u8]) -> Result<usize> {
    let field = &header[124..136];
    if field.first().is_some_and(|byte| byte & 0x80 != 0) {
        bail!("base-256 tar sizes are not supported in bundle archives");
    }
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    let size = std::str::from_utf8(&field[..end])
        .context("tar size field is not utf-8")?
        .trim();
    if size.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(size, 8).context("invalid tar size field")
}

#[cfg(test)]
mod tests {
    use super::{
        decompress_xz, extract_bundle_archive_command, extract_bundle_files,
        finalize_bundle_command, load_bundle_data, prepare_bundle_command,
        upload_bundle_file_command, ArchiveCompression, BUNDLE_FILE_NAMES,
    };
    use std::io::Write;
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
    fn extract_bundle_files_reads_embedded_archive() {
        for (arch, bundle) in [
            (
                "amd64",
                include_bytes!("../bundles/sshd_amd64.xz").as_slice(),
            ),
            (
                "arm64",
                include_bytes!("../bundles/sshd_arm64.xz").as_slice(),
            ),
        ] {
            let files = extract_bundle_files(bundle).expect("extract bundle files");
            let entries = files.entries();
            assert_eq!(
                entries.map(|(name, _)| name),
                BUNDLE_FILE_NAMES,
                "{} bundle file order should stay stable",
                arch
            );
            for (name, data) in entries {
                assert!(
                    data.starts_with(b"\x7fELF"),
                    "{} {} should be a Linux executable",
                    arch,
                    name
                );
            }
        }
    }

    #[test]
    fn install_commands_do_not_require_archive_or_split_tools() {
        let commands = [
            prepare_bundle_command("/tmp/sshpod/base", "/tmp/sshpod/base/bundle.new.test"),
            upload_bundle_file_command("/tmp/sshpod/base/bundle.new.test/sshd"),
            finalize_bundle_command(
                "/tmp/sshpod/base",
                "/tmp/sshpod/base/bundle.new.test",
                "linux/amd64",
            ),
        ]
        .join("\n");

        let tokens: Vec<&str> = commands
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
            .filter(|token| !token.is_empty())
            .collect();
        for tool in ["tar", "xz", "gzip", "dd"] {
            assert!(
                !tokens.contains(&tool),
                "remote install command must not require {}",
                tool
            );
        }
        assert!(tokens.contains(&"cat"));
    }

    #[test]
    fn archive_install_commands_use_remote_archive_tools_without_dd() {
        let xz = extract_bundle_archive_command(
            "/tmp/sshpod/base/bundle.new.test",
            ArchiveCompression::Xz,
        );
        assert!(xz.contains("xz -dc | tar xf -"));
        assert!(!xz.split_whitespace().any(|token| token == "dd"));

        let gzip = extract_bundle_archive_command(
            "/tmp/sshpod/base/bundle.new.test",
            ArchiveCompression::Gzip,
        );
        assert!(gzip.contains("gzip -dc | tar xf -"));
        assert!(!gzip.contains("xz -dc"));
        assert!(!gzip.split_whitespace().any(|token| token == "dd"));
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
