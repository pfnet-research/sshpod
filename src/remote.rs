use crate::keys::Key;
use crate::kubectl::{self, RemoteTarget};
use anyhow::{bail, Context, Result};
use tokio::time::{timeout, Duration};

pub async fn try_acquire_lock(target: &RemoteTarget, base: &str) {
    let lock_cmd = format!("umask 077; mkdir \"{}/lock\"", base);
    let _ = kubectl::exec_capture_optional_target(target, &["sh", "-c", &lock_cmd]).await;
}

pub async fn assert_login_user_allowed(target: &RemoteTarget, login_user: &str) -> Result<()> {
    let uid = kubectl::exec_capture_target(target, &["id", "-u"])
        .await
        .context("failed to read remote uid")?;
    if uid.trim() == "0" {
        return Ok(());
    }
    let remote_user = kubectl::exec_capture_target(target, &["id", "-un"])
        .await
        .context("failed to read remote user")?;
    if remote_user.trim() != login_user {
        bail!(
            "This Pod runs as non-root. Use the container user for login (requested: {}, required: {}).",
            login_user,
            remote_user.trim()
        );
    }
    Ok(())
}

pub async fn install_host_keys(target: &RemoteTarget, base: &str, host_keys: &Key) -> Result<()> {
    let private = &host_keys.private;
    let public = &host_keys.public;
    let script = format!(
        r#"set -eu
BASE="{base}"
PRIV="$BASE/hostkeys/ssh_host_ed25519_key"
PUB="$BASE/hostkeys/ssh_host_ed25519_key.pub"
TMP_PRIV="$BASE/hostkeys/.tmp_priv"
TMP_PUB="$BASE/hostkeys/.tmp_pub"
umask 077
mkdir -p "$BASE" "$BASE/hostkeys" "$BASE/logs"
chmod 700 "$BASE" "$BASE/hostkeys"
cat > "$TMP_PRIV" <<'__SSH_PKEY__'
{private}
__SSH_PKEY__
cat > "$TMP_PUB" <<'__SSH_PUB__'
{public}
__SSH_PUB__
if [ -f "$PRIV" ] && [ -f "$PUB" ] && cmp -s "$PRIV" "$TMP_PRIV" && cmp -s "$PUB" "$TMP_PUB"; then
  rm -f "$TMP_PRIV" "$TMP_PUB"
  exit 0
fi
mv "$TMP_PRIV" "$PRIV"
mv "$TMP_PUB" "$PUB"
chmod 600 "$PRIV" "$PUB"
"#
    );
    kubectl::exec_with_input_target(target, &["sh", "-s"], script.as_bytes())
        .await
        .with_context(|| format!("failed to install host keys into {}", base))?;
    Ok(())
}

pub async fn ensure_sshd_running(
    target: &RemoteTarget,
    base: &str,
    login_user: &str,
    pubkey_line: &str,
) -> Result<u16> {
    let script = START_SSHD_SCRIPT.as_bytes();
    let output = timeout(Duration::from_secs(40), {
        kubectl::exec_with_input_target(
            target,
            &["sh", "-s", "--", base, login_user, pubkey_line],
            script,
        )
    })
    .await
    .map_err(|_| anyhow::anyhow!("starting sshd timed out after 40s"))?
    .with_context(|| format!("failed to start sshd under {}", base))?;

    let port: u16 = output
        .trim()
        .parse()
        .with_context(|| format!("unexpected sshd port output: {}", output))?;
    Ok(port)
}

const START_SSHD_SCRIPT: &str = r#"#!/bin/sh
set -eu

BASE="$1"
LOGIN_USER="$2"
PUBKEY_LINE="$3"
SSHD="$BASE/bundle/sshd"
ENV_FILE="$BASE/environment"

exec 3>&1
exec 1>&2

debug_log() {
  printf '[sshpod] %s\n' "$1" >&2
}

umask 077
mkdir -p "$BASE" "$BASE/logs" "$BASE/hostkeys"
chmod 700 "$BASE" "$BASE/hostkeys" "$BASE/logs"
BASE_PARENT="$(dirname "$BASE")"
TOP_DIR="$(dirname "$BASE_PARENT")"
chmod 711 "$TOP_DIR" "$BASE_PARENT"
debug_log "start script begin (base=$BASE user=$LOGIN_USER)"

get_home() {
  if command -v getent >/dev/null 2>&1; then
    getent passwd "$1" | awk -F: '{print $6}'
  elif [ -f /etc/passwd ]; then
    awk -F: -v u="$1" '$1==u {print $6}' /etc/passwd | head -n1
  fi
}

have_user() {
  if command -v getent >/dev/null 2>&1; then
    getent passwd "$1"
  elif [ -f /etc/passwd ]; then
    awk -F: -v u="$1" '$1==u {found=1} END{exit found?0:1}' /etc/passwd
  else
    return 1
  fi
}

if [ ! -f "$BASE/authorized_keys" ]; then
  : > "$BASE/authorized_keys"
fi
grep -qxF "$PUBKEY_LINE" "$BASE/authorized_keys" || printf '%s\n' "$PUBKEY_LINE" >> "$BASE/authorized_keys"
chmod 600 "$BASE/authorized_keys"
if [ -n "$LOGIN_USER" ]; then
  chown "$LOGIN_USER":"$LOGIN_USER" "$BASE" "$BASE/authorized_keys" || true
fi

mkdir -p /tmp/empty
chmod 755 /tmp/empty
if ! have_user sshd; then
  debug_log "creating sshd user"
  if command -v useradd >/dev/null 2>&1; then
    useradd -r -M -d /tmp/empty -s /sbin/nologin sshd || true
  elif command -v adduser >/dev/null 2>&1; then
    adduser -D -H -s /sbin/nologin -h /tmp/empty sshd || true
  fi
fi

if [ ! -f "$BASE/hostkeys/ssh_host_ed25519_key" ]; then
  echo "host key missing at $BASE/hostkeys/ssh_host_ed25519_key" >&2
  exit 1
fi
chmod 600 "$BASE/hostkeys/"*

if [ -f "$BASE/sshd.pid" ] && kill -0 "$(cat "$BASE/sshd.pid")" && [ -f "$BASE/sshd.port" ]; then
  debug_log "sshd already running"
  cat "$BASE/sshd.port" >&3
  exit 0
fi
debug_log "sshd not running, starting new instance"

rand_port() {
  val="$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')"
  echo $((20000 + (val % 45000)))
}

REMOTE_PATH="${PATH:-/usr/bin:/bin}"
ENV_EXPORTS="$(env | awk -F= '/^KUBERNETES_/ {print $1}')"
USER_HOME="$(get_home "$LOGIN_USER")"

i=0
while [ $i -lt 30 ]; do
  i=$((i+1))
  PORT="$(rand_port)"

  cat > "$BASE/sshd_config" <<EOF
ListenAddress 127.0.0.1
Port $PORT
HostKey $BASE/hostkeys/ssh_host_ed25519_key
PidFile $BASE/sshd.pid
AuthorizedKeysFile $BASE/authorized_keys
PubkeyAuthentication yes
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PermitEmptyPasswords no
AllowAgentForwarding yes
AllowTcpForwarding yes
X11Forwarding no
Subsystem sftp internal-sftp
LogLevel VERBOSE
PermitUserEnvironment yes
EOF

  printf 'SetEnv PATH=%s\n' "$REMOTE_PATH" >> "$BASE/sshd_config"
  for key in $ENV_EXPORTS; do
    val="$(printenv "$key" || true)"
    printf 'SetEnv %s=%s\n' "$key" "$val" >> "$BASE/sshd_config"
  done
  if [ -n "${KUBECONFIG:-}" ]; then
    printf 'SetEnv KUBECONFIG=%s\n' "$KUBECONFIG" >> "$BASE/sshd_config"
  fi
  if [ -n "$USER_HOME" ] && [ -d "$USER_HOME" ]; then
    mkdir -p "$USER_HOME/.ssh"
    {
      printf 'PATH=%s\n' "$REMOTE_PATH"
      for key in $ENV_EXPORTS; do
        val="$(printenv "$key" || true)"
        printf '%s=%s\n' "$key" "$val"
      done
      if [ -n "${KUBECONFIG:-}" ]; then
        printf 'KUBECONFIG=%s\n' "$KUBECONFIG"
      fi
    } > "$USER_HOME/.ssh/environment"
    chmod 700 "$USER_HOME/.ssh"
    chmod 600 "$USER_HOME/.ssh/environment"
    if [ -n "$LOGIN_USER" ]; then
      chown "$LOGIN_USER":"$LOGIN_USER" "$USER_HOME/.ssh" "$USER_HOME/.ssh/environment" || true
    fi
  fi

  {
    printf 'PATH=%s\n' "$REMOTE_PATH"
    for key in $ENV_EXPORTS; do
      val="$(printenv "$key" || true)"
      printf '%s=%s\n' "$key" "$val"
    done
    if [ -n "${KUBECONFIG:-}" ]; then
      printf 'KUBECONFIG=%s\n' "$KUBECONFIG"
    fi
  } > "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  if [ -n "$LOGIN_USER" ]; then
    chown "$LOGIN_USER":"$LOGIN_USER" "$ENV_FILE" || true
  fi

  chmod 600 "$BASE/sshd_config"
  rm -f "$BASE/sshd.pid"
  debug_log "launching sshd on $PORT"
  "$SSHD" -f "$BASE/sshd_config" -E "$BASE/logs/sshd.log" </dev/null || true
  j=0
  while [ $j -lt 10 ]; do
    if [ -f "$BASE/sshd.pid" ] && kill -0 "$(cat "$BASE/sshd.pid")"; then
      echo "$PORT" > "$BASE/sshd.port"
      chmod 600 "$BASE/sshd.pid" "$BASE/sshd.port"
      echo "$PORT" >&3
      exit 0
    fi
    j=$((j+1))
    sleep 1
  done
  debug_log "retrying sshd start (attempt $i)"
done

echo "sshd did not start" >&2
exit 1
"#;

#[cfg(test)]
mod tests {}
