# 検証記録

2026-10-08 時点のローカル検証です。CI の実行結果と区別して記載します。

| 対象 | 結果 |
|---|---|
| macOS arm64 / Rust 1.98 stable | 全 42 テスト成功、fmt / Clippy 成功、release ビルド成功 |
| macOS x86_64 | 全ターゲットの Clippy / 型チェック成功。実行は未検証 |
| Windows x64 | 全ターゲットの Clippy / 型チェック成功。実行は未検証 |
| Linux x86_64 | 全ターゲットの Clippy / 型チェック成功。実行は未検証 |
| Linux arm64 | CI マトリクスに追加。ローカル実行は未検証 |

macOS では実際の LaunchAgent を使う `scripts/native-macos-smoke.py` も成功しています。確認した操作は次のとおりです。

- `uv run main.py --port 8000` の cwd / PATH 解決と元 argv の保持
- 子ディレクトリからのサービス解決
- `enable --now` と LaunchAgent 登録
- OS 登録だけを削除した後の `enable` による登録の復元
- OS からの daemon 起動時に enabled サービスが起動
- OS 管理下の daemon を強制終了した後の自動再起動と、サービスの重複防止
- `restart`、`stop`、foreground `run` と Ctrl+C
- 一時 LaunchAgent、daemon、ログ、設定の削除

自動テストでは、複数 CLI の同時 start、daemon の強制終了と直後の start、SIGTERM を無視する子・孫プロセスの強制停止、手動停止後の再起動禁止、バックオフ中の停止、foreground と background の競合、擬似端末での対話入力と Ctrl+C、ログ follow の Ctrl+C、env-file と明示環境の優先順位、atomic 設定更新、ログ世代管理、壊れた設定の診断を確認しています。

`.github/workflows/ci.yml` は macOS arm64 / Intel、Windows x64、Ubuntu x86_64 / arm64 で fmt、Clippy、全テスト、release ビルド、JSON Schema、実時間の再起動ポリシー、OS 自動起動の実動作を検証します。GitHub リポジトリが未設定のため、この時点では CI を起動していません。Windows / Linux を正式な動作検証済みとして扱うには、各環境で CI または同等のテストを実行する必要があります。

単一起動は保存先にかかわらずユーザー単位です。別保存先で第二 daemon が稼働しないこと、明示サービス名と `--cwd` 指定が削除済み cwd に依存しないこと、daemon の IPC が利用できない場合に stop が停止成功を返さないことを追加で検証しています。全 55 項目の判定と残件は [completion-audit.md](completion-audit.md) を参照してください。

登録専用 PATH からの親 executable の解決と、既存 daemon の PATH に存在しない子 executable の起動が成功しました。実サービスで 26 MiB 超の出力を発生させ、標準 10 MiB で二回以上ローテーションし、元の PID が継続すること、follow の全 800 レコードに欠落と重複がないことを確認しています。

`scripts/check-json-schema.py` は Draft 2020-12 validator で実際の status / list の出力 16 件を検証し、stopped / running / foreground / failed / backoff を確認しました。Windows の npm / npx shim は Node.js / JavaScript の直接起動へ解決する実装と共通 parser テスト、Windows 専用の実 npx と literal argv の E2E テストがあり、型チェックは成功しています。Windows E2E の実行はまだ未検証です。

`scripts/terminal-background-smoke.py` では実際の擬似端末から add / start を実行し、その端末セッションを切断した後も background サービスが同じ PID で稼働することを確認しました。

`scripts/restart-policy-smoke.py` は release バイナリの標準 on-failure を実時間で検証しました。起動間の待機は約 1.05 / 2.06 / 4.06 / 8.06 / 16.07 秒、その後は約 30.06 秒で、10 回の再起動後に `failed` / `restart-limit`、PID なし、最後の終了コード 7 になりました。別サービスを 61 秒稼働させ、次の待機が約 1.07 秒に戻ることと、手動停止後に起動しないことも確認しています。

`scripts/native-os-smoke.py` は三つの OS のユーザー自動起動を対象に、専用の定義を一時登録し、uv の解決、子ディレクトリ、登録の修復、OS 管理からの起動、daemon のクラッシュ回復、重複防止、restart、foreground Ctrl+C を確認します。macOS の実行は成功しました。Linux の systemd user と Windows の Task Scheduler / console 経路は CI に組み込み済みですが、実行結果はまだありません。
