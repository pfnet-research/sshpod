use crate::keys::Key;
use crate::kubectl::{self, RemoteTarget};
use anyhow::{bail, Context, Result};
use tokio::time::{timeout, Duration};

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
TMP_PRIV="$BASE/hostkeys/.tmp_priv.$$"
TMP_PUB="$BASE/hostkeys/.tmp_pub.$$"
trap 'rm -f "$TMP_PRIV" "$TMP_PUB"' EXIT
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

# Let concurrent clients start one sshd between them. A lock left behind by a
# killed script is ignored after 20 seconds instead of blocking every later login.
waited=0
until mkdir "$BASE/start.lock" 2>/dev/null || [ "$waited" -ge 20 ]; do
  waited=$((waited+1))
  sleep 1
done
trap 'rmdir "$BASE/start.lock" 2>/dev/null || true' EXIT
trap 'exit 1' HUP INT TERM

# A pid file can outlive its container when /tmp is a volume, so check that the
# pid still belongs to this sshd rather than to an unrelated process.
sshd_running() {
  pid="$(cat "$BASE/sshd.pid" 2>/dev/null)" &&
    grep -qF "$BASE/sshd_config" "/proc/$pid/cmdline" 2>/dev/null
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

# Sessions start from the environment that kubectl exec provides, which includes
# the variables of the image and the Pod spec. sshd keeps the variables it sets
# for each login, and a value with a newline cannot be written to sshd_config.
SET_ENV="$(awk 'BEGIN {
  for (name in ENVIRON) {
    value = ENVIRON[name]
    if (name !~ /^[A-Za-z_][A-Za-z0-9_]*$/ || index(value, "\n") ||
        name ~ /^(HOME|USER|LOGNAME|SHELL|MAIL|TERM|DISPLAY|PWD|OLDPWD|SHLVL|_|SSH_.*)$/)
      continue
    quoted = ""
    for (i = 1; i <= length(value); i++) {
      c = substr(value, i, 1)
      quoted = quoted (c == "\\" || c == "\"" ? "\\" : "") c
    }
    printf "\"%s=%s\"\n", name, quoted
  }
}' | sort | tr '\n' ' ')"

# sshd honors only the first SetEnv directive, so all variables share one line.
write_config() {
  cat <<EOF
ListenAddress 127.0.0.1
Port $1
HostKey $BASE/hostkeys/ssh_host_ed25519_key
SshdSessionPath $BASE/bundle/sshd-session
SshdAuthPath $BASE/bundle/sshd-auth
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
GatewayPorts clientspecified
X11Forwarding no
Subsystem sftp internal-sftp
LogLevel VERBOSE
EOF
  [ -z "$SET_ENV" ] || printf 'SetEnv %s\n' "$SET_ENV"
}

# Reuse the daemon only while it runs the configuration this script would write,
# so one started by another sshpod version is replaced. Open sessions stay up.
if sshd_running; then
  PORT="$(cat "$BASE/sshd.port" 2>/dev/null || true)"
  if write_config "$PORT" | cmp -s - "$BASE/sshd_config"; then
    debug_log "sshd already running"
    echo "$PORT" >&3
    exit 0
  fi
  kill "$pid" 2>/dev/null || true
fi
debug_log "starting sshd"

rand_port() {
  val="$(od -An -N2 -tu2 /dev/urandom | tr -d ' ')"
  echo $((20000 + (val % 45000)))
}

i=0
while [ $i -lt 30 ]; do
  i=$((i+1))
  PORT="$(rand_port)"
  write_config "$PORT" > "$BASE/sshd_config"
  rm -f "$BASE/sshd.pid"
  debug_log "launching sshd on $PORT"
  "$SSHD" -f "$BASE/sshd_config" -E "$BASE/logs/sshd.log" </dev/null || true
  j=0
  while [ $j -lt 10 ]; do
    if sshd_running; then
      echo "$PORT" > "$BASE/sshd.port"
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
mod tests {
    use super::START_SSHD_SCRIPT;

    #[test]
    fn start_script_avoids_user_home_side_effects() {
        assert!(!START_SSHD_SCRIPT.contains(".ssh/environment"));
        assert!(!START_SSHD_SCRIPT.contains("PermitUserEnvironment yes"));
    }

    #[test]
    fn start_script_allows_client_requested_remote_forward_bind_addresses() {
        assert!(START_SSHD_SCRIPT.contains("\nAllowTcpForwarding yes\n"));
        assert!(START_SSHD_SCRIPT.contains("\nGatewayPorts clientspecified\n"));
        assert!(!START_SSHD_SCRIPT.contains("\nGatewayPorts yes\n"));
    }

    #[test]
    fn start_script_uses_bundled_openssh_helper_binaries() {
        assert!(START_SSHD_SCRIPT.contains("\nSshdSessionPath $BASE/bundle/sshd-session\n"));
        assert!(START_SSHD_SCRIPT.contains("\nSshdAuthPath $BASE/bundle/sshd-auth\n"));
    }
}
