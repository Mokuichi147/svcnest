会話やコメントの記述は日本語で行なってください。

## Git のコミットとプッシュ

- ユーザーがコミットとプッシュだけを依頼した場合は、GitHub CLI (`gh`) の認証を前提にせず、通常の `git add`、`git commit`、`git push` を使ってください。
- `gh auth status` の失敗は、PR作成やGitHub API操作が必要でない限りブロッカーとして扱わないでください。
- `.git/index.lock` の作成が `Operation not permitted` で失敗した場合は、ユーザー側のGitHub認証問題ではなくサンドボックス権限の問題として扱い、Git操作を承認済みの昇格経路で直ちに再実行してください。
- `git push` 自体が認証エラーを返した場合に限り、ユーザーへGit認証を依頼してください。
