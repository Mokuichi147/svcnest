# 検証記録

2026-10-09 時点のローカル検証です。CI の実行結果と区別して記載します。

| 対象 | 結果 |
|---|---|
| macOS arm64 / Rust 1.98 stable | 全 42 テスト成功、fmt / Clippy 成功、release ビルド成功 |
| macOS x86_64 | 全ターゲットの Clippy / 型チェック成功。実行は未検証 |
| Windows x64 / Rust 1.98 stable | 空の端末画面の修正後、全 45 テスト成功、fmt / Clippy 成功、release ビルド成功。Task Scheduler と foreground Ctrl+C は既存の実機検証も成功 |
| Linux x86_64 | unit 10・core 21・E2E 19 の全テスト成功、fmt / Clippy / release ビルド成功。systemd user の自動起動・常駐中更新・クラッシュ回復スモークも成功 |
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

`.github/workflows/ci.yml` は macOS arm64 / Intel、Windows x64、Ubuntu x86_64 / arm64 で fmt、Clippy、全テスト、release ビルド、JSON Schema、実時間の再起動ポリシー、OS 自動起動の実動作を検証します。今回の Windows 修正についてはローカルで検証し、修正後の GitHub Actions での再実行は未確認です。

単一起動は保存先にかかわらずユーザー単位です。別保存先で第二 daemon が稼働しないこと、明示サービス名と `--cwd` 指定が削除済み cwd に依存しないこと、daemon の IPC が利用できない場合に stop が停止成功を返さないことを追加で検証しています。全 55 項目の判定と残件は [completion-audit.md](completion-audit.md) を参照してください。

登録専用 PATH からの親 executable の解決と、既存 daemon の PATH に存在しない子 executable の起動が成功しました。実サービスで 26 MiB 超の出力を発生させ、標準 10 MiB で二回以上ローテーションし、元の PID が継続すること、follow の全 800 レコードに欠落と重複がないことを確認しています。

`scripts/check-json-schema.py` は Draft 2020-12 validator で実際の status / list の出力 16 件を検証し、stopped / running / foreground / failed / backoff を確認しました。Windows でも Python 3.12 と release バイナリで成功しました。Windows の npm / npx shim は Node.js / JavaScript の直接起動へ解決する実装と共通 parser テスト、Windows 専用の実 npx と literal argv の E2E テストがあり、Windows での実行も成功しています。

Windows の `.bat` / `.cmd` / `.ps1` の直接登録もローカルで検証しました。バッチは空白を含むパス、空文字、引用符、末尾のバックスラッシュ、日本語、環境変数展開や追加コマンドに見える引数を foreground / background で保持します。Windows PowerShell 5.1 と PowerShell 7 のそれぞれから、PATH にシェルがない状態で登録し、選んだ実行ファイルとスクリプト、作業ディレクトリ、モジュールパス、実行ポリシーを再利用できることを確認しました。`--shell` の明示指定、env-file の更新が保存した既定環境より優先すること、従来設定の読み込みも成功しています。この変更後の他 OS の実行と GitHub Actions は未確認です。

Windows の background runner が空の端末画面を開く問題を修正し、画面の有無と CTRL_BREAK による親子の正常停止を確認する E2E テストを追加しました。修正後の全 45 テスト、fmt、Clippy、release ビルドは成功しています。daemon 経由の E2E と、一時設定の runner を `CREATE_NO_WINDOW` で直接起動する検証の両方で、runner・対象・子が同じ画面なしの console を共有し、親子とも CTRL_BREAK を受信して終了コード 0 で停止し、対象プロセスが残らないことを確認しました。インストール先の実行ファイルを使用中の daemon を停止した後、`cargo install --path . --locked` による更新も成功しています。修正後の GitHub Actions の実行は未確認です。

`scripts/terminal-background-smoke.py` では実際の擬似端末から add / start を実行し、その端末セッションを切断した後も background サービスが同じ PID で稼働することを確認しました。

`scripts/restart-policy-smoke.py` は release バイナリの標準 on-failure を実時間で検証しました。起動間の待機は約 1.05 / 2.06 / 4.06 / 8.06 / 16.07 秒、その後は約 30.06 秒で、10 回の再起動後に `failed` / `restart-limit`、PID なし、最後の終了コード 7 になりました。別サービスを 61 秒稼働させ、次の待機が約 1.07 秒に戻ることと、手動停止後に起動しないことも確認しています。

Windows の release バイナリでも同じ実時間検証が成功しました。待機は約 1.13 / 2.13 / 4.14 / 8.13 / 16.14 秒、その後は約 30 秒で、10 回の再起動制限を確認しました。61 秒の安定稼働後の待機は約 1.11 秒に戻り、手動停止後に再起動しないことも確認しました。

`scripts/native-os-smoke.py` は三つの OS のユーザー自動起動を対象に、専用の定義を一時登録し、uv の解決、子ディレクトリ、登録の修復、OS 管理からの起動、daemon のクラッシュ回復、重複防止、restart、foreground Ctrl+C を確認します。macOS、Linux、Windows の実行が成功しました。Windows では BOM 付き UTF-16LE の XML で Task Scheduler への登録・修復・起動を確認し、クラッシュ時の旧ツリー回収後に OS から再起動して enabled サービスの復旧を検証しました。foreground Ctrl+C の終了コードは 130 です。Task Scheduler の起動失敗の再試行設定を、実行中 daemon の強制終了からの自動復旧保証として扱わないことは [README.md](../README.md) に記載しています。

Linux x86_64 では、内容ハッシュ別の runtime copy、systemd user unit からの起動、更新中の daemon・サービス PID 維持、更新後の `enable` / `daemon install`、停止後の新版選択、OS登録からの新版選択、クラッシュ回復を実プロセスで確認しました。systemd が監視する起動役と daemon の PID は一致し、実行中のコピーは上書きされません。
