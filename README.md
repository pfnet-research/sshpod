[🌐 English](README.md) | [🇯🇵 Japanese](README.ja.md)

# sshpod

`sshpod` makes Kubernetes Pods reachable from your existing OpenSSH client. It spins up a short-lived `sshd` inside the target container via `kubectl exec`, then connects to it through `kubectl port-forward` using `*.sshpod` hostnames defined in your SSH config.

## Quick start

### Automatic install (Linux / Apple Silicon macOS)
```bash
curl -fsSL https://raw.githubusercontent.com/pfnet-research/sshpod/main/install.sh | sh -s -- --yes
```
Installs the latest release to `~/.local/bin` (override with `--prefix`) and runs `sshpod configure` without prompting when `--yes` is supplied.

### Automatic install (Windows PowerShell)
PowerShell 5+:
```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force; `
  & ([scriptblock]::create((irm https://raw.githubusercontent.com/pfnet-research/sshpod/main/install.ps1))) -Yes
```
Remove `-Yes` to be prompted before updating `~/.ssh/config`.

### Manual install
1. Download the release asset for your OS/arch (`.tar.gz` for Linux or Apple Silicon macOS, `.zip` for Windows) and place the binary on your PATH (for example `~/.local/bin/sshpod` or `~/.local/bin/sshpod.exe`).
2. Run `sshpod configure` (backs up `~/.ssh/config` and rewrites the sshpod block), or add the block below yourself—adjust the path if you installed elsewhere:
```sshconfig
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
```

## Usage
With the ProxyCommand block in place, use `ssh`, `scp`, or `sftp` against `*.sshpod` hostnames:
```bash
ssh root@pod--api.namespace--default.context--prod.sshpod
ssh app@deployment--web.namespace--app.context--dev.sshpod
ssh app@container--sidecar.pod--debug.namespace--tools.context--dev.sshpod
scp ./local.tgz ubuntu@job--batch.namespace--etl.context--dev.sshpod:/tmp/
```
- `.sshpod` suffix is required; no DNS entry is needed.
- Targets: `pod--<pod>`, `deployment--<deployment>`, `job--<job>`; deployments/jobs pick a ready Pod automatically.
- Optional pieces: `container--<container>` (required for multi-container Pods), `namespace--<namespace>` (falls back to the namespace set on the context, otherwise the cluster default), `context--<context>` (defaults to your current `kubectl` context).
- Pods running as non-root require you to SSH as that user; root Pods accept any SSH user.
- Remote port forwarding honors the bind address requested by the client. For example, `ssh -R 0.0.0.0:18000:127.0.0.1:8000 ...sshpod` exposes port `18000` on the Pod network, while omitting the bind address keeps OpenSSH's loopback-only default.

### Session environment

SSH sessions start with the environment that `kubectl exec` provides in the
target container, including variables from the image and the Pod spec. sshd
still sets the variables that belong to the login, such as `HOME`, `USER`,
`SHELL`, `TERM`, and `SSH_*`. A value that spans multiple lines cannot be
passed through sshd and is left out.

## Requirements
- Local: `kubectl` configured for the target cluster with permission to `exec` and `port-forward`; OpenSSH client tools (`ssh`/`scp`/`sftp`) and `ssh-keygen`; ability to write to `~/.ssh/config` and `~/.cache/sshpod`.
- In the container: Linux `amd64` or `arm64`; `sh` and `cat` available; `/tmp` writable; the bundled `sshd` binaries must be allowed to run. If `tar` plus `xz` or `gzip` are available, sshpod uses them for faster bundle extraction, but they are optional.

## How it works
- `sshpod configure` writes a `Host *.sshpod` block into `~/.ssh/config` with a timestamped backup, pointing ProxyCommand at the `sshpod` binary.
- On first connect, sshpod creates `~/.cache/sshpod/id_ed25519`, uploads an architecture-matched `sshd` bundle to `/tmp/sshpod/<pod-uid>/<container>`, installs host keys, and starts the daemon on `127.0.0.1`.
- A `kubectl port-forward` connects your local SSH client to that in-pod `sshd`; subsequent connections reuse the bundle and host keys while they remain in `/tmp/sshpod`.

## Development
- `make install` builds the release binary, runs `sshpod configure`, and installs under `~/.local`.
- `make test` and `make lint` run the test and lint suites.
- To release, bump `version` in `Cargo.toml` (and `Cargo.lock`) on `main`, then run the `Release` workflow manually on `main`. The workflow reads the version from `Cargo.toml`, checks that all five archives report it with `--version`, and publishes `v<version>` with `SHA256SUMS`, tagging the commit it built. It stops if that tag already exists.
- The `sshd` bundle in a container is replaced only when the bundled binaries change; a new sshpod version alone does not reinstall it.
