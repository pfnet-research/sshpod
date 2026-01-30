use crate::paths;
use anyhow::{Context, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::fs;
use tokio::process::Command;

pub struct Key {
    pub private: String,
    pub public: String,
}

pub async fn ensure_key(name: &str) -> Result<Key> {
    let cache_dir = paths::home_dir()?.join(".cache/sshpod");
    prepare_dir(&cache_dir, 0o700).await?;

    let private_key = cache_dir.join(name);
    let public_key = private_key.with_extension("pub");

    ensure_ed25519_keys(&private_key)
        .await
        .with_context(|| format!("failed to create keypair {}", name))?;

    let private = fs::read_to_string(&private_key)
        .await
        .with_context(|| format!("failed to read {}", private_key.display()))?;
    let public = fs::read_to_string(&public_key)
        .await
        .with_context(|| format!("failed to read {}", public_key.display()))?;

    Ok(Key { private, public })
}

async fn prepare_dir(path: &Path, mode: u32) -> Result<()> {
    fs::create_dir_all(path)
        .await
        .with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
    }
    Ok(())
}

async fn ensure_ed25519_keys(private_key: &Path) -> Result<()> {
    let public_key = private_key.with_extension("pub");
    if !private_key.exists() || !public_key.exists() {
        let status = Command::new("ssh-keygen")
            .args([
                "-q",
                "-t",
                "ed25519",
                "-f",
                private_key.to_str().unwrap_or_default(),
                "-N",
                "",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("failed to spawn ssh-keygen")?;
        if !status.success() {
            anyhow::bail!("ssh-keygen failed with status {}", status);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(private_key, std::fs::Permissions::from_mode(0o600)).await;
        let _ = fs::set_permissions(&public_key, std::fs::Permissions::from_mode(0o600)).await;
    }
    Ok(())
}
