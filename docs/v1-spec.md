# svcnest v1 Goal

## 1. 目的

`svcnest` は、macOS / Windows / Linux 上で任意のローカルプログラムを、OS固有のサービス管理機構を意識せず登録・起動・停止・自動起動・監視できるクロスプラットフォームCLIツールである。

主な対象は、自分で作成した以下のようなプログラム。

```text
uv run main.py --port 8000
./target/release/myproject serve
node server.js
npx some-mcp-server
python bot.py
```

ユーザーは `systemd`、`launchd`、Task Scheduler、実行ファイルの絶対パスなどを普段意識しなくてよい。

最重要UXは以下。

```bash
cd ~/projects/api

svcnest add api -- uv run main.py --port 8000
svcnest enable --now
```

以降は同じプロジェクト内で、

```bash
svcnest status
svcnest restart
svcnest logs -f
```

のようにサービス名を指定せず操作できること。

---

# 2. 対応環境

v1正式対応:

```text
macOS arm64
macOS x86_64

Windows 10 / 11 x64

Linux x86_64
Linux arm64
```

Linuxはsystemd環境を正式対象とする。

v1ではroot/system-wide serviceは扱わず、ユーザー単位のサービス管理に限定する。

---

# 3. 基本アーキテクチャ

管理対象プログラムをそれぞれOSのサービスとして登録してはならない。

OSへ登録するのは `svcnest daemon` のみ。

```text
OS
│
├─ launchd
├─ systemd --user
└─ Task Scheduler
       │
       ▼
 svcnest daemon
       │
       ├─ runner ─ target process
       ├─ runner ─ target process
       └─ runner ─ target process
```

構成:

```text
svcnest CLI
      │
      │ local IPC
      ▼
svcnest daemon
      │
      ▼
svcnest runner
      │
      ▼
target process
```

`daemon` と `runner` は同じ `svcnest` executableの内部モードとして実装し、別バイナリにはしない。

---

# 4. サービス登録

基本形:

```bash
svcnest add <name> -- <command> [args...]
```

例:

```bash
cd ~/projects/api

svcnest add api -- uv run main.py --port 8000
```

この場合、ユーザーが明示しなくても以下を自動取得する。

```text
name:
api

cwd:
/Users/user/projects/api

command:
uv run main.py --port 8000

resolved executable:
/Users/user/.local/bin/uv
```

---

# 5. cwdの自動登録

`--cwd` が指定されていない場合、

```text
svcnest add
```

を実行した時点のカレントディレクトリをserviceのworking directoryとして登録する。

例:

```bash
cd ~/src/my-api
svcnest add api -- uv run main.py
```

なら、

```text
cwd = /Users/user/src/my-api
```

として保存する。

相対パスや `..` は正規化する。

可能な場合はfilesystem canonical pathへ変換する。

明示指定も可能。

```bash
svcnest add api --cwd ~/src/my-api -- uv run main.py
```

---

# 6. executableの自動解決

ユーザーに `uv` や `node` などの絶対パスを入力させない。

例えば、

```bash
svcnest add api -- uv run main.py
```

を実行した場合、登録時の環境で `uv` を自動検索する。

解決ルール:

```text
"uv"
→ PATHから検索

"./target/release/server"
→ cwd基準で絶対パス化

"/usr/local/bin/server"
→ そのまま使用

Windowsの "server"
→ PATH + PATHEXTから server.exe 等を検索
```

見つからない場合は登録を失敗させる。

---

# 7. commandの保存

元のcommandとresolved executableは分けて保存する。

概念:

```toml
command = [
    "uv",
    "run",
    "main.py",
    "--port",
    "8000"
]

resolved_executable = "/Users/user/.local/bin/uv"
```

表示には元commandを使用する。

実行時には `resolved_executable` を使用する。

これにより、

```bash
svcnest status api
```

では、

```text
Command:       uv run main.py --port 8000
Executable:    /Users/user/.local/bin/uv
Working dir:   /Users/user/projects/api
```

と表示できる。

---

# 8. shellを介さない

commandはargv配列として保存・実行する。

```bash
svcnest add api -- uv run main.py --port 8000
```

は、

```text
argv[0] = uv
argv[1] = run
argv[2] = main.py
argv[3] = --port
argv[4] = 8000
```

として扱う。

shell command文字列へ結合してはならない。

パイプ等を使用したい場合だけユーザー自身が、

```text
sh -lc "..."
```

またはWindowsで、

```text
powershell -Command "..."
```

を登録する。

Windows の `.bat` / `.cmd` / `.ps1` は直接登録できる。一般のバッチは Windows 標準の `cmd.exe` を使い、引数のエスケープを Rust 標準ライブラリに委ねる。標準の Node.js ランチャーは実体の Node.js と JavaScript へ解決する。

`.ps1` は `--shell` で指定した PowerShell、登録元の直近のシェルが PowerShell の場合の実体、登録時 PATH の `pwsh.exe` / `powershell.exe`、Windows 標準の PowerShell の順で解決する。実行ファイルとスクリプトの絶対パス、`-NoLogo -NoProfile -File` を保存し、実行時には元の argv の引数を渡す。セッション限定 `PSExecutionPolicyPreference` と、同じシェルを選んだ場合の `PSModulePath` を保存し、明示した環境設定を優先する。daemon の同名環境変数は継承せず、選択したシェルの既定値または保存した設定を使う。関数、プロファイル、セッション内変数の再現は対象外とする。

---

# 9. 登録時PATH

daemonはTerminalとは異なるPATHで起動する可能性がある。

そのため登録時:

```text
command executable
```

は絶対パスへ解決する。

さらにtarget process自身が別プログラムを起動できるよう、登録時の `PATH` もservice環境として保存する。

Windows の `.ps1` では実行環境を再現するため、前節の `PSModulePath` / `PSExecutionPolicyPreference` をシェル用の既定値として保存する。これらより env-file と `--env` の設定を優先する。

これら以外の環境変数は自動保存しない。API_KEY等を意図せず永続化しないため。

---

# 10. カレントディレクトリからのサービス解決

service名を省略可能にする。

例えば、

```text
登録済service:
api

cwd:
/Users/user/projects/api
```

なら、

```bash
cd /Users/user/projects/api

svcnest start
svcnest stop
svcnest restart
svcnest status
svcnest logs
```

は自動的に `api` を対象とする。

---

# 11. 子ディレクトリからの解決

完全一致だけではなく親ディレクトリ方向へ探索する。

登録:

```text
/Users/user/projects/api
```

現在位置:

```text
/Users/user/projects/api/src/routes
```

で、

```bash
svcnest status
```

した場合:

```text
/Users/user/projects/api/src/routes
/Users/user/projects/api/src
/Users/user/projects/api
```

の順に探索し、

```text
api
```

を対象にする。

Git repository探索のようなUXとする。

---

# 12. service target解決順序

serviceを必要とするcommandでは以下の順序を必ず使用する。

```text
1. service名が明示されている
       ↓
   そのservice

2. service名なし
       ↓
   current directory完全一致

3. 完全一致なし
       ↓
   親directoryを順番に探索

4. 見つからない
       ↓
   SERVICE_NOT_FOUND
```

例:

```bash
svcnest restart api
```

ではcwd探索を行わず必ず `api` を対象にする。

---

# 13. 同一cwdの複数サービス

同じdirectoryへ複数serviceを登録できる。

例:

```bash
svcnest add api -- uv run api.py
svcnest add worker -- uv run worker.py
```

この状態で、

```bash
svcnest start
```

した場合、勝手に1つを選んではならない。

エラー:

```text
Multiple services are registered for this directory:

  api
  worker

Specify a service:

  svcnest start api

or:

  svcnest start --all
```

とする。

---

# 14. --all

directory-based commandでは、

```bash
svcnest start --all
svcnest stop --all
svcnest restart --all
svcnest status --all
```

をサポートする。

現在directoryまたは最初に一致した親directoryに登録されているservice全件を対象とする。

global全serviceという意味にはしない。

将来的に必要なら別途global optionを追加する。

---

# 15. start

```bash
svcnest start [service]
```

は登録済serviceをdaemon管理下でbackground起動する。

例:

```bash
svcnest start api
```

または、

```bash
cd ~/projects/api
svcnest start
```

target processはterminalを閉じても動き続ける。

既にrunningなら二重起動せず成功扱い。

---

# 16. run

`run` は `start` と明確に分離する。

```bash
svcnest run [service]
```

は登録済みservice設定を利用してforeground実行する。

使用する設定:

```text
cwd
resolved executable
arguments
environment
env-file
```

例:

```bash
cd ~/projects/api

svcnest run
```

は実質、

```bash
uv run main.py --port 8000
```

を登録済み設定で実行する。

---

# 17. runの用途

`run` は主にデバッグ用途とする。

典型例:

```bash
svcnest stop
svcnest run
```

targetのstdout/stderrは現在のterminalへ直接接続する。

Ctrl+Cはforeground targetへ送信する。

daemon管理のbackground serviceとしては登録しない。

restart policyも適用しない。

---

# 18. run中とstart中の重複

serviceが既にdaemonでrunningの場合、

```bash
svcnest run
```

はデフォルトで拒否する。

例:

```text
api is already running under svcnest.

Stop it first:

  svcnest stop api
```

明示的に別instanceを起動するforce optionはv1では提供しない。

---

# 19. stop

```bash
svcnest stop [service]
```

manual stopではrestart policyを発火させない。

Unix:

```text
SIGTERM
↓
timeout
↓
SIGKILL
```

Windows:

```text
graceful termination attempt
↓
timeout
↓
Job Object terminate
```

子孫processも含めて終了させる。

---

# 20. restart

```bash
svcnest restart [service]
```

は、

```text
stop
↓
完全停止確認
↓
start
```

として実装する。

旧process treeが残った状態で新processを起動してはならない。

---

# 21. enable / disable

```bash
svcnest enable [service]
```

は、

```text
daemon起動時にserviceを自動startする
```

という意味。

即時起動:

```bash
svcnest enable --now
```

停止しつつ無効化:

```bash
svcnest disable --now
```

service名省略時はcwd解決を使用する。

---

# 22. 最初のdaemon登録

最初に、

```bash
svcnest enable ...
```

した際、svcnest daemon自体がOSに未登録なら自動登録する。

明示的にも可能。

```bash
svcnest daemon install
```

---

# 23. OS integration

macOS:

```text
LaunchAgent
~/Library/LaunchAgents/
```

Linux:

```text
systemd --user
```

Windows:

```text
Task Scheduler
At logon
current user
```

Windows Serviceはv1では使用しない。

管理者権限を要求しないことを優先する。

---

# 24. restart policy

以下をサポート。

```text
never
on-failure
always
```

default:

```text
on-failure
```

manual `stop` / `restart` / daemon shutdownではrestartしない。

---

# 25. crash backoff

高速restart loopを防ぐ。

```text
1 sec
2 sec
4 sec
8 sec
16 sec
30 sec
30 sec
...
```

60秒以上安定稼働したらリセット。

5分以内に10回restartした場合:

```text
state = failed
reason = restart-limit
```

として停止する。

---

# 26. runner

daemonが直接target processを管理しない。

必ず:

```text
daemon
  ↓
runner
  ↓
target
```

とする。

runnerの責務:

```text
process spawn
process tree管理
stdout/stderr
restart policy
backoff
exit status
stop
daemonとのcontrol channel
```

---

# 27. daemon crash

daemonとrunnerのcontrol channelが切断された場合、runnerはtargetを停止する。

```text
daemon crash
↓
runner detects channel close
↓
graceful stop
↓
timeout
↓
force kill tree
↓
runner exit
```

daemon再起動時に古いprocessと新しいprocessが二重起動しないことを必須条件とする。

---

# 28. logs

background startされたserviceのstdout/stderrを保存する。

```text
2026-10-08T12:30:20+09:00 stdout | Server started
2026-10-08T12:30:21+09:00 stderr | Warning...
```

CLI:

```bash
svcnest logs [service]
svcnest logs [service] -n 100
svcnest logs [service] -f
```

service名省略時はcwd解決。

Ctrl+Cでserviceを停止してはならない。

---

# 29. log rotation

default:

```text
10 MiB / file
5 generations
```

target processを停止せずrotationする。

---

# 30. status

```bash
svcnest status
```

例:

```text
Service:       api
Status:        running
PID:           18234
Command:       uv run main.py --port 8000
Executable:    /Users/user/.local/bin/uv
Working dir:   /Users/user/projects/api
Enabled:       yes
Restart:       on-failure
Uptime:        01:32:08
Restarts:      0
```

---

# 31. list

```bash
svcnest list
```

は全登録serviceを表示する。

```text
NAME       STATUS    PID      ENABLED   CWD
api        running   18234    yes       ~/projects/api
worker     stopped   -        no        ~/projects/worker
mcp        failed    -        yes       ~/projects/mcp
```

`list` はcwdによるfilterをしない。

---

# 32. 環境変数

追加:

```bash
svcnest add api \
  --env MODE=production \
  --env-file .env \
  -- uv run main.py
```

env-file pathもcwd基準から絶対pathへ解決する。

優先順位:

```text
--env
>
env-file
>
daemon environment
```

ただし保存したPATHはservice環境へ適用する。

---

# 33. add options

最低限:

```text
--cwd
--restart
--env
--env-file
--enable
--description
--stop-timeout
--replace
```

を実装する。

---

# 34. remove

```bash
svcnest remove [service]
```

service名省略時はcwdから解決する。

runningならデフォルトでは拒否。

```bash
svcnest remove --stop
```

で停止して削除。

logも削除:

```bash
svcnest remove --stop --purge
```

---

# 35. service name

以下に限定。

```regex
[a-z0-9][a-z0-9._-]{0,63}
```

同名serviceは登録不可。

上書き:

```bash
svcnest add api --replace -- ...
```

---

# 36. config

serviceごとにconfig fileを持つ。

概念:

```toml
version = 1

name = "api"
description = ""

cwd = "/Users/user/projects/api"

command = [
    "uv",
    "run",
    "main.py",
    "--port",
    "8000"
]

resolved_executable = "/Users/user/.local/bin/uv"

enabled = true
restart = "on-failure"

stop_timeout_ms = 10000

[environment]
PATH = "..."
```

---

# 37. path portability

config内部では実行時に必要なpathは絶対pathとして保持する。

ただし表示用commandは元の入力形式を維持する。

v1では登録したservice configを別PCへそのままコピーして動作することまでは保証しない。

---

# 38. IPC

ネットワークportは使用しない。

macOS/Linux:

```text
Unix Domain Socket
```

Windows:

```text
Named Pipe
```

同一userからのみアクセス可能にする。

---

# 39. daemon single instance

1 userにつきdaemonは1つのみ。

Unixではlock file等。

WindowsではNamed Mutex等。

二重daemonを起動してはならない。

---

# 40. JSON output

machine-readable用途として、

```bash
svcnest list --json
svcnest status --json
```

を提供する。

JSON schemaはv1内では安定させる。

---

# 41. doctor

```bash
svcnest doctor
```

で最低限以下を診断。

```text
daemon registration
daemon state
IPC
config directories
log directories
broken config
missing cwd
missing executable
missing env-file
OS integration
```

---

# 42. config確認

```bash
svcnest config show [service]
svcnest config path [service]
```

service名省略時はcwdから解決する。

secret valuesはデフォルトでmaskする。

---

# 43. Rust実装

Rust stableを使用する。

推奨構成:

```text
src/
├── cli/
├── config/
├── daemon/
├── runner/
├── process/
├── ipc/
├── logging/
├── resolve/
│   ├── executable.rs
│   └── service.rs
├── platform/
│   ├── macos.rs
│   ├── linux.rs
│   └── windows.rs
└── error.rs
```

特に、

```text
resolve::executable
resolve::service
```

を独立させる。

`resolve::service` はcwd探索の唯一の実装箇所とし、各commandで独自実装しない。

---

# 44. Tokio

daemon、runner、IPC、log streaming等にはTokioを使用してよい。

単純なconfig操作等まで無理にasync化しない。

---

# 45. 必須invariant

以下は絶対に破ってはならない。

```text
同じserviceをdaemon管理下で二重起動しない

manual stop後にrestart policyで復活させない

daemon crashで孤児processを残さない

stopでchild/grandchildも終了する

shell command文字列へ再結合しない

atomic config updateを行う

OS差分を通常CLI操作へ露出させない

service名省略時の解決規則を全commandで統一する

同一cwd複数serviceを勝手に選択しない

runとstartの意味を混同しない

登録時に解決できたcommandはdaemon PATHに依存させない
```

---

# 46. v1で実装しないもの

以下は対象外。

```text
root/system service
remote host
SSH
Web UI
GUI
containers
Docker management
service dependencies
service groups
health checks
HTTP checks
resource limits
scheduler
cron
multi-user control
network API
secret manager
automatic updater
cluster management
```

将来的な追加を妨げない設計にはするが、v1に先回りして実装しない。

---

# 47. Acceptance Scenario 1: Python / uv

```bash
cd ~/projects/api

svcnest add api -- uv run main.py --port 8000
```

成功条件:

```text
cwdが自動登録される

uvのabsolute pathが自動解決される

service名apiで登録される
```

その後:

```bash
svcnest start
svcnest status
svcnest logs
svcnest stop
```

がservice名なしで動く。

---

# 48. Acceptance Scenario 2: Rust binary

```bash
cd ~/projects/myproject

svcnest add backend -- ./target/release/myproject serve
```

成功条件:

```text
relative executableがcwd基準でabsolute pathへ解決される

svcnest start

で起動できる
```

---

# 49. Acceptance Scenario 3: child directory

登録directory:

```text
~/projects/api
```

から、

```bash
cd ~/projects/api/src/routes

svcnest status
```

を実行して `api` を解決できる。

---

# 50. Acceptance Scenario 4: multiple services

```bash
cd ~/projects/app

svcnest add api -- uv run api.py
svcnest add worker -- uv run worker.py
```

その後:

```bash
svcnest start
```

ではambiguous errorになる。

```bash
svcnest start api
```

または、

```bash
svcnest start --all
```

なら成功する。

---

# 51. Acceptance Scenario 5: foreground debugging

```bash
cd ~/projects/api

svcnest stop
svcnest run
```

で登録済みcommandがforeground起動する。

stdout/stderrをterminalで直接確認できる。

Ctrl+Cで終了できる。

---

# 52. Acceptance Scenario 6: automatic startup

```bash
svcnest enable --now
```

後、

```text
logout/login
```

またはdaemon再起動しても、enabled serviceが自動起動する。

---

# 53. CI

GitHub Actionsで最低限:

```text
macOS
Windows
Ubuntu
```

について、

```text
cargo fmt --check
cargo clippy
cargo test
cargo build
```

を実行する。

service resolution / executable resolution / restart policy / config validation等はplatform independent testとして十分にテストする。

---

# 54. Definition of Done

以下の操作がmacOS / Windows / Linuxで同じ意味を持つこと。

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

さらに別directoryからは、

```bash
svcnest start api
```

のように明示service名でも操作可能であること。

ユーザーが通常利用で、

```text
systemd
launchd
Task Scheduler

uvのabsolute path
binaryのabsolute path
working directoryのabsolute path
```

を手動設定する必要がない状態を `svcnest v1` の完成とする。

---

# 55. UX原則

svcnestは「OSのservice managerを抽象化しただけのCLI」にしない。

目標は、

> プロジェクトのdirectoryへ移動し、普段使っているcommandを一度登録すれば、その後はservice名すら意識せず管理できること。

である。

典型的な日常操作は最終的に、

```bash
cd my-project

svcnest status
svcnest restart
svcnest logs -f
```

だけで完結することを重視する。
