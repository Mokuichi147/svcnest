# svcnest

任意のローカルプログラムを、プロジェクトのディレクトリから管理する CLI です。OS に登録するのは `svcnest daemon` だけで、サービスごとの runner が対象プログラムを監督します。daemon と runner は同じ実行ファイルの内部モードです。

対応対象は macOS arm64 / x86_64、Windows 10 / 11 x64、systemd を使用する Linux x86_64 / arm64。ユーザー単位で動作します。

## インストールと基本操作

Rust stable（1.89 以上）でビルドできます。

```bash
cargo install --path . --locked

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

Windows のコンソール出力では、ドライブパスの正規化で付く `\\?\` 接頭辞を除いて表示します。共有フォルダーは `\\server\share\...` 形式で表示します。`config show` のパス項目にも適用し、保存済み設定と `status --json` / `list --json` のパスは実行用の正規化形式を保持します。

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

設定はサービスごとに TOML として保存し、一時ファイルの同期と atomic rename で更新します。Windows の予約ファイル名にも対応するため、ファイル名には `svc-` 接頭辞を付けます。Node.js ランチャーでは `resolved_executable` に Node.js、`resolved_script` に JavaScript の絶対パスを保存し、表示用 `command` は元の入力を保ちます。PowerShell スクリプトでは同じフィールドにシェルと `.ps1` の絶対パスを保存し、`interpreter_args` に起動オプション、`interpreter_environment` にシェル用の環境変数を保存します。これらのフィールドがない従来の設定も読み込めます。

## 再起動とログ

再起動ポリシーは `never`、`on-failure`（標準）、`always`。異常終了後の待機時間は 1、2、4、8、16、30 秒、その後は 30 秒です。60 秒以上の安定稼働で待機時間をリセットし、5 分間の再起動数を 10 回に制限します。上限到達時は `failed` / `restart-limit` になります。手動停止、手動再起動、daemon 停止では再起動を予約しません。

ログには時刻・タイムゾーンと stdout / stderr の区別を付けます。10 MiB を目安に対象を停止せずローテーションし、現在のファイルと 4 世代の過去ログを保持します。`logs -n` は世代をまたいで末尾を表示します。

## OS 連携と保存先

最初の `enable`、`add --enable`、または `daemon install` で daemon のユーザー向け自動起動を登録します。通常の `add` / `start` は必要に応じて daemon を background 起動します。以後のログイン時には OS が daemon を起動し、daemon が有効なサービスを開始します。

| OS | 自動起動 | 標準の保存先 |
|---|---|---|
| macOS | `~/Library/LaunchAgents/` の LaunchAgent | `~/Library/Application Support/svcnest/` |
| Linux | `systemd --user` | `$XDG_CONFIG_HOME/svcnest/` または `~/.config/svcnest/` |
| Windows | 現在のユーザーのログオン時に動く Task Scheduler | `%LOCALAPPDATA%\svcnest\` |

`--home <directory>` または `SVCNEST_HOME` で保存先を変更できます。daemon は保存先にかかわらず一人のユーザーにつき一つです。保存先を切り替えるときは、それまでの保存先の daemon を先に停止してください。OS 登録には起動した実行ファイルの絶対パスを使用するため、通常利用では `cargo install` などで固定した場所へインストールしてください。登録ファイルが残っていても OS 側の登録がなくなっていた場合は、`enable` / `daemon install` で復元します。

Unix の IPC は所有者専用のディレクトリ内にある Unix Domain Socket、Windows は現在のユーザーだけを許可する DACL を持つ Named Pipe です。ネットワークポートは開きません。daemon のロックとサービスごとのロックにより二重起動を防ぎます。daemon との制御パイプが閉じると runner は対象ツリーを停止し、停止完了までサービスのロックを保持します。

`status --json` と `list --json` は同じ v1 スキーマを使用します。定義は [schema/status-v1.json](schema/status-v1.json)、設計は [docs/architecture.md](docs/architecture.md) を参照してください。

## 開発と検証

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --test-threads=3
cargo build --locked --release
```

`cargo test` は一時ディレクトリと専用の daemon を使用し、自動起動の実機設定を変更しません。実際の子・孫プロセス、同時起動、daemon の強制終了、foreground 実行、再起動バックオフを検証します。CI は macOS arm64 / Intel、Windows x64、Ubuntu x86_64 / arm64 で同じ確認を実行します。runner のラベルは [GitHub の公式一覧](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) に基づいています。

実際の OS 連携は `python scripts/native-os-smoke.py` で検証できます。`uv` と、macOS の GUI ログイン、Linux の systemd user セッション、または Windows のログイン済みユーザー環境が必要です。専用の自動起動を一時登録し、登録の修復、自動起動、daemon のクラッシュ回復、重複防止、foreground の Ctrl+C を確認して、終了時に登録と一時ファイルを削除します。CI の Linux ではテスト基盤として user manager を先に開始します。svcnest 自体の操作は一般ユーザーで実行します。確認済みの範囲は [docs/validation.md](docs/validation.md) に記載しています。

Windows のクラッシュ検証では、旧プロセスの回収を確認してから Task Scheduler で daemon を再起動し、enabled サービスが復旧することを確認します。Task Scheduler の [RestartOnFailure](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tsch/2ff4aa5a-7bc4-449f-bbb1-27475645867f) は起動失敗の再試行を設定するもので、実行中の daemon の強制終了からの自動再起動は保証しません。

公開 JSON Schema の検証は `scripts/requirements-test.txt` の依存関係を入れた専用 Python 環境で `python scripts/check-json-schema.py --binary target/release/svcnest` を実行します。実際の stopped / running / foreground / failed / backoff の status と list 出力を検証し、テスト用 daemon を終了します。CI でも同じ検証を実行します。

`python scripts/restart-policy-smoke.py --binary target/release/svcnest` は実時間の待機間隔、60 秒の安定稼働でのリセット、10 回制限を約 3 分で確認します。Unix の端末切断は `python scripts/terminal-background-smoke.py --binary target/release/svcnest` で確認できます。Windows では `--binary target/release/svcnest.exe` を指定します。各検証は独立した一時保存先を使い、テスト用 daemon を終了します。daemon がユーザー単位で一つのため、既存の daemon を停止した状態で、一つずつ実行してください。
