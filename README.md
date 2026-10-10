# svcnest

任意のローカルプログラムを、プロジェクトのディレクトリから管理する CLI です。OS に登録するのは `svcnest daemon` だけで、サービスごとの runner が対象プログラムを監督します。daemon と runner は同じ実行ファイルの内部モードです。

対応対象は macOS arm64 / x86_64、Windows 10 / 11 x64、systemd を使用する Linux x86_64 / arm64。ユーザー単位で動作します。

## インストールと基本操作

macOS / Linux では、公開済みの最新安定版を次のコマンドでインストールできます。Rust と管理者権限は不要です。

```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/Mokuichi147/svcnest/main/install.sh | sh
```

OS と CPU（arm64 / x86_64）を自動判定し、GitHub Releases のバイナリを SHA-256 で検証して `~/.local/bin/svcnest` に配置します。PATH に配置先がない場合は、使用しているシェルの起動設定へ自動追加します。手動編集は不要で、インストール後に新しいターミナルを開くと `svcnest` を使えます。すでに PATH に含まれる場合は設定を変更せず、再インストールでも同じ設定を重複して追加しません。

zsh は `${ZDOTDIR:-$HOME}/.zshrc`、bash は読み込まれるログイン設定（`.bash_profile` / `.bash_login` / `.profile`）と `.bashrc`、fish は `${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish`、sh は `.profile` を設定します。設定ファイルを指定する場合は `SVCNEST_PROFILE`、自動設定を無効にする場合は `--no-modify-path` を指定できます。

Linux 版は musl を使った静的リンクです。自動起動には systemd のユーザーセッションが必要です。

バージョンや配置先を指定する場合は、`sh -s --` に引数を渡せます。配置先は `SVCNEST_INSTALL_DIR` 環境変数でも指定できます。

```sh
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/Mokuichi147/svcnest/main/install.sh \
  | sh -s -- --version 1.0.0 --install-dir "$HOME/.local/bin"
```

更新も同じコマンドで行います。以前 `cargo install` を使用していた場合は `--install-dir "$HOME/.cargo/bin"` を指定して、daemon の登録済みパスを保ってください。取得・検証・実行確認に失敗した場合は、既存のバイナリを保持します。

常駐中の更新については、後述の「macOS / Linux / Windows での常駐中の更新」を参照してください。

Windows x64 では、Windows PowerShell 5.1 または PowerShell 7 で次のコマンドを実行します。Rust と管理者権限は不要です。

```powershell
irm https://raw.githubusercontent.com/Mokuichi147/svcnest/main/install.ps1 | iex
```

公開済みの最新安定版の ZIP と SHA-256 を取得・検証し、`%LOCALAPPDATA%\Programs\svcnest\svcnest.exe` へ配置します。ユーザー PATH と実行中の PowerShell の PATH へ重複なく自動追加するため、そのまま `svcnest --version` を実行できます。更新も同じコマンドです。`run` / `logs -f` など、インストール先の exe を使う長時間の CLI は更新前に終了してください。

バージョンや配置先を指定する場合は、取得したスクリプトに引数を渡します。配置先は `SVCNEST_INSTALL_DIR`、PATH の自動設定の無効化は `-NoModifyPath` でも指定できます。

```powershell
$installer = irm https://raw.githubusercontent.com/Mokuichi147/svcnest/main/install.ps1
& ([scriptblock]::Create($installer)) -Version 1.0.0 -InstallDir "$env:LOCALAPPDATA\Programs\svcnest"
```

手動で配置する場合は、[GitHub Releases](https://github.com/Mokuichi147/svcnest/releases) の `svcnest-x86_64-pc-windows-msvc.zip` を使えます。

ソースからビルドする場合は、Rust stable（1.89 以上）が必要です。リポジトリを取得し、そのディレクトリでインストールします。

```sh
git clone https://github.com/Mokuichi147/svcnest.git
cd svcnest
cargo install --path . --locked
```

インストール後は、プロジェクトのディレクトリから使います。

```bash
cd ~/projects/api
svcnest add api -- uv run main.py --port 8000
svcnest enable --now

svcnest status
svcnest logs -f
svcnest restart

svcnest stop
svcnest run
```

登録時にカレントディレクトリを正規化し、実行ファイルを PATH から絶対パスへ解決します。`./target/release/myproject` のような入力はサービスの working directory を基準に解決します。表示用の元のコマンドと、実行用の絶対パスを分けて保存します。

引数は argv 配列として実行します。パイプやリダイレクトが必要な場合は、`sh -lc '...'`、`powershell -Command '...'` などを明示的に登録してください。Windows の標準 npm / npx / Node.js 用ランチャーは、参照先の Node.js と JavaScript を絶対パスへ解決するため、`svcnest add mcp -- npx some-mcp-server` と登録できます。これらのランチャーは Node.js を直接起動します。

Windows の `.bat` / `.cmd` / `.ps1` は、普段実行するスクリプトをそのまま登録できます。

```powershell
svcnest add app -- .\start.bat
svcnest add worker -- .\start.ps1 -Port 8000
svcnest start app
svcnest start worker

# PowerShell を明示指定する場合
svcnest add worker7 --shell pwsh -- .\start.ps1
```

一般のバッチは Windows 標準の `cmd.exe` で起動し、引数のエスケープは Rust 標準ライブラリへ委ねます。`.ps1` は登録元の直近のシェルが PowerShell ならその実行ファイルを選び、それ以外は登録時の PATH の `pwsh.exe`、`powershell.exe`、Windows 標準の PowerShell の順に探します。`--shell` は `.ps1` 用で、`pwsh` / `powershell` またはその実行ファイルのパスを指定できます。拡張子は大文字・小文字を区別しません。

スクリプトの絶対パスと、PowerShell の実行ファイル・起動オプションを保存するため、起動時のカレントディレクトリや daemon の PATH には依存しません。PowerShell は [`-NoLogo -NoProfile -File`](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.core/about/about_powershell_exe?view=powershell-5.1) で起動します。登録元と同じ PowerShell を使う場合は `PSModulePath` を保存し、セッション限定の `PSExecutionPolicyPreference` も保存します。`--env` / env-file の同名設定が優先します。別バージョンのシェルを選ぶ場合は、そのシェルの既定モジュールパスを使います。daemon のモジュールパスやセッション限定ポリシーは引き継ぎません。プロファイル、関数、セッション内の変数は再現しません。登録時はスクリプトを実行せず、起動時に指定された引数を渡します。

サービス用バッチでは、対象プログラムの直後に終了コードを保存し、バッチ全体の終了コードとして返してください。後続の `pause` で終了コードが `0` に変わる場合があります。background 実行の標準入力は接続されていないため、`pause` はユーザーの入力待ちとして使えません。例えば、次のように記述します。

```bat
@echo off
python server.py
set "SERVICE_EXIT_CODE=%ERRORLEVEL%"
exit /b %SERVICE_EXIT_CODE%
```

`svcnest` が監視するのはバッチ全体の終了コードです。対象がエラーになってもバッチが `0` を返すと、既定の `on-failure` では再起動しません。

## サービスの選択

サービス名を指定すると、ディレクトリに関係なくそのサービスを操作します。省略すると、現在のディレクトリから親を順番に探索して、最初に一致した working directory を使います。子ディレクトリからの操作やシンボリックリンク経由の操作にも対応します。

同じディレクトリに複数サービスがある場合は、名前の明示が必要です。`--all` は現在のディレクトリまたは最初に一致する親ディレクトリのサービス全件を選びます。

```bash
svcnest add api -- uv run api.py
svcnest add worker -- uv run worker.py
svcnest start api
svcnest start --all
svcnest status --all
```

`list` は常に全登録サービスを表示します。`run`、`logs`、`config show`、`config path` は一つのサービスを選択します。

## コマンド

| コマンド | 動作 |
|---|---|
| `add <name> -- <command> [args...]` | コマンドを登録 |
| `start [name] [--all]` | daemon 管理で background 起動。起動済みなら成功 |
| `stop [name] [--all]` | 子・孫を含めて停止。再起動ポリシーは発火しない |
| `restart [name] [--all]` | 旧プロセスツリーの停止完了後に起動 |
| `enable [name] [--all] [--now]` | daemon 起動時の自動起動を有効化。`--now` で即時起動 |
| `disable [name] [--all] [--now]` | 自動起動を無効化。`--now` で停止も実行 |
| `run [name]` | foreground 実行。標準入出力を端末に接続し、Ctrl+C を対象へ送信 |
| `status [name] [--all] [--json]` | 状態、PID、コマンド、working directory、稼働時間などを表示 |
| `list [--json]` | 全サービスを表示 |
| `logs [name] [-n 100] [-f]` | 保存ログを表示。follow の Ctrl+C はサービスを停止しない |
| `remove [name] [--all] [--stop] [--purge]` | 登録を削除。起動中は `--stop` が必要。`--purge` でログも削除 |
| `config show [name] [--show-secrets]` | 設定を表示。保存した環境変数は PATH 以外を標準でマスク |
| `config path [name]` | 設定ファイルのパスを表示 |
| `doctor [--json]` | daemon、IPC、OS 登録、設定、実行ファイル、ディレクトリ、env-file を診断 |
| `daemon install [--dry-run]` | OS 自動起動へ daemon を登録。`--dry-run` は登録内容の表示のみ |
| `daemon start / stop / status / uninstall` | daemon 自体の操作 |

`run` と background 起動は、同じサービスのロックを共有します。起動中の background サービスに対する `run`、foreground 実行中の `start` は拒否します。`run` に再起動ポリシーは適用しません。対象プログラムの終了コードを返し、割り込みを検出した場合は 130 を返します。Unix では端末制御を対象へ移譲して対話入力も受け付け、終了後に元の制御を戻します。

## 登録オプションと環境変数

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

`--enable` で将来の daemon 起動時の自動起動を有効にして登録できます。`--replace` は停止済みの同名サービスを上書きします。名前は `[a-z0-9][a-z0-9._-]{0,63}` です。`--stop-timeout` は `1ms` から `300s`、単位なしの場合は秒として解釈します。

登録時に自動保存する環境変数は PATH と、`.ps1` の場合のシェル用 `PSModulePath` / `PSExecutionPolicyPreference` です。それ以外は明示した `--env` の値と env-file のパスを保存します。env-file の値は起動のたびに読み込み、設定ファイルへコピーしません。優先順位は `--env`、env-file、保存したシェル環境、親プロセスの環境の順で、登録した PATH は env-file より優先します。

## 再起動とログ

再起動ポリシーは `never`、`on-failure`（標準）、`always`。異常終了後の待機時間は 1、2、4、8、16、30 秒、その後は 30 秒です。60 秒以上の安定稼働で待機時間をリセットし、5 分間の再起動数を 10 回に制限します。上限到達時は `failed` / `restart-limit` になります。手動停止、手動再起動、daemon 停止では再起動を予約しません。

`enabled` は daemon 起動時の自動起動を指定します。稼働中の再起動は `restart` で決まり、`enabled=yes` でも正常終了した `on-failure` のサービスは停止状態になります。`list` の `LAST EXIT` と、`status` の `Last exit` で直近の終了コードを確認できます。Unix のシグナル終了は `signal:<番号>`、終了履歴がなければ `-` を表示します。

ログには時刻・タイムゾーンと stdout / stderr の区別を付けます。`svcnest` の行には起動 PID、終了コード・シグナル・稼働時間、停止理由、再起動の待機時間も記録します。10 MiB を目安に対象を停止せずローテーションし、現在のファイルと 4 世代の過去ログを保持します。`logs -n` は世代をまたいで末尾を表示します。

稼働中の対象には実行時間の上限を設けません。`--stop-timeout` は停止を要求した後の待機時間で、時間のかかるバッチも終了または停止要求まで監視します。`running` はプロセスの起動を表し、HTTP などの応答準備の完了を保証しません。対象のログとヘルスチェックで準備状態を確認してください。`status` / `list` は `svcnest` が管理する実行の状態を表示し、管理外で起動した同じプログラムを自動的に取り込むことはありません。

## OS 連携と保存先

最初の `enable`、`add --enable`、または `daemon install` で daemon のユーザー向け自動起動を登録します。通常の `add` / `start` は必要に応じて daemon を background 起動します。以後のログイン時には OS が daemon を起動し、daemon が有効なサービスを開始します。

| OS | 自動起動 | 標準の保存先 |
|---|---|---|
| macOS | `~/Library/LaunchAgents/` の LaunchAgent | `~/Library/Application Support/svcnest/` |
| Linux | `systemd --user` | `$XDG_CONFIG_HOME/svcnest/` または `~/.config/svcnest/` |
| Windows | 現在のユーザーのログオン時に動く Task Scheduler | `%LOCALAPPDATA%\svcnest\` |

`--home <directory>` または `SVCNEST_HOME` で保存先を変更できます。daemon は保存先にかかわらず一人のユーザーにつき一つです。保存先を切り替えるときは、それまでの保存先の daemon を先に停止してください。OS 登録には起動した実行ファイルの絶対パスを使用するため、通常利用ではインストーラーや `cargo install` で固定した場所へインストールしてください。登録ファイルが残っていても OS 側の登録がなくなっていた場合は、`enable` / `daemon install` で復元します。

daemon が異常終了した場合は、サービスの子・孫プロセスも停止します。

### macOS / Linux / Windows での常駐中の更新

daemon とサービスを動かしたまま、同じ配置先へインストーラーを再実行して更新できます。ソースからビルドする場合は、リポジトリのディレクトリで `cargo install --path . --locked` を実行します。

稼働中の daemon とサービスは更新前のバイナリで動き続け、新しい daemon は次回起動時に使います。更新のたびに自動起動を登録し直す必要はありません。

新しい daemon をすぐに使う場合は、daemon とサービスを一度停止してから更新します。旧ビルドからの移行も含め、次の手順をリポジトリのディレクトリで実行してください。

```sh
svcnest daemon stop
cargo install --path . --locked
svcnest daemon install
```

`daemon install` は自動起動を登録して daemon を開始します。Windows ではインストール先の CLI で `run` や `logs -f` を実行中の場合、その長時間の CLI コマンドも更新先の exe を使用するため、更新前に終了してください。

`status --json` と `list --json` は同じ v1 スキーマを使用します。定義は [schema/status-v1.json](schema/status-v1.json)、設計は [docs/architecture.md](docs/architecture.md) を参照してください。

## 開発と検証

### リリースの公開

公開対象のコミットで `Cargo.toml` / `Cargo.lock` のバージョンを揃え、同じバージョンの `v<version>` タグを push すると、GitHub Actions がそのコミットをビルドして GitHub Releases へ公開します。例えば 1.0.0 の公開は次の操作です。

```sh
git tag -a v1.0.0 -m 'v1.0.0 を公開'
git push origin v1.0.0
```

タグを付けるコミットには `.github/workflows/release.yml` とリリース用スクリプトを含めてください。README のインストールコマンドは `main/install.sh` / `main/install.ps1` を取得するため、初回公開時は両方のインストーラーも `main` へ反映します。

リリースノートには、前回の公開リリースから追加されたコミットの件名・リンクと、差分全体へのリンクを載せます。PR を使わずに直接コミットした変更も含みます。安定版は同じ履歴上の直前の安定版、prerelease は直前の公開版（prerelease を含む）と比較します。初回は全コミットの一覧を載せます。

配布対象は [.github/release-targets.json](.github/release-targets.json) で定義し、ビルド、梱包、公開前の検証、リリースノートのダウンロード一覧で共有します。

| 環境 | 配布物 |
|---|---|
| macOS arm64 | `svcnest-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `svcnest-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `svcnest-x86_64-unknown-linux-musl.tar.gz` |
| Linux arm64 | `svcnest-aarch64-unknown-linux-musl.tar.gz` |
| Windows x64 | `svcnest-x86_64-pc-windows-msvc.zip` |

各アーカイブには実行ファイルと LICENSE を含めます。個別の `.sha256` と全配布物の `SHA256SUMS` も添付します。リリースは、タグのコミットにある `ci.yml` を `workflow_call` で呼び出し、5 環境の CI 全項目（fmt、Clippy、全テスト、JSON Schema、再起動・端末切断、OS 連携、常駐中更新、インストーラー検証など）が成功することを必須にしています。その後に配布用ビルドと検証を実行し、全配布物の存在と SHA-256 を確認してから draft を作成し、アップロードが完了してから公開します。CI・ビルド・配布物の検証が失敗・キャンセルした場合は公開しません。途中のアップロード失敗は draft のまま残り、Actions を再実行できます。公開済みのリリースは上書きしません。

複数のタグのビルドは並列に進め、公開処理は [GitHub Actions の `queue: max`](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency) を使って順番に処理します。

`v1.1.0-rc.1` のようなタグは prerelease とし、引数なしのインストーラーは安定版を取得します。過去の安定版向けの修正版を公開しても、それより新しい安定版の Latest は維持します。初回リリースの公開までは、上記のインストールコマンドからバイナリを取得できません。

### ローカルでの確認

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
python3 scripts/check-release.py
```

テストは CI と同じ並列数で実行します。

- macOS / Linux: `cargo test --locked --all-targets -- --test-threads=3`
- Windows: `cargo test --locked --all-targets -- --test-threads=1`

インストーラーの検証は macOS / Linux で実行します。通信を一時ディレクトリ内のリリース資材に置き換え、OS / CPU の選択、パイプからの実行、更新、検証失敗時の既存バイナリの保持、実際の release バイナリの配置と、PATH の自動設定・重複防止を確認します。シェル設定の検証も専用の一時ファイルを使用します。

```sh
sh -n install.sh
python3 scripts/check-installer.py --binary target/release/svcnest
```

Windows のインストーラーは、PowerShell で `./scripts/check-windows-installer.ps1 -Binary target/release/svcnest.exe` を実行して検証します。通信とユーザー PATH の保存を模擬処理に置き換え、実ユーザーのレジストリを変更せずに検証します。Windows CI では PowerShell 7 と Windows PowerShell 5.1 の両方で実行します。

`cargo test` は一時ディレクトリと専用の daemon を使用し、自動起動の実機設定を変更しません。実際の子・孫プロセス、同時起動、daemon の強制終了、foreground 実行、再起動バックオフを検証します。CI は macOS arm64 / Intel、Windows x64、Ubuntu x86_64 / arm64 で同じ確認を実行します。runner のラベルは [GitHub の公式一覧](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) に基づいています。

実際の OS 連携は `python scripts/native-os-smoke.py` で検証できます。`uv` と、macOS の GUI ログイン、Linux の systemd user セッション、または Windows のログイン済みユーザー環境が必要です。専用の自動起動を一時登録し、登録の修復、自動起動、daemon のクラッシュ回復、重複防止、foreground の Ctrl+C を確認して、終了時に登録と一時ファイルを削除します。CI の Linux ではテスト基盤として user manager を先に開始します。svcnest 自体の操作は一般ユーザーで実行します。確認済みの範囲は [docs/validation.md](docs/validation.md) に記載しています。

Windows のクラッシュ検証では、旧プロセスの回収を確認してから Task Scheduler で daemon を再起動し、enabled サービスが復旧することを確認します。Task Scheduler の [RestartOnFailure](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tsch/2ff4aa5a-7bc4-449f-bbb1-27475645867f) は起動失敗の再試行を設定するもので、実行中の daemon の強制終了からの自動再起動は保証しません。

公開 JSON Schema の検証は `scripts/requirements-test.txt` の依存関係を入れた専用 Python 環境で `python scripts/check-json-schema.py --binary target/release/svcnest` を実行します。実際の stopped / running / foreground / failed / backoff の status と list 出力を検証し、テスト用 daemon を終了します。CI でも同じ検証を実行します。

`python scripts/restart-policy-smoke.py --binary target/release/svcnest` は実時間の待機間隔、60 秒の安定稼働でのリセット、10 回制限を約 3 分で確認します。Unix の端末切断は `python scripts/terminal-background-smoke.py --binary target/release/svcnest` で確認できます。Windows では `--binary target/release/svcnest.exe` を指定します。各検証は独立した一時保存先を使い、テスト用 daemon を終了します。daemon がユーザー単位で一つのため、既存の daemon を停止した状態で、一つずつ実行してください。
