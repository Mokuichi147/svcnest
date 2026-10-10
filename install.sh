#!/bin/sh
# 最後の main 呼び出しまで処理を開始せず、途中で切れたダウンロードを実行しない。
set -eu

usage() {
    cat <<'EOF'
svcnest のリリース済みバイナリをインストールします。

使い方: sh install.sh [--version <version>] [--install-dir <directory>] [--no-modify-path]

  --version       指定したバージョンを取得（例: 1.0.0 / v1.0.0）。標準は最新の安定版
  --install-dir   配置先。標準は $HOME/.local/bin
  --no-modify-path シェルの PATH 設定を変更しない
  -h, --help      この説明を表示

配置先は環境変数 SVCNEST_INSTALL_DIR でも指定できます。
PATH はシェルの起動設定へ自動追加します。反映には新しいターミナルを開いてください。
設定ファイルを指定する場合は SVCNEST_PROFILE を使用できます。
Linux で更新する場合は、先に svcnest daemon stop で常駐プロセスを停止してください。
EOF
}

fail() {
    printf 'svcnest: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [ -n "$temporary" ]; then rm -rf "$temporary"; fi
    if [ -n "$staging" ]; then rm -rf "$staging"; fi
}

download() {
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --fail --silent --show-error --location --retry 3 \
        --connect-timeout 15 --max-time 300 --output "$2" "$1" \
        || fail "取得できませんでした: $1（公開済みリリースとネットワークを確認してください）"
}

shell_quote() {
    # 単一引用符・空白・シェルの展開文字を含むパスも、文字列として保存する。
    if [ "${2:-}" = fish ]; then
        escaped=$(printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e "s/'/'\"'\"'/g")
    else
        escaped=$(printf '%s' "$1" | sed "s/'/'\"'\"'/g")
    fi
    printf "'%s'" "$escaped"
}

add_to_profile() {
    profile=$1
    if [ -e "$profile" ] && [ ! -f "$profile" ]; then
        fail "シェルの設定先が通常のファイルではありません: $profile"
    fi
    if [ -f "$profile" ] && grep -Fqx "$path_line" "$profile"; then
        return
    fi
    mkdir -p "$(dirname "$profile")" \
        || fail "シェルの設定先を作成できません: $profile"
    printf '\n# svcnest の PATH 設定\n%s\n' "$path_line" >> "$profile" \
        || fail "バイナリは配置済みですが、シェルの設定先へ書き込めません: $profile"
    printf 'PATH を設定しました: %s\n' "$profile"
}

configure_path() {
    case ":${PATH:-}:" in *:"$install_dir":*) return 0 ;; esac
    [ "$modify_path" = yes ] || return 0
    quoted_dir=$(shell_quote "$install_dir")
    path_line="case \":\${PATH:-}:\" in *:${quoted_dir}:*) ;; *) export PATH=${quoted_dir}:\"\${PATH:-}\" ;; esac"
    shell_name=${SHELL:-}
    shell_name=${shell_name##*/}
    if [ "$shell_name" = fish ]; then
        quoted_dir=$(shell_quote "$install_dir" fish)
        path_line="fish_add_path --global --prepend $quoted_dir"
    fi
    if [ -n "${SVCNEST_PROFILE:-}" ]; then
        add_to_profile "$SVCNEST_PROFILE"
    else
        [ -n "${HOME:-}" ] || fail 'PATH の自動設定には HOME または SVCNEST_PROFILE が必要です'
        case "$shell_name" in
            zsh) add_to_profile "${ZDOTDIR:-$HOME}/.zshrc" ;;
            bash)
                if [ -f "$HOME/.bash_profile" ]; then
                    add_to_profile "$HOME/.bash_profile"
                elif [ -f "$HOME/.bash_login" ]; then
                    add_to_profile "$HOME/.bash_login"
                else
                    add_to_profile "$HOME/.profile"
                fi
                add_to_profile "$HOME/.bashrc"
                ;;
            fish) add_to_profile "${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish" ;;
            sh|dash|ksh|'') add_to_profile "$HOME/.profile" ;;
            *) fail "PATH の自動設定に未対応のシェルです: ${shell_name}（SVCNEST_PROFILE を指定してください）" ;;
        esac
    fi
    printf '新しいターミナルを開くと svcnest コマンドを使えます。\n'
}

main() {
    release=latest
    install_dir=${SVCNEST_INSTALL_DIR:-}
    temporary=
    staging=
    modify_path=yes
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail '--version の値が必要です'
                release=$2
                shift 2
                ;;
            --install-dir)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail '--install-dir の値が必要です'
                install_dir=$2
                shift 2
                ;;
            --no-modify-path) modify_path=no; shift ;;
            -h|--help) usage; exit 0 ;;
            *) fail "不明な引数: $1（--help を参照してください）" ;;
        esac
    done

    if [ -z "$install_dir" ]; then
        [ -n "${HOME:-}" ] || fail 'HOME または --install-dir を指定してください'
        install_dir=$HOME/.local/bin
    fi
    # オプションに見える相対パスも、通常のパスとして扱う。
    case "$install_dir" in /*) ;; *) install_dir=$PWD/$install_dir ;; esac
    # PATH の区切りや改行を含むディレクトリは、明示的な自動設定の無効化が必要。
    if [ "$modify_path" = yes ]; then
        case "$install_dir" in *:*|*'
'*) fail 'PATH に追加できない配置先です（--no-modify-path を指定できます）' ;; esac
    fi

    repository=https://github.com/Mokuichi147/svcnest
    if [ "$release" = latest ]; then
        base_url=$repository/releases/latest/download
    else
        release=${release#v}
        case "$release" in ''|*[!0-9A-Za-z.+-]*) fail "無効なバージョン: $release" ;; esac
        case "$release" in [0-9]*.[0-9]*.[0-9]*) ;; *) fail "無効なバージョン: $release" ;; esac
        base_url=$repository/releases/download/v$release
    fi

    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        arm64|aarch64) arch=aarch64 ;;
        x86_64|amd64) arch=x86_64 ;;
        *) fail "未対応の CPU: $arch" ;;
    esac
    case "$os" in
        Darwin) target=$arch-apple-darwin ;;
        Linux) target=$arch-unknown-linux-musl ;;
        *) fail "このインストーラーは macOS / Linux 用です（検出: ${os}）" ;;
    esac

    for dependency in curl tar mktemp; do
        command -v "$dependency" >/dev/null 2>&1 || fail "$dependency が必要です"
    done
    if command -v sha256sum >/dev/null 2>&1; then
        checksum_tool=sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        checksum_tool=shasum
    else
        fail 'SHA-256 の検証に sha256sum または shasum が必要です'
    fi

    temporary=$(mktemp -d "${TMPDIR:-/tmp}/svcnest-install.XXXXXX")
    trap cleanup 0
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    archive=svcnest-$target.tar.gz
    printf 'svcnest を取得しています: %s (%s)\n' "$release" "$target"
    download "$base_url/$archive" "$temporary/$archive"
    download "$base_url/$archive.sha256" "$temporary/$archive.sha256"
    IFS=' ' read -r expected checksum_name < "$temporary/$archive.sha256" \
        || fail 'チェックサムの形式が不正です'
    [ "$checksum_name" = "$archive" ] && [ "${#expected}" -eq 64 ] \
        || fail 'チェックサムの形式が不正です'
    case "$expected" in *[!0-9a-f]*) fail 'チェックサムの形式が不正です' ;; esac
    if [ "$checksum_tool" = sha256sum ]; then
        actual=$(sha256sum "$temporary/$archive")
    else
        actual=$(shasum -a 256 "$temporary/$archive")
    fi
    [ "${actual%% *}" = "$expected" ] || fail 'SHA-256 が一致しません。インストールを中止しました'

    tar -xzf "$temporary/$archive" -C "$temporary" svcnest
    [ -f "$temporary/svcnest" ] && [ ! -L "$temporary/svcnest" ] \
        || fail 'アーカイブに通常の svcnest 実行ファイルがありません'
    chmod 755 "$temporary/svcnest"
    installed_version=$("$temporary/svcnest" --version) \
        || fail '取得したバイナリを実行できませんでした'
    case "$installed_version" in 'svcnest '*) ;; *) fail 'バイナリのバージョン表示が不正です' ;; esac
    if [ "$release" != latest ]; then
        [ "$installed_version" = "svcnest $release" ] || fail '指定したバージョンとバイナリが一致しません'
    fi

    mkdir -p "$install_dir"
    install_dir=$(cd "$install_dir" && pwd -P)
    [ ! -L "$install_dir/svcnest" ] && [ ! -d "$install_dir/svcnest" ] \
        || fail "配置先がシンボリックリンクまたはディレクトリです: $install_dir/svcnest"
    # 同じファイルシステムで rename し、稼働中の実行ファイルの内容を上書きしない。
    staging=$(mktemp -d "$install_dir/.svcnest-install.XXXXXX")
    cp "$temporary/svcnest" "$staging/svcnest"
    chmod 755 "$staging/svcnest"
    mv -f "$staging/svcnest" "$install_dir/svcnest"

    printf 'インストール完了: %s\n配置先: %s/svcnest\n' "$installed_version" "$install_dir"
    configure_path
}

main "$@"
