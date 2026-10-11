# svcnest

任意のローカルプログラムを、プロジェクトのディレクトリから常駐サービスとして管理する CLI です。起動・停止・自動再起動・ログ保存・ログイン時の自動起動を、ユーザー権限だけで扱えます。

対応 OS: macOS (arm64 / x86_64)、Windows 10 / 11 (x64)、systemd を使用する Linux (x86_64 / arm64)

## インストール

macOS / Linux（Rust と管理者権限は不要）:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/Mokuichi147/svcnest/main/install.sh | sh
```

Windows（PowerShell）:

```powershell
irm https://raw.githubusercontent.com/Mokuichi147/svcnest/main/install.ps1 | iex
```

PATH は自動で設定します。macOS / Linux では、インストール後に新しいターミナルを開いてください。

## クイックスタート

```bash
cd ~/projects/api
svcnest add api -- uv run main.py --port 8000   # 登録
svcnest enable --now                            # 自動起動を有効にして起動

svcnest status      # 状態を確認
svcnest logs -f     # ログを追跡
svcnest restart     # 再起動
svcnest stop        # 停止
svcnest run         # foreground で実行（Ctrl+C で終了）
```

`--` 以降のコマンドは、登録時のディレクトリと PATH を基準に絶対パスへ解決して保存します。そのため、起動時のカレントディレクトリや daemon の PATH には依存しません。

コマンドと引数はシェルを介さずに渡します。macOS / Linux でシェルから読み込むのは環境変数だけです（[シェルの環境](#シェルの環境)）。パイプやリダイレクトを使う場合は `sh -lc '...'` や `powershell -Command '...'` のように明示して登録してください。

## サービスの選択

サービス名を指定すると、どのディレクトリからでもそのサービスを操作できます。省略した場合は、現在のディレクトリから親へ向かって探索し、最初に見つかったディレクトリのサービスを対象にします。

同じディレクトリに複数のサービスがある場合は、名前を指定するか `--all` で全件を選びます。

```bash
svcnest add api -- uv run api.py
svcnest add worker -- uv run worker.py
svcnest start api
svcnest status --all
```

## コマンド

| コマンド | 動作 |
|---|---|
| `add <name> -- <command> [args...]` | サービスを登録 |
| `start [name] [--all]` | background で起動 |
| `stop [name] [--all]` | 子・孫プロセスを含めて停止 |
| `restart [name] [--all]` | 停止の完了を待ってから起動 |
| `enable [name] [--all] [--now]` | ログイン時の自動起動を有効化。`--now` で即時起動 |
| `disable [name] [--all] [--now]` | 自動起動を無効化。`--now` で停止も実行 |
| `run [name]` | foreground で実行。標準入出力を端末に接続 |
| `status [name] [--all] [--json]` | 状態、PID、コマンド、稼働時間などを表示 |
| `list [--json]` | 全サービスを表示 |
| `logs [name] [-n 100] [-f]` | ログを表示。`-f` の Ctrl+C はサービスを停止しない |
| `remove [name] [--all] [--stop] [--purge]` | 登録を削除。起動中は `--stop` が必要。`--purge` でログも削除 |
| `config show [name] [--show-secrets]` | 設定を表示。環境変数は PATH 以外をマスク |
| `config path [name]` | 設定ファイルのパスを表示 |
| `doctor [--json]` | daemon、自動起動、設定、実行ファイル、シェルの環境などを診断 |
| `daemon install [--dry-run]` | daemon をログイン時の自動起動へ登録 |
| `daemon start / stop / status / uninstall` | daemon 自体の操作 |

`run` は background 起動と同時には実行できません。再起動ポリシーは適用せず、対象の終了コード（Ctrl+C で中断した場合は 130）を返します。

`status --json` / `list --json` の形式は [schema/status-v1.json](schema/status-v1.json) で定義しています。

## 登録オプション

```bash
svcnest add api \
  --cwd ~/projects/api \
  --restart on-failure \
  --stop-timeout 10s \
  --description 'Local API' \
  --env MODE=production \
  --env-file .env \
  -- uv run main.py
```

| オプション | 内容 |
|---|---|
| `--cwd <dir>` | working directory（標準は現在のディレクトリ） |
| `--restart <policy>` | `never` / `on-failure`（標準）/ `always` |
| `--stop-timeout <time>` | 停止要求から強制終了までの待機時間。`1ms`〜`300s`、単位なしは秒 |
| `--env KEY=VALUE` | 環境変数を保存 |
| `--env-file <path>` | 起動のたびに読み込む env-file（値は設定へコピーしない） |
| `--no-shell-env` | シェルの login / rc ファイルが export する環境を読み込まない（macOS / Linux） |
| `--enable` | 登録と同時に自動起動を有効化 |
| `--replace` | 停止中の同名サービスを上書き |

サービス名は `[a-z0-9][a-z0-9._-]{0,63}` です。環境変数は `--env`、env-file、シェルの環境、daemon の環境の順に優先します。登録時の PATH は自動で保存され、env-file やシェルの環境より優先します。

### シェルの環境

macOS / Linux では、端末から起動した場合と同じ環境になるよう、起動のたびにシェルの login / rc ファイルを読み込み、export された環境変数をサービスへ渡します。rc ファイルで別のファイルから読み込む秘密情報も、daemon から起動したサービスへ渡ります。値は設定へ保存せず、再起動のたびに読み直します。

使用するシェルは、登録した端末で動いている対話シェル、passwd のログインシェル、`SHELL`、`/bin/sh` の順に選びます。別のシェルの環境で動かしたい場合は、そのシェルから登録するか、設定ファイル（`svcnest config path` で確認）の `shell` に絶対パスを指定します。シェルごとに次のファイルを読み込みます。

| シェル | 起動方法 | 主に読み込むファイル |
|---|---|---|
| bash | `bash -l` から `bash -i` を実行 | `/etc/profile`、`~/.bash_profile` または `~/.profile`、`~/.bashrc` |
| zsh | `zsh -i -l` | `.zshenv`、`.zprofile`、`.zshrc`、`.zlogin` |
| fish | `fish -i -l` | `config.fish`、`conf.d` |
| sh / dash / ksh など | `-i -l` | `/etc/profile`、`~/.profile`、`$ENV` |
| csh / tcsh | `-l`（コマンドは stdin） | `.cshrc` / `.tcshrc`、`.login` |
| PowerShell | `pwsh -Login -Command` | `/etc/profile`、`~/.profile`、`$PROFILE` |

その他のシェルは `-i -l -c` で起動します。tmux や screen はシェルとして扱いません。

- rc ファイルの出力は破棄し、ログへ記録しません。
- 10 秒以内に環境を取得できない場合、またはシェルが途中で終了した場合は起動に失敗します。
- `svcnest doctor` の `shell-env` で、使用するシェルと取得できるかを確認できます。
- 不要な場合は `--no-shell-env` で登録するか、設定ファイルの `shell_env` を `false` にします。

bash や zsh などは対話モード（`-i`）で起動するため、`[[ $- == *i* ]]` などの対話判定は真になります。入力待ちや `exec tmux` など端末を前提とする処理は、`[ -t 1 ]` で端末に接続されている場合だけ実行してください。

```sh
# ~/.zshrc
[ -t 1 ] && [ -z "$TMUX" ] && exec tmux
```

## 再起動とログ

- 異常終了時は 1、2、4、8、16、30 秒と間隔を延ばして再起動します。60 秒以上安定して動けば間隔をリセットします。
- 5 分間に 10 回再起動すると `failed`（`restart-limit`）で止まります。
- `stop`・`restart`・daemon の停止では自動再起動しません。
- `enable` はログイン時に起動するかどうかの設定です。`on-failure` のサービスが正常終了した場合は、`enable` 済みでも停止したままになります。
- 直近の終了コードは `status` の `Last exit` と `list` の `LAST EXIT` で確認できます。
- `running` はプロセスが起動したことを表し、HTTP などの応答準備の完了は保証しません。

ログには時刻と stdout / stderr の区別、起動・終了・再起動の記録が残ります。10 MiB ごとにローテーションし、現在のファイルと過去 4 世代を保持します。

## Windows のスクリプト

`.bat` / `.cmd` / `.ps1` は、普段実行するスクリプトをそのまま登録できます。

```powershell
svcnest add app -- .\start.bat
svcnest add worker -- .\start.ps1 -Port 8000
```

- `.bat` / `.cmd` は `cmd.exe` で起動します。
- `.ps1` は登録元のシェルが PowerShell ならそれを使い、それ以外は登録時の PATH から `pwsh.exe`、`powershell.exe` の順に探し、見つからなければ Windows 標準の PowerShell を使います。特定の PowerShell で動かしたい場合は、その PowerShell から登録してください。プロファイルは読み込みません（`-NoProfile -File`）。
- npm / npx などの Node.js ランチャーも `svcnest add mcp -- npx some-mcp-server` のように登録できます。

svcnest はバッチ全体の終了コードで失敗を判定します。対象の終了コードをそのまま返し、`pause` は使わないでください（background では入力を受け付けず、終了コードも `0` に変わる場合があります）。

```bat
@echo off
python server.py
exit /b %ERRORLEVEL%
```

## 自動起動と保存先

OS に登録するのは svcnest の daemon だけです。最初の `enable`、`add --enable`、`daemon install` で daemon をログイン時の自動起動へ登録し、daemon が有効なサービスを起動します。

| OS | 自動起動 | 保存先 |
|---|---|---|
| macOS | LaunchAgent | `~/Library/Application Support/svcnest/` |
| Linux | `systemd --user` | `$XDG_CONFIG_HOME/svcnest/` または `~/.config/svcnest/` |
| Windows | Task Scheduler | `%LOCALAPPDATA%\svcnest\` |

保存先は `--home <dir>` または `SVCNEST_HOME` で変更できます。daemon はユーザーごとに一つだけ動くため、保存先を切り替える前に既存の daemon を停止してください。

自動起動の登録が失われた場合は、`enable` または `daemon install` で復元できます。

## 更新

インストール時と同じコマンドを再実行します。daemon とサービスは動かしたままで構いません。稼働中のものは更新前のバイナリで動き続け、次に daemon を起動したときから新しいバージョンになります。

新しいバージョンをすぐに使う場合は、daemon を停止してから更新します。

```sh
svcnest daemon stop
# インストールコマンドを再実行
svcnest daemon install
```

Windows では、更新前に `run` や `logs -f` などの実行中の svcnest コマンドを終了してください。

## 開発者向け

ビルドとテストの手順は [docs/development.md](docs/development.md)、内部設計は [docs/architecture.md](docs/architecture.md) を参照してください。
