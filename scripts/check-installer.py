"""通信をローカルのリリース資材に置き換え、インストーラーの成功・失敗を検証する。"""

import argparse
import hashlib
import io
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib


SOURCE = Path(__file__).resolve().parents[1]
VERSION = tomllib.loads((SOURCE / "Cargo.toml").read_text())["package"]["version"]
REPOSITORY = "https://github.com/Mokuichi147/svcnest"
PLATFORMS = {
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Linux", "aarch64"): "aarch64-unknown-linux-musl",
    ("Linux", "x86_64"): "x86_64-unknown-linux-musl",
}


def executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


def fixture(directory, target, contents):
    archive = directory / f"svcnest-{target}.tar.gz"
    with tarfile.open(archive, "w:gz") as package:
        entry = tarfile.TarInfo("svcnest")
        entry.mode = 0o755
        entry.size = len(contents)
        package.addfile(entry, io.BytesIO(contents))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n")
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    if os.name == "nt":
        raise SystemExit("この検証は macOS / Linux 用です")
    native_binary = args.binary.resolve(strict=True) if args.binary else None
    checked = 0
    with tempfile.TemporaryDirectory(prefix="svcnest-installer-") as temporary:
        root = Path(temporary).resolve()
        shims = root / "shims"
        assets = root / "assets"
        downloads = root / "downloads"
        for directory in (shims, assets, downloads):
            directory.mkdir()
        executable(shims / "uname", '#!/bin/sh\ncase "$1" in -s) printf "%s\\n" "$SVCNEST_TEST_OS" ;; -m) printf "%s\\n" "$SVCNEST_TEST_ARCH" ;; *) exit 1 ;; esac\n')
        executable(shims / "curl", f"#!{sys.executable}\n" + '''import os
from pathlib import Path
import shutil
import sys
args = sys.argv[1:]
for option, value in (("--proto", "=https"), ("--proto-redir", "=https"), ("--tlsv1.2", None), ("--fail", None), ("--location", None)):
    assert option in args, option
    if value is not None:
        assert args[args.index(option) + 1] == value
url = args[-1]
with open(os.environ["SVCNEST_TEST_REQUESTS"], "a") as log:
    log.write(url + "\\n")
assert url.startswith(os.environ["SVCNEST_TEST_BASE"] + "/"), url
source = Path(os.environ["SVCNEST_TEST_ASSETS"]) / url.rsplit("/", 1)[1]
if not source.is_file():
    sys.exit(22)
shutil.copyfile(source, args[args.index("--output") + 1])
''')
        env = os.environ.copy()
        env.update({
            "PATH": str(shims) + os.pathsep + env.get("PATH", ""),
            "TMPDIR": str(downloads),
            "SVCNEST_TEST_ASSETS": str(assets),
            "SVCNEST_TEST_REQUESTS": str(root / "requests"),
            "SVCNEST_TEST_BASE": REPOSITORY + "/releases/latest/download",
            "SVCNEST_TEST_OS": "Darwin",
            "SVCNEST_TEST_ARCH": "arm64",
            "SVCNEST_INSTALL_DIR": str(root / "install with spaces"),
        })
        contents = f'#!/bin/sh\nprintf "svcnest {VERSION}\\n"\n'.encode()
        for target in PLATFORMS.values():
            fixture(assets, target, contents)
        destination = Path(env["SVCNEST_INSTALL_DIR"]) / "svcnest"

        def install(*options, succeeds=True, message=None, streamed=False, modify_path=False, test_env=None):
            nonlocal checked
            # パイプで受け取る sh と、保存したスクリプトの実行を両方検証する。
            command = ["sh", "-s", "--"] if streamed else ["sh", str(SOURCE / "install.sh")]
            # 実ユーザーの設定ファイルへ書き込まず、専用の一時設定だけを検証する。
            if not modify_path:
                command.append("--no-modify-path")
            result = subprocess.run(command + list(options), env=env if test_env is None else test_env, capture_output=True, text=True,
                                    input=(SOURCE / "install.sh").read_text() if streamed else None, timeout=30)
            assert (result.returncode == 0) == succeeds, result.stdout + result.stderr
            if message:
                assert message in result.stdout + result.stderr, result.stdout + result.stderr
            assert not list(downloads.iterdir()), "取得用の一時ファイルが残っています"
            assert not list(destination.parent.glob(".svcnest-install.*")), "配置用の一時ファイルが残っています"
            checked += 1
            return result

        for (os_name, arch), target in PLATFORMS.items():
            env.update(SVCNEST_TEST_OS=os_name, SVCNEST_TEST_ARCH=arch)
            install(streamed=True)
            assert destination.read_bytes() == contents
            assert os.access(destination, os.X_OK)
            assert (root / "requests").read_text().splitlines()[-1].endswith(target + ".tar.gz.sha256")

        # 同じパスを再利用し、開いている旧ファイルの内容が更新されないことを確認する。
        destination.write_bytes(b"previous executable")
        with destination.open("rb") as previous:
            install()
            assert previous.read() == b"previous executable"
        assert destination.read_bytes() == contents

        env["SVCNEST_TEST_BASE"] = REPOSITORY + f"/releases/download/v{VERSION}"
        install("--version", VERSION)
        install("--version", "v" + VERSION)
        alternate = root / "alternate with spaces"
        install("--version", VERSION, "--install-dir", str(alternate))
        assert (alternate / "svcnest").read_bytes() == contents
        env["SVCNEST_TEST_BASE"] = REPOSITORY + "/releases/latest/download"

        install("--help", message="使い方")
        install("--version", succeeds=False, message="値が必要")
        install("--install-dir", succeeds=False, message="値が必要")
        install("--version", "../../other", succeeds=False, message="無効なバージョン")
        install("--unknown", succeeds=False, message="不明な引数")
        for os_name, arch in (("FreeBSD", "x86_64"), ("Linux", "armv7l")):
            env.update(SVCNEST_TEST_OS=os_name, SVCNEST_TEST_ARCH=arch)
            install(succeeds=False, message="未対応" if arch == "armv7l" else "macOS / Linux 用")
        env.update(SVCNEST_TEST_OS="Linux", SVCNEST_TEST_ARCH="x86_64")
        archive = assets / "svcnest-x86_64-unknown-linux-musl.tar.gz"
        checksum = archive.with_name(archive.name + ".sha256")
        before = destination.read_bytes()
        checksum.unlink()
        install(succeeds=False, message="取得できませんでした")
        assert destination.read_bytes() == before
        fixture(assets, "x86_64-unknown-linux-musl", contents)
        archive.write_bytes(archive.read_bytes() + b"tampered")
        install(succeeds=False, message="SHA-256 が一致しません")
        assert destination.read_bytes() == before
        fixture(assets, "x86_64-unknown-linux-musl", contents)
        checksum.write_text("invalid checksum\n")
        install(succeeds=False, message="チェックサムの形式が不正")
        assert destination.read_bytes() == before
        fixture(assets, "x86_64-unknown-linux-musl", b"#!/bin/sh\nexit 7\n")
        install(succeeds=False, message="実行できませんでした")
        assert destination.read_bytes() == before
        fixture(assets, "x86_64-unknown-linux-musl", b"#!/bin/sh\necho svcnest 0.0.0\n")
        env["SVCNEST_TEST_BASE"] = REPOSITORY + f"/releases/download/v{VERSION}"
        install("--version", VERSION, succeeds=False, message="バージョンとバイナリが一致しません")
        assert destination.read_bytes() == before
        env["SVCNEST_TEST_BASE"] = REPOSITORY + "/releases/latest/download"
        fixture(assets, "x86_64-unknown-linux-musl", contents)
        destination.unlink()
        destination.symlink_to(alternate / "svcnest")
        install(succeeds=False, message="シンボリックリンク")
        assert (alternate / "svcnest").read_bytes() == contents
        destination.unlink()
        destination.mkdir()
        install(succeeds=False, message="ディレクトリ")
        destination.rmdir()

        profiles = root / "profiles"
        profiles.mkdir()
        original_dir = env["SVCNEST_INSTALL_DIR"]
        # シェルの展開に見える文字を、そのままの配置先として保持する。
        literal_dir = root / "bin with ' $SVCNEST_TEST_VALUE $(touch injected) `touch injected2` \\slash"
        env["SVCNEST_INSTALL_DIR"] = str(literal_dir)
        for shell in ("sh", "bash", "zsh"):
            shell_binary = shutil.which(shell)
            if not shell_binary:
                continue
            profile = profiles / f"{shell}rc"
            profile.write_text("export SVCNEST_PROFILE_KEEP=preserved")
            env.update(SHELL=shell_binary, SVCNEST_PROFILE=str(profile))
            install(modify_path=True, streamed=True, message="PATH を設定しました")
            configured = profile.read_text()
            assert configured.startswith("export SVCNEST_PROFILE_KEEP=preserved\n")
            install(modify_path=True)
            assert profile.read_text() == configured, "同じ PATH 設定を重複して追加しました"
            command = '. "$SVCNEST_PROFILE"; . "$SVCNEST_PROFILE"; test "$SVCNEST_PROFILE_KEEP" = preserved; command -v svcnest; printf "%s\\n" "$PATH"'
            result = subprocess.run([shell_binary, "-c", command], env=env, cwd=root, capture_output=True,
                                    text=True, check=True, timeout=30)
            selected, search_path = result.stdout.splitlines()
            assert selected == str(literal_dir / "svcnest"), result.stdout
            assert search_path.split(os.pathsep).count(str(literal_dir)) == 1
            assert not (root / "injected").exists()
            assert not (root / "injected2").exists()
            checked += 1
        env["SVCNEST_INSTALL_DIR"] = original_dir

        # 配置先がすでに PATH にある場合と、明示的に無効化した場合は編集しない。
        profile = profiles / "unchanged"
        env.update(SHELL="/bin/sh", SVCNEST_PROFILE=str(profile))
        search_path = env["PATH"]
        env["PATH"] = original_dir + os.pathsep + search_path
        install(modify_path=True)
        assert not profile.exists()
        env["PATH"] = search_path
        install()
        assert not profile.exists()

        # 取得・検証が失敗した場合はシェルの設定も変更しない。
        checksum.unlink()
        install(modify_path=True, succeeds=False, message="取得できませんでした")
        assert not profile.exists()
        fixture(assets, "x86_64-unknown-linux-musl", contents)
        profile.mkdir()
        install(modify_path=True, succeeds=False, message="通常のファイルではありません")
        profile.rmdir()
        env.pop("SVCNEST_PROFILE")

        # zsh / fish の標準設定先は、各シェルの公式の設定ディレクトリ変数で隔離する。
        zsh_binary = shutil.which("zsh")
        if zsh_binary:
            dotdir = profiles / "zsh startup"
            env.update(SHELL=zsh_binary, ZDOTDIR=str(dotdir))
            install(modify_path=True)
            assert (dotdir / ".zshrc").is_file()
            result = subprocess.run([zsh_binary, "-d", "-ic", "command -v svcnest"], env=env,
                                    capture_output=True, text=True, check=True, timeout=30)
            assert result.stdout.strip() == str(destination)
            checked += 1
        env.update(SHELL="/usr/bin/fish", XDG_CONFIG_HOME=str(profiles / "fish startup"))
        install(modify_path=True)
        fish_profile = Path(env["XDG_CONFIG_HOME"]) / "fish/config.fish"
        assert "fish_add_path --global --prepend" in fish_profile.read_text()
        fish_before = fish_profile.read_text()
        install(modify_path=True)
        assert fish_profile.read_text() == fish_before
        fish_binary = shutil.which("fish")
        if fish_binary:
            env["SVCNEST_PROFILE"] = str(fish_profile)
            result = subprocess.run([fish_binary, "--no-config", "-c", 'source "$SVCNEST_PROFILE"; command -v svcnest'],
                                    env=env, capture_output=True, text=True, check=True, timeout=30)
            assert result.stdout.strip() == str(destination)
            env.pop("SVCNEST_PROFILE")
            checked += 1

        # 既存 Cargo CLI と自動起動の参照先を、既定インストールでそのまま更新する。
        cargo = root / "legacy cargo"
        legacy = cargo / "bin/svcnest"
        legacy.parent.mkdir(parents=True)
        executable(legacy, '#!/bin/sh\necho "svcnest 0.9.0"\n')
        migration_env = env.copy()
        migration_env.pop("SVCNEST_INSTALL_DIR")
        migration_env["CARGO_HOME"] = str(cargo)
        migration_env["SVCNEST_PROFILE"] = str(profiles / "migration")
        system_path = os.pathsep.join((str(shims), "/usr/bin", "/bin", "/usr/sbin", "/sbin"))
        migration_env["PATH"] = os.pathsep.join((str(legacy.parent), str(destination.parent), system_path))
        fixture(assets, PLATFORMS[(env["SVCNEST_TEST_OS"], env["SVCNEST_TEST_ARCH"])], contents)
        install(modify_path=True, test_env=migration_env)
        assert legacy.read_bytes() == contents
        selected = subprocess.check_output(["sh", "-c", 'command -v svcnest; svcnest --version'], env=migration_env, text=True).splitlines()
        assert selected == [str(legacy), f"svcnest {VERSION}"]
        assert not (profiles / "migration").exists()

        # PATH にない既存 Cargo インストールも CARGO_HOME から見つける。
        executable(legacy, '#!/bin/sh\necho "svcnest 0.9.0"\n')
        migration_env["PATH"] = system_path
        install(test_env=migration_env)
        assert legacy.read_bytes() == contents

        # 明示した CLI 引数または環境変数の配置先は、自動検出より優先する。
        executable(legacy, '#!/bin/sh\necho "svcnest 0.9.0"\n')
        migration_env["PATH"] = str(legacy.parent) + os.pathsep + system_path
        explicit = root / "explicit install"
        install("--install-dir", str(explicit), test_env=migration_env)
        assert (explicit / "svcnest").read_bytes() == contents
        assert legacy.read_text().endswith('echo "svcnest 0.9.0"\n')
        migration_env["SVCNEST_INSTALL_DIR"] = str(explicit)
        install(test_env=migration_env)
        assert legacy.read_text().endswith('echo "svcnest 0.9.0"\n')

        # パッケージ管理などのリンクは更新せず、次の既存 CLI か標準の配置先へ導入する。
        migration_env.pop("SVCNEST_INSTALL_DIR")
        linked = root / "linked bin/svcnest"
        linked.parent.mkdir()
        linked.symlink_to(legacy)
        migration_env["PATH"] = str(linked.parent) + os.pathsep + system_path
        install(test_env=migration_env, message="更新していません")
        assert legacy.read_bytes() == contents
        assert linked.is_symlink()
        migration_env["PATH"] = system_path
        home = root / "home"
        executable(legacy, '#!/bin/sh\necho "svcnest 0.9.0"\n')
        fallback_env = migration_env | {"HOME": str(home), "CARGO_HOME": str(root / "empty cargo"),
                                        "PATH": str(linked.parent) + os.pathsep + system_path}
        install(test_env=fallback_env, message="更新していません")
        assert (home / ".local/bin/svcnest").read_bytes() == contents
        assert legacy.read_text().endswith('echo "svcnest 0.9.0"\n')
        if os.geteuid() != 0:
            # 書き込めない場所の既存 CLI も、権限エラーで中止せずに標準の配置先へ導入する。
            readonly = root / "readonly bin"
            readonly.mkdir()
            executable(readonly / "svcnest", '#!/bin/sh\necho "svcnest 0.9.0"\n')
            readonly.chmod(0o555)
            try:
                (home / ".local/bin/svcnest").unlink()
                readonly_env = fallback_env | {"PATH": str(readonly) + os.pathsep + system_path}
                install(test_env=readonly_env, message="更新していません")
                assert (home / ".local/bin/svcnest").read_bytes() == contents
                assert (readonly / "svcnest").read_text().endswith('echo "svcnest 0.9.0"\n')
            finally:
                readonly.chmod(0o755)

        # リンク経由の表記で PATH にある配置先は、実体のパスでも登録済みとして扱う。
        real_dir = root / "real bin"
        real_dir.mkdir()
        link_dir = root / "link bin"
        link_dir.symlink_to(real_dir, target_is_directory=True)
        link_profile = profiles / "linked"
        link_env = env | {"SVCNEST_INSTALL_DIR": str(link_dir), "SHELL": "/bin/sh",
                          "SVCNEST_PROFILE": str(link_profile), "PATH": str(link_dir) + os.pathsep + env["PATH"]}
        install(modify_path=True, test_env=link_env)
        assert (real_dir / "svcnest").read_bytes() == contents
        assert not link_profile.exists(), "リンク経由で PATH にある配置先を重複して追加しました"

        # 未対応のシェルでも配置は成功させ、PATH の手動設定を案内する。
        unsupported_env = env | {"SHELL": "/bin/tcsh"}
        unsupported_env.pop("SVCNEST_PROFILE", None)
        install(modify_path=True, test_env=unsupported_env, message="未対応のシェル")
        assert destination.read_bytes() == contents

        if native_binary:
            target = PLATFORMS[(platform.system(), platform.machine())]
            env.update(SVCNEST_TEST_OS=platform.system(), SVCNEST_TEST_ARCH=platform.machine())
            subprocess.run([sys.executable, str(SOURCE / "scripts/package-release.py"), "--target", target,
                            "--binary", str(native_binary), "--output-dir", str(assets)], check=True, timeout=30)
            install(streamed=True)
            assert destination.read_bytes() == native_binary.read_bytes()
            subprocess.run([str(destination), "--help"], check=True, stdout=subprocess.DEVNULL, timeout=30)
            shutil.copy2(native_binary, legacy)
            definition = subprocess.check_output([str(legacy), "--home", str(root / "registered state"),
                                                 "daemon", "install", "--dry-run"], text=True)
            assert str(legacy) in definition
            migration_env.update(SVCNEST_TEST_OS=platform.system(), SVCNEST_TEST_ARCH=platform.machine())
            install(test_env=migration_env)
            after = subprocess.check_output([str(legacy), "--home", str(root / "registered state"),
                                            "daemon", "install", "--dry-run"], text=True)
            assert after == definition, "更新によって自動起動の実行ファイルの参照先が変わりました"
    print(f"インストーラー検証成功: {checked} 件")


if __name__ == "__main__":
    main()
