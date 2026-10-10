# 検証記録

2026-10-09 時点のローカル検証です。CI の実行結果と区別して記載します。

| 対象 | 結果 |
|---|---|
| macOS arm64 / Rust 1.98 stable | 常駐中更新の macOS 対応後、全 51 テスト成功、fmt / Clippy 成功、release ビルド成功 |
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

専用ブランチの常駐中更新対応では、単体・共通テスト 30 件と、稼働中のコピーを止めずに更新元の exe を rename・差し替えする独立 E2E が成功しました。対象と子の PID が維持され、旧コピーが変わらず、同じバージョン番号の別ビルドが別のコピーへ配置されることを実プロセスで確認しています。同時配置、壊れたコピーの拒否、自動起動定義のコピー先と更新元もテストしています。Windows の fmt / Clippy / release ビルド、macOS arm64 の全ターゲット型チェックも成功しました。daemon 経由の更新と、古い自動起動役から新版を選ぶ E2E は CI に追加しています。ローカルでは稼働中の 3 サービスを停止せず、ユーザー単位の daemon を使う E2E 一式の再実行は行っていません。

macOS もビルド別コピーから常駐する方式に対応しました。macOS arm64 / Rust 1.98 で全 51 テスト、fmt、Clippy、release ビルドが成功し、macOS Intel、Windows x64、Linux x86_64 の全ターゲット型チェックも成功しました。共通 E2E は署名を維持した異なる Mach-O への更新、daemon と対象・子孫の PID 維持、旧コピーの保持、次回 daemon 起動と旧自動起動役からの新版選択を確認しています。実 LaunchAgent の検証でも、更新後の enable / daemon install が登録と稼働中 PID を維持し、次回起動では新版へ同じ PID で exec することが成功しました。

`scripts/macos-install-update-smoke.py` では、専用の一時インストール先を 1.0.0 から 1.0.1 へ実際の `cargo install --offline --locked --force` で更新しました。daemon PID 48795、サービス PID 48826 は更新中も維持され、次回 daemon 起動で 1.0.1 の別コピーが選ばれました。この検証と LaunchAgent の常駐中更新検証は macOS arm64 / Intel の CI に追加しています。この段落はローカル実行結果を記載しています。

`scripts/terminal-background-smoke.py` では実際の擬似端末から add / start を実行し、その端末セッションを切断した後も background サービスが同じ PID で稼働することを確認しました。

`install.sh` の追加と PATH の自動設定対応後、macOS arm64 で `sh -n install.sh`、release ビルド、`python3 scripts/check-installer.py --binary target/release/svcnest` が成功しました。通信をローカル資材に置き換えた 40 件の検証で、4 OS / CPU 構成の選択、標準入力からのスクリプト実行、空白を含む配置先、バージョン指定、開いている旧ファイルを保持した更新、取得・SHA-256・実行確認の失敗時に既存バイナリを保持すること、実際の Mach-O の梱包・配置・実行を確認しています。専用の一時設定を使い、sh / bash / zsh で設定の読み込み、引用符・展開文字を含むパスの保持、追記の重複防止、PATH の重複防止、既存 PATH と自動設定無効化時の非編集も確認しました。zsh は新しい対話シェルからコマンドを解決できました。fish の設定生成と重複防止は成功していますが、fish 自体の実行は未検証です。リリース workflow の YAML とシェルの構文確認も成功しました。Linux の musl ビルド、Windows のネイティブバイナリのリリース用ビルド、GitHub Releases への実際の公開は未検証で、タグ push 時の workflow で確認します。

タグ push 時の自動公開について、`python3 scripts/check-release.py` の 14 テストが macOS arm64 で成功しました。一時 Git リポジトリと模擬 GitHub の応答を使い、タグ・manifest・lock・checkout の一致、前回の公開版からの直接コミットを含む差分、初回と prerelease の比較、draft・存在しないタグ・別履歴の除外、5 種類の tar.gz / zip のファイル構成と SHA-256、配布物の不足・破損時の公開禁止、アップロード後の公開、失敗した draft の再実行、公開済みリリースの保持、過去の安定版の修正による Latest の維持を確認しています。梱包の単体検証はサンプルのファイル内容を使用し、実際の Mach-O の梱包・配置はインストーラー検証で確認しています。GitHub への書き込みは行っていません。

Windows 用の `install.ps1` を追加し、macOS arm64 上の一時的な PowerShell 7.6.6 で共通処理の 70 項目を検証しました。Windows の判定、取得処理、バイナリのバージョン表示、ユーザー PATH の保存は模擬処理に置き換え、実ユーザーのレジストリを変更していません。SHA-256、更新、開いている旧ファイルの保持、取得・検証失敗時の既存ファイル保持、一時ファイルの削除、PATH の保持・重複防止、Invoke-Expression と引数付き ScriptBlock の起動を確認しています。実際の HTTPS とリダイレクトの取得も Microsoft の公開ページを使って成功しました。両方の `.ps1` は Windows PowerShell 5.1 で読み込める UTF-8 BOM 付きです。Windows のネイティブ PE の配置と実行は、CI に PowerShell 7 / Windows PowerShell 5.1 の検証を追加しましたが、実機での実行は未確認です。

リリースから既存 `ci.yml` を `workflow_call` で呼び出し、配布用ビルドは `validate` と `ci`、公開は `ci` と `build` を依存先にしました。5 環境の CI がすべて成功することを要求する依存関係と YAML / シェル / PowerShell の構文をローカルで確認しました。タグ push 時は通常 CI の二重起動を避け、リリースから同じコミットの CI 全項目を呼び出します。GitHub 上での実際の全 CI と公開の実行は未確認です。

`main` の履歴内のタグだけを公開する検証を追加し、リリース関連の全 19 テストが macOS arm64 で成功しました。初期検証と公開処理で `origin/main` を取得してタグ先の祖先関係を確認し、アップロード後の公開直前にも再取得・再検証します。一時的な bare origin を使い、main 内の注釈付きタグと過去のコミットを許可すること、バージョンが一致する未マージの別ブランチのタグを拒否して GitHub API を呼ばないこと、main の参照・取得ができない場合の拒否、アップロード中に main の履歴から対象が外れた場合に draft のまま止めることを確認しました。変更後のインストーラー 40 項目の検証も成功しました。

PR #4 の初回 CI は macOS arm64 / Intel と Linux x86_64 / arm64 で成功しました。Windows は梱包テストの tar 内の実行権限検証で失敗したため、tar の実行ファイルを 0755、LICENSE を 0644 に明示しました。元ファイルが 0644 でも実行権限付きで梱包できることを検証しました。この修正後の Windows CI は再実行で確認します。

2026-10-10 の PR #4 の再実行では、PowerShell 7 のインストーラー検証 74 件は成功しましたが、Windows PowerShell 5.1 で `System.IO.Compression.ZipArchiveMode` が未読み込みのため失敗しました。検証スクリプトで `System.IO.Compression` を明示的に読み込むよう修正し、Windows x64 / Rust 1.98 のローカル実行で PowerShell 7 と Windows PowerShell 5.1 のそれぞれ 74 件が成功しました。実際の release バイナリの ZIP 作成・配置・実行・SHA-256 の一致も含みます。fmt / Clippy / release ビルド、リリース処理 19 テスト、単体・共通・runner 診断の計 42 テストも成功しました。ユーザー PATH の保存は模擬処理で検証しています。既存のサービスが稼働しているため、daemon を使う E2E / OS 連携の再実行は CI で確認します。

インストーラーは、配置先が明示されていなければ PATH 内の既存 CLI、次に既存 Cargo 配置先を優先して更新します。macOS arm64 のインストーラー検証 45 項目で、旧 CLI が PATH の先頭にある状態で更新後のコマンドが新版になること、PATH にない CARGO_HOME の検出、明示した配置先の優先、実際のバイナリを使った自動起動定義の参照先の維持を確認しました。macOS の PowerShell 7.6.6 でも Windows 用共通処理 76 項目が成功しました。

Windows のユーザー PATH はレジストリから DoNotExpandEnvironmentNames で読み、REG_SZ / REG_EXPAND_SZ の種類を保持して書き戻す方式にしました。実ユーザーの Environment を変更せず、HKCU の専用テストキーで未展開値・種類の保持、変数変更への追従、重複防止、新規値の作成を検証する処理を Windows CI に追加しています。

`scripts/restart-policy-smoke.py` は release バイナリの標準 on-failure を実時間で検証しました。起動間の待機は約 1.05 / 2.06 / 4.06 / 8.06 / 16.07 秒、その後は約 30.06 秒で、10 回の再起動後に `failed` / `restart-limit`、PID なし、最後の終了コード 7 になりました。別サービスを 61 秒稼働させ、次の待機が約 1.07 秒に戻ることと、手動停止後に起動しないことも確認しています。

Windows の release バイナリでも同じ実時間検証が成功しました。待機は約 1.13 / 2.13 / 4.14 / 8.13 / 16.14 秒、その後は約 30 秒で、10 回の再起動制限を確認しました。61 秒の安定稼働後の待機は約 1.11 秒に戻り、手動停止後に再起動しないことも確認しました。

`scripts/native-os-smoke.py` は三つの OS のユーザー自動起動を対象に、専用の定義を一時登録し、uv の解決、子ディレクトリ、登録の修復、OS 管理からの起動、daemon のクラッシュ回復、重複防止、restart、foreground Ctrl+C を確認します。macOS、Linux、Windows の実行が成功しました。Windows では BOM 付き UTF-16LE の XML で Task Scheduler への登録・修復・起動を確認し、クラッシュ時の旧ツリー回収後に OS から再起動して enabled サービスの復旧を検証しました。foreground Ctrl+C の終了コードは 130 です。Task Scheduler の起動失敗の再試行設定を、実行中 daemon の強制終了からの自動復旧保証として扱わないことは [README.md](../README.md) に記載しています。

Linux x86_64 では、内容ハッシュ別の runtime copy、systemd user unit からの起動、更新中の daemon・サービス PID 維持、更新後の `enable` / `daemon install`、停止後の新版選択、OS登録からの新版選択、クラッシュ回復を実プロセスで確認しました。systemd が監視する起動役と daemon の PID は一致し、実行中のコピーは上書きされません。

2026-10-10 の PR #4 の CI（5c845c9）では、Linux x86_64 の Ctrl+C E2E が `foreground` を期待した時点で一時的な `stopping` を読み、失敗しました。`svcnest run` がサービスのロックを取得してから `foreground` を保存するまでの間、daemon はロック中の保存済み状態を前の runner の停止中として報告していました。テストで待機するのではなく、Ctrl+C の受信を登録した直後、子プロセスの起動前に `foreground` を保存し、設定の読み込みや起動に失敗した場合は前回の状態へ戻すよう修正しました。E2E は従来どおり PID の記録直後に `foreground` を要求します。

同日のレビューを受けて、インストーラーを次のように修正しました。既存 CLI がシンボリックリンクまたは書き込めない場所にある場合は更新せず、次の既存 CLI か標準の配置先へ導入して警告します。リンク経由の表記で PATH にある配置先は、実体のパスに解決した後も登録済みとして扱います。未対応のシェルでは配置を成功させ、PATH の手動設定を案内します。Windows では既存の exe を `File.Replace` で置き換えず、改名して退避してから配置し、実行中で削除できなかった旧 exe は次回の更新で削除します。macOS arm64 のインストーラー検証 50 項目、リリース処理 19 テスト、fmt / Clippy、単体・共通・runner 診断の全 39 テストが成功しました。Windows の検証には、配置先で node.exe を常駐させたままの更新、BOM を残した文字列の Invoke-Expression、リンクの既存 CLI を除外する処理を追加しました。手元に PowerShell がないため、これらと修正した E2E は CI で確認します。

修正後の CI（64b8db0）では、`foreground` 状態の保存を Ctrl+C の受信登録より前に移したため、macOS arm64 と Linux x86_64 の `scripts/native-os-smoke.py` が状態を確認した直後の SIGINT で既定の終了（-2）となり失敗しました。受信登録を状態の保存より前に移し、`foreground` が見えた時点で Ctrl+C を必ず処理するよう修正しました。BOM を残した文字列の Invoke-Expression は構文エラーになることを CI で確認したため、87af0db で install.ps1 から BOM を除き、検証スクリプトは UTF-8 として読み込むよう変更しています。
