[🌐 English](README.md) | [🇯🇵 Japanese](README.ja.md)
# sshpod

`sshpod` は、手元の OpenSSH から Kubernetes Pod に SSH/SCP/SFTP で接続できるようにするツールです。`kubectl exec` で対象コンテナ内に一時的な `sshd` を配置し、`kubectl port-forward` を使って `*.sshpod` ホスト名向けの ProxyCommand でつなぎます。

## クイックスタート

### 方法1: 自動 (Linux/macOS)
```bash
curl -fsSL https://raw.githubusercontent.com/pfnet-research/sshpod/main/install.sh | sh -s -- --yes
```
デフォルトで `~/.local/bin` に最新リリースをインストールし（`--prefix` で変更可）、`--yes` 指定時は確認なしで `sshpod configure` を実行します。

### 方法1: 自動 (Windows PowerShell)
PowerShell 5+:
```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force; `
  & ([scriptblock]::create((irm https://raw.githubusercontent.com/pfnet-research/sshpod/main/install.ps1))) -Yes
```
`-Yes` を外すと `~/.ssh/config` 更新前に確認します。

### 方法2: 手動
1. リリースから OS/アーキテクチャに合うアセット（Linux/macOS は `.tar.gz`、Windows は `.zip`）をダウンロードし、PATH（例: `~/.local/bin/sshpod` または `~/.local/bin/sshpod.exe`）に置きます。
2. `sshpod configure` を実行する（`~/.ssh/config` をバックアップしつつ sshpod 用ブロックを書き換えます）、または次のブロックを自分で追加してください。バイナリの設置場所に合わせてパスを調整してください:
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

## 使い方
ProxyCommand ブロックを設定したら、`*.sshpod` ホスト名で `ssh`/`scp`/`sftp` を使えます:
```bash
ssh root@pod--api.namespace--default.context--prod.sshpod
ssh app@deployment--web.namespace--app.context--dev.sshpod
ssh app@container--sidecar.pod--debug.namespace--tools.context--dev.sshpod
scp ./local.tgz ubuntu@job--batch.namespace--etl.context--dev.sshpod:/tmp/
```
- `.sshpod` サフィックスは必須（DNS への登録は不要）。
- 対象は `pod--<pod>`、`deployment--<deployment>`、`job--<job>` のいずれかで指定します。Deployment/Job は Ready な Pod を自動で選択します。
- オプション: `container--<container>`（マルチコンテナ Pod では必須）、`namespace--<namespace>`（コンテキストに設定された namespace があればそれを、無い場合はクラスタのデフォルトを使用）、`context--<context>`（省略時は現在の `kubectl` コンテキスト）。
- Pod が非 root で動いている場合、SSH ユーザはコンテナ内の実ユーザと一致させてください。root Pod であれば任意のユーザで接続できます。

## 要件
- ローカル: 対象クラスタに到達でき、`exec`/`port-forward` が許可された `kubectl`、OpenSSH クライアント (`ssh`/`scp`/`sftp`) と `ssh-keygen`、`~/.ssh/config` と `~/.cache/sshpod` への書き込み権限。
- Pod 側: Linux `amd64` または `arm64`、`sh` が利用可能、`/tmp` が書き込み可。`xz`/`gzip` が無くてもプレーン転送にフォールバックし、同梱の `sshd` バイナリが実行できる必要があります。

## 動作概要
- `sshpod configure` は `~/.ssh/config` に `Host *.sshpod` ブロックを書き込み（タイムスタンプ付きでバックアップ作成）、ProxyCommand を `sshpod` バイナリに向けます。
- 初回接続時に `~/.cache/sshpod/id_ed25519` を作成し、Pod 内 `/tmp/sshpod/<pod-uid>/<container>` にアーキテクチャ適合の `sshd` バンドルを配置、ホスト鍵をインストールして `127.0.0.1` で起動します。
- `kubectl port-forward` でその `sshd` に接続し、`/tmp/sshpod` に残っている間は同じバンドルとホスト鍵を再利用します。

## 開発メモ
- `make install` でリリースビルド、`sshpod configure` の実行、`~/.local` へのインストールをまとめて行います。
- テストは `make test`、lint は `make lint` で実行できます。
