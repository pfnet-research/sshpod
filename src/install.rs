use crate::paths;
use anyhow::{Context, Result};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

const START_MARKER: &str = "# >>> sshpod start";
const END_MARKER: &str = "# <<< sshpod end";

pub async fn run() -> Result<()> {
    let ssh_dir = paths::home_dir()?.join(".ssh");
    fs::create_dir_all(&ssh_dir)
        .with_context(|| format!("failed to create {}", ssh_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&ssh_dir, fs::Permissions::from_mode(0o700));
    }

    let config_path = ssh_dir.join("config");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_path = config_path
        .exists()
        .then(|| ssh_dir.join(format!("config.bak.{}", timestamp)));

    let current = if config_path.exists() {
        fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?
    } else {
        String::new()
    };

    let updated = merge_config(&current, &render_block());

    if current == updated {
        println!("No changes needed for {}", config_path.display());
        return Ok(());
    }

    if let Some(backup) = backup_path.as_ref() {
        fs::copy(&config_path, backup)
            .with_context(|| format!("failed to create backup {}", backup.as_path().display()))?;
    }

    let tmp_path = ssh_dir.join(format!("config.tmp.{}", timestamp));
    fs::write(&tmp_path, updated.as_bytes())
        .with_context(|| format!("failed to write temporary config {}", tmp_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp_path, &config_path).with_context(|| {
        format!(
            "failed to replace {} with updated config",
            config_path.display()
        )
    })?;

    println!("Updated {}", config_path.display());
    if let Some(backup) = backup_path {
        println!("Backup saved to {}", backup.display());
    }
    Ok(())
}

fn render_block() -> String {
    format!(
        r#"{start}
Host *.sshpod
  ProxyCommand ~/.local/bin/sshpod proxy --host %h --user %r --port %p
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
  GlobalKnownHostsFile /dev/null
  CheckHostIP no
  IdentityFile ~/.cache/sshpod/id_ed25519
  IdentitiesOnly yes
  BatchMode yes
  ForwardAgent yes
{end}
"#,
        start = START_MARKER,
        end = END_MARKER
    )
}

fn merge_config(current: &str, block: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in current.lines() {
        if line.trim() == START_MARKER {
            skipping = true;
            continue;
        }
        if skipping {
            if line.trim() == END_MARKER {
                skipping = false;
            }
            continue;
        }
        kept.push(line);
    }

    while kept.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        kept.pop();
    }

    let mut result = String::new();
    if !kept.is_empty() {
        result.push_str(&kept.join("\n"));
        result.push_str("\n\n");
    }
    result.push_str(block.trim_end());
    result.push('\n');
    result
}
