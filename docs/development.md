# 開発

## ビルドと静的検査

```bash
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
```

## テスト

CI と同じ並列数で実行します。

- macOS / Linux: `cargo test --locked --all-targets -- --test-threads=3`
- Windows: `cargo test --locked --all-targets -- --test-threads=1`

`cargo test` は一時ディレクトリと専用の daemon を使用し、自動起動の実機設定を変更しません。実際の子・孫プロセス、同時起動、daemon の強制終了、foreground 実行、再起動バックオフを検証します。CI は macOS arm64 / Intel、Windows x64、Ubuntu x86_64 / arm64 で同じ確認を実行します。runner のラベルは [GitHub の公式一覧](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) に基づいています。

## 実機での検証スクリプト

daemon はユーザー単位で一つのため、既存の daemon を停止した状態で一つずつ実行してください。各スクリプトは独立した一時保存先を使い、テスト用 daemon を終了します。Windows では `--binary target/release/svcnest.exe` を指定します。

### OS 連携

```bash
python scripts/native-os-smoke.py
```

`uv` と、macOS の GUI ログイン、Linux の systemd user セッション、または Windows のログイン済みユーザー環境が必要です。専用の自動起動を一時登録し、登録の修復、自動起動、daemon のクラッシュ回復、重複防止、foreground の Ctrl+C を確認して、終了時に登録と一時ファイルを削除します。CI の Linux ではテスト基盤として user manager を先に開始します。svcnest 自体の操作は一般ユーザーで実行します。

Windows のクラッシュ検証では、旧プロセスの回収を確認してから Task Scheduler で daemon を再起動し、enabled サービスが復旧することを確認します。Task Scheduler の [RestartOnFailure](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-tsch/2ff4aa5a-7bc4-449f-bbb1-27475645867f) は起動失敗の再試行を設定するもので、実行中の daemon の強制終了からの自動再起動は保証しません。

### JSON Schema

```bash
python scripts/check-json-schema.py --binary target/release/svcnest
```

`scripts/requirements-test.txt` の依存関係を入れた専用 Python 環境で実行します。実際の stopped / running / foreground / failed / backoff の status と list 出力を [schema/status-v1.json](../schema/status-v1.json) で検証します。CI でも同じ検証を実行します。

### 再起動ポリシー

```bash
python scripts/restart-policy-smoke.py --binary target/release/svcnest
```

実時間の待機間隔、60 秒の安定稼働でのリセット、10 回制限を約 3 分で確認します。

### Unix の端末切断

```bash
python scripts/terminal-background-smoke.py --binary target/release/svcnest
```
