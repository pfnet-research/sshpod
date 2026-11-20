use crate::keys::Key;
use crate::kubectl::{self, RemoteTarget};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tokio::time::{timeout, Duration};

pub(crate) fn parse_inherited_env(name: &str) -> Result<String, String> {
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        || !bytes.all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err("expected a POSIX environment variable name".into());
    }

    if matches!(
        name,
        "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "TERM"
            | "LANG"
            | "LANGUAGE"
            | "DISPLAY"
            | "XAUTHORITY"
            | "MAIL"
            | "PWD"
            | "OLDPWD"
            | "SHLVL"
            | "_"
            | "BASH_ENV"
            | "ENV"
            | "IFS"
            | "SHELLOPTS"
            | "BASHOPTS"
            | "PROMPT_COMMAND"
    ) || ["SSH_", "LC_", "LD_", "DYLD_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return Err(format!(
            "{name} is reserved for the SSH session or process startup"
        ));
    }

    Ok(name.into())
}

pub(crate) fn session_directory(
    pod_uid: &str,
    container: &str,
    container_id: &str,
    login_user: &str,
    inherited_env: &[String],
) -> String {
    // Reuse an sshd only within the same container instance and environment policy.
    let mut hash = Sha256::new();
    let policy_user = if inherited_env.is_empty() {
        ""
    } else {
        login_user
    };
    for part in [container_id, policy_user]
        .into_iter()
        .chain(inherited_env.iter().map(String::as_str))
    {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    format!(
        "/tmp/sshpod/{pod_uid}/{container}-session-v1-{:x}",
        hash.finalize()
    )
}

pub async fn assert_login_user_allowed(
    target: &RemoteTarget,
    login_user: &str,
    inherit_env: bool,
) -> Result<()> {
    let remote_user = kubectl::exec_capture_target(target, &["id", "-un"])
        .await
        .context("failed to read remote user")?;
    if inherit_env
        && !login_user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        bail!("--inherit-env requires a login name that sshd can match literally");
    }
    if remote_user == login_user {
        return Ok(());
    }
    if inherit_env {
        bail!("--inherit-env requires logging in as the user that runs kubectl exec");
    }
    let uid = kubectl::exec_capture_target(target, &["id", "-u"])
        .await
        .context("failed to read remote uid")?;
    if uid != "0" {
        bail!(
            "This Pod runs as non-root. Use the container user for login (requested: {}, required: {}).",
            login_user,
            remote_user
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
    inherited_env: &[String],
) -> Result<u16> {
    let mut command = vec!["sh", "-s", "--", base, login_user, pubkey_line];
    command.extend(inherited_env.iter().map(String::as_str));
    let script = START_SSHD_SCRIPT.as_bytes();
    let output = timeout(Duration::from_secs(40), {
        kubectl::exec_with_input_target(target, &command, script)
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
shift 3
SSHD="$BASE/bundle/sshd"

exec 3>&1
exec 1>&2

debug_log() {
  printf '[sshpod] %s\n' "$1" >&2
}

append_setenv_arg() {
  key="$1"
  value="$2"
  escaped="$(printf '%s=%s' "$key" "$value" | sed 's/[\\"]/\\&/g')"
  printf ' "%s"' "$escaped"
}

umask 077
mkdir -p "$BASE" "$BASE/logs" "$BASE/hostkeys"
chmod 700 "$BASE" "$BASE/hostkeys" "$BASE/logs"
BASE_PARENT="$(dirname "$BASE")"
TOP_DIR="$(dirname "$BASE_PARENT")"
chmod 711 "$TOP_DIR" "$BASE_PARENT"
debug_log "start script begin (base=$BASE)"

# Multiple SSH clients may start the same policy at once. Only one writes its config.
attempt=0
until mkdir "$BASE/start.lock" 2>/dev/null; do
  attempt=$((attempt+1))
  if [ "$attempt" -ge 15 ]; then
    echo "timed out waiting for sshd startup lock" >&2
    exit 1
  fi
  sleep 1
done
trap 'rmdir "$BASE/start.lock"' EXIT
trap 'exit 1' HUP INT TERM

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

# Check the identity again in the same exec call that reads the environment.
if [ "$#" -gt 0 ] && [ "$(id -un)" != "$LOGIN_USER" ]; then
  echo "--inherit-env requires logging in as the user that runs kubectl exec" >&2
  exit 1
fi

# Collect only explicit names. Keep the sentinel so command substitution does not
# discard newlines that belong to the value. Remove it and printenv's line ending.
SET_ENV=""
CR="$(printf '\r')"
for key do
  if value="$(printenv "$key" && printf '.')"; then
    value="${value%?}"
    value="${value%?}"
    case "$value" in
      *"$CR"*|*'
'*)
        printf 'cannot inherit %s: multiline values are not supported by sshd SetEnv\n' "$key" >&2
        exit 1
        ;;
    esac
    SET_ENV="$SET_ENV$(append_setenv_arg "$key" "$value")"
  fi
done

i=0
while [ $i -lt 30 ]; do
  i=$((i+1))
  PORT="$(rand_port)"

  cat > "$BASE/sshd_config" <<EOF
ListenAddress 127.0.0.1
Port $PORT
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

  if [ "$#" -gt 0 ]; then
    printf 'AllowUsers %s\n' "$LOGIN_USER" >> "$BASE/sshd_config"
    if [ -n "$SET_ENV" ]; then
      printf 'SetEnv%s\n' "$SET_ENV" >> "$BASE/sshd_config"
    fi
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
mod tests {
    use super::{parse_inherited_env, session_directory, START_SSHD_SCRIPT};

    #[test]
    fn inheritance_allows_only_explicit_non_session_names() {
        for accepted in ["PATH", "KUBERNETES_SERVICE_HOST", "KUBECONFIG", "_APP_1"] {
            assert_eq!(parse_inherited_env(accepted).unwrap(), accepted);
        }
        for rejected in [
            "",
            "A=B",
            "A B",
            "1ABC",
            "SSH_AUTH_SOCK",
            "TERM",
            "HOME",
            "USER",
            "SHELL",
            "LANG",
            "LC_ALL",
            "LD_PRELOAD",
            "BASH_ENV",
        ] {
            assert!(parse_inherited_env(rejected).is_err(), "{rejected}");
        }
    }

    #[test]
    fn different_container_instances_and_policies_do_not_share_an_sshd() {
        let env = vec!["MY_VAR".to_string()];
        let base = session_directory("pod", "main", "containerd://one", "root", &env);
        assert_ne!(
            base,
            session_directory("pod", "main", "containerd://two", "root", &env)
        );
        assert_ne!(
            base,
            session_directory("pod", "main", "containerd://one", "other", &env)
        );
        assert_ne!(
            base,
            session_directory("pod", "main", "containerd://one", "root", &[])
        );
    }

    #[test]
    fn start_script_avoids_user_home_side_effects() {
        assert!(!START_SSHD_SCRIPT.contains(".ssh/environment"));
        assert!(!START_SSHD_SCRIPT.contains("PermitUserEnvironment yes"));
        assert!(!START_SSHD_SCRIPT.contains("KUBERNETES_"));
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
