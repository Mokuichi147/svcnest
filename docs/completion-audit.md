# v1 完了判定の監査

対象は [元の仕様](v1-spec.md) の全 55 項目です。実装の存在、macOS arm64 での動作、全対応環境での検証を区別します。型チェックだけでは、その OS の実行確認を済ませたと判断しません。

現時点では v1 の完了を証明できていません。macOS arm64 の実動作と共通ロジックのテストは成功していますが、macOS Intel、Windows、Linux の実行証拠と GitHub Actions の結果がありません。Windows の `npx` 起動経路は実装と型チェックがあり、実行は CI で確認する必要があります。

| 番号 | 必要な証拠と現在の状態 |
|---|---|
| 1 | `cli`、`daemon`、`runner` と native macOS smoke が日常操作を確認。他 OS の同じ UX は未検証。Windows の `npx` は Node.js と script の直接起動へ解決する実装があり、Windows 実行は未検証。 |
| 2 | CI に 5 OS/CPU 構成を設定。実行確認済みは macOS arm64。Windows 10/11 を含む残りの環境は未検証。 |
| 3 | `src/main.rs` の一つのバイナリで daemon / hidden runner を実行。OS 登録定義のテストは daemon だけを含むことを確認。 |
| 4 | `registration_config` と `project_workflow_resolves_children_and_preserves_registration_environment` が名前・cwd・実行ファイルの保存を確認。 |
| 5 | `resolve::executable::working_directory` と正規化テスト、削除済み cwd から明示 `--cwd` を指定するテストで確認。 |
| 6 | executable resolution の PATH・相対・絶対・存在しないファイルのテストで確認。Windows PATHEXT は実装と型チェックがあり、Windows 実行は未検証。 |
| 7 | TOML roundtrip と native smoke が元 argv / resolved executable を別々に保存することを確認。 |
| 8 | `ProcessTree::spawn` は保存した argv を `Command::args` へ渡す。一般の Windows batch は標準ライブラリの専用エスケープを使用し、標準 npm / npx / Node 用 shim は Node.js を直接起動する。`.ps1` は登録元のシェルと起動方法を保存する。Windows の batch / PowerShell / npx の実行テストで foreground・background・literal 引数・空白を含むパスを確認。 |
| 9 | PATH の自動保存と API_KEY の非保存を実行テストで確認。`.ps1` はシェル用のモジュールパスと実行ポリシーも保存し、env-file / 明示環境を優先する。既存 daemon の環境と異なる executable / 子 executable / PowerShell を登録時の環境で起動する実行テストが成功。 |
| 10 | 名前省略の start / status / logs / stop を project workflow と native smoke で確認。 |
| 11 | 子ディレクトリからの status / config / start / logs / stop を実行テストで確認。 |
| 12 | 唯一の resolver に明示名優先・完全一致・親探索を実装。名前指定の全主要操作は cwd が削除された状態でも成功。 |
| 13 | 同じ cwd の複数サービスで `AMBIGUOUS_SERVICE` を確認。名前を勝手に選ぶ経路はない。 |
| 14 | `--all` が最初に一致したディレクトリのサービスだけを選ぶことを、別ディレクトリのサービスを含めて確認。 |
| 15 | background 起動と同時 start の一つの PID を確認。実際の擬似端末セッションを切断しても同じ PID が稼働する実行確認が成功。Unix setsid / Windows detached 起動の実装証拠があり、Windows 端末の終了は未検証。 |
| 16 | foreground 実行が cwd・実行ファイル・argv・環境・env-file を再利用する実装と実行テストがある。 |
| 17 | stdout / stderr の直接接続、対話入力、Ctrl+C、foreground での再起動禁止を実行テストで確認。Windows console の実行は未検証。 |
| 18 | background 実行中の `run` と foreground 実行中の `start` の拒否を確認。force option はない。 |
| 19 | SIGTERM を無視する親・子・孫の強制停止と停止後の生存確認が成功。Windows Job Object の型チェックは成功、実行は未検証。 |
| 20 | 旧ツリーが完全停止した後の新 PID を restart テストで確認。 |
| 21 | enable / disable の状態と `--now` をテスト。native macOS smoke でも enable の cwd 解決を確認。他 OS の OS 登録を含む実行は未検証。 |
| 22 | 最初の enable の LaunchAgent 登録と、ファイルだけが残る stale 登録の復元を native smoke で確認。Windows / Linux の復元は未検証。 |
| 23 | LaunchAgent / systemd user / current-user Task Scheduler の生成を検証。実際の登録確認は macOS のみ。 |
| 24 | never / on-failure / always と手動停止の単体テスト、background と foreground の差、バックオフ中の停止の実行テストが成功。 |
| 25 | 1/2/4/8/16/30 秒、安定 60 秒でのリセット、5 分の履歴と 10 回制限を仮想時刻の単体テストで確認。release の実サービスでも 1/2/4/8/16/30 秒の待機、61 秒稼働後の 1 秒リセット、10 回の再起動後の `failed / restart-limit` を確認。 |
| 26 | daemon は `__runner` を生成し、runner が `ProcessTree` を生成。プロセスツリー・ログ・policy・制御・終了状態は runner にある。 |
| 27 | daemon 強制終了と直後の新起動で旧ツリーが残らず重複しないことを確認。実際の launchd の自動再起動でも成功。Windows / Linux は未検証。 |
| 28 | タイムゾーン付き stdout / stderr、末尾表示、follow の Ctrl+C がサービスを停止しないことを確認。 |
| 29 | 10 MiB / 現在と過去 4 ファイルの定数、世代を跨ぐ tail のテストがある。実サービスの 26 MiB 超の出力が二回以上 rotate し、同じ PID の継続と follow の全 800 レコードの欠落・重複なしを確認。 |
| 30 | status の公開フィールドを `ServiceStatus` と JSON スキーマで定義。native smoke と JSON 実行テストが確認。 |
| 31 | list は全登録サービスを返し、複数ディレクトリを含む実行テストで確認。 |
| 32 | env-file の絶対化、明示環境優先、保存 PATH 優先、env-file 値の非永続化を単体 / 実行テストで確認。 |
| 33 | CLI が全必須 add option を定義。cwd / restart / env / env-file / enable / stop-timeout / replace を検証。description は保存・表示の実装証拠。 |
| 34 | 起動中の拒否、`--stop` と `--purge` の削除、名前省略 / 全件選択を実行テストで確認。 |
| 35 | 正規表現・長さ・重複拒否・起動中の replace 拒否を確認。Windows の予約ファイル名も `svc-` prefix で扱う。 |
| 36 | サービスごとの version=1 TOML、default 値、名前と filename の一致、環境、設定検証と roundtrip を確認。 |
| 37 | 実行に必要なパスは絶対パス。command は元 argv。別 PC へのコピーを保証する実装はない。 |
| 38 | Unix socket の 0700/0600 と peer UID、Windows pipe の DACL / peer SID / remote 拒否を実装。Windows におけるアクセス制御の実動作は未検証。 |
| 39 | 保存先を変更しても同じユーザー単位の lock を使用。別保存先の第二 daemon が稼働しない実行テストは macOS で成功。Windows / Linux は未検証。 |
| 40 | status / list の schema_version=1 と `schema/status-v1.json` を用意。JSON 実行テストは成功。公開スキーマの Draft 2020-12 validator で実 status / list 出力 16 件、五つの状態を確認。Windows / Linux の実行は未検証。 |
| 41 | doctor が daemon 登録・状態・IPC・ディレクトリ・壊れた設定・失われた cwd / executable / env-file を検査。診断に入力 secret を含めない実行テストが成功。 |
| 42 | config show / path が共通の target 解決を利用。PATH 以外の保存環境値を標準でマスクし、明示解除と path を確認。 |
| 43 | stable Rust を使用し、CLI / config / daemon / runner / process / IPC / logging / resolve / platform / error を分離。cwd 探索は `resolve::service` のみ。 |
| 44 | 非同期処理は Tokio、設定の保存・検証・path 解決は同期処理。 |
| 45 | 二重起動、manual stop、crash、子孫停止、argv、atomic update、target 解決、曖昧さ、run / start を macOS で確認。すべての OS における不変条件の証明は未達。 |
| 46 | root/system service、remote / SSH、GUI / Web、containers 管理、依存関係、health check、scheduler / cron、multi-user / network API、secret manager、updater、cluster は実装していない。 |
| 47 | 実際の uv の argv、cwd、絶対実行ファイル、名前省略操作を native macOS smoke で確認。他 OS は未検証。 |
| 48 | 相対パスの Rust テスト用 executable をコピーして起動し、保存した表示用相対パスと絶対パスを確認。他 OS は未検証。 |
| 49 | project/src/routes からサービスを選択するテストと native smoke が成功。 |
| 50 | 同じ cwd の複数サービスで曖昧エラー、名前指定、`--all` を確認。他 OS の実行は未検証。 |
| 51 | stop → run、直接 stdout / stderr、端末入力、Ctrl+C を macOS で確認。他 OS の実行は未検証。 |
| 52 | 実際の LaunchAgent から daemon を再起動し、enabled service の起動を確認。logout/login は未実施。他 OS は未検証。 |
| 53 | GitHub Actions の 5 構成に fmt / Clippy / test / build、公開 JSON Schema、実時間の restart policy、実際の OS 自動起動の検証を設定。リポジトリ / remote が未設定で CI 未実行のため未達。 |
| 54 | macOS arm64 で一連の操作を確認。全対応環境で同じ意味と動作を証明する証拠が不足し、完了判定は未達。 |
| 55 | 日常の status / restart / logs が project cwd と子 cwd から名前なしで動く実行証拠がある。他 OS も同じ UX であることを確認する必要がある。 |

追加の PATH・大容量ログ・公開 JSON スキーマ・端末切断・実時間の再起動ポリシーの実行証拠は揃いました。残る対象は他 OS / CPU の実行、Windows の実 npx・アクセス制御・Job Object・Task Scheduler、Linux systemd user などの OS 連携です。CI の実際の OS 起動・修復・クラッシュ回復を含むスクリプトも用意し、macOS の経路は実行済みです。型チェックと共通ロジックの成功を、各 OS の実行成功に読み替えずに完了を判定します。
