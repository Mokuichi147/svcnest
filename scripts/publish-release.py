"""前回の公開リリースとの差分と全配布物を揃えてから GitHub Release を公開する。"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import quote

from release_metadata import SOURCE, release_targets, validate_tag


def run(*args, source=SOURCE, check=True):
    return subprocess.run(args, cwd=source, text=True, encoding="utf-8", capture_output=True,
                          check=check, timeout=120)


def tag_commit(tag, source=SOURCE):
    result = run("git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}", source=source, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def is_prerelease(tag):
    return "-" in tag.split("+", 1)[0]


def previous_release(releases, tag, current, source=SOURCE):
    candidates = [release for release in releases if not release["draft"] and release["tag_name"] != tag
                  and (is_prerelease(tag) or not release["prerelease"])]
    for release in sorted(candidates, key=lambda item: item.get("published_at") or "", reverse=True):
        commit = tag_commit(release["tag_name"], source)
        if commit and run("git", "merge-base", "--is-ancestor", commit, current, source=source, check=False).returncode == 0:
            return release["tag_name"], commit
    return None, None


def markdown_text(value):
    return re.sub(r"([\\`*_{}\[\]()#+.!|<>~-])", r"\\\1", value)


def release_notes(repository, tag, current, previous_tag=None, previous_commit=None, source=SOURCE):
    base = f"https://github.com/{repository}"
    changes = f"{previous_commit}..{current}" if previous_commit else current
    log = run("git", "log", "--reverse", "--format=%H%x00%s", changes, source=source).stdout
    lines = ["## 変更点", ""]
    if previous_tag:
        compare = f"{base}/compare/{quote(previous_tag, safe='')}...{quote(tag, safe='')}"
        lines += [f"前回の公開リリース `{previous_tag}` からの変更です。[差分全体]({compare})", ""]
    else:
        lines += ["初回リリースです。これまでの変更を掲載します。", ""]
    for entry in log.splitlines():
        commit, subject = entry.split("\0", 1)
        lines.append(f"- {markdown_text(subject)} ([{commit[:7]}]({base}/commit/{commit}))")
    if not log.strip():
        lines.append("前回の公開リリースから追加のコミットはありません。")
    lines += ["", "## 配布物", "", "| 環境 | ダウンロード | SHA-256 |", "|---|---|---|"]
    for row in release_targets():
        archive = f"svcnest-{row['target']}{row['extension']}"
        download = f"{base}/releases/download/{quote(tag, safe='')}/{archive}"
        lines.append(f"| {row['label']} | [{archive}]({download}) | [チェックサム]({download}.sha256) |")
    lines += ["", f"[全配布物の SHA256SUMS]({base}/releases/download/{quote(tag, safe='')}/SHA256SUMS)", ""]
    return "\n".join(lines)


def validate_artifacts(directory):
    assets, checksums = [], []
    for row in release_targets():
        name = f"svcnest-{row['target']}{row['extension']}"
        archive, checksum = directory / name, directory / (name + ".sha256")
        if not archive.is_file() or not checksum.is_file():
            raise ValueError(f"配布物またはチェックサムが不足しています: {name}")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if checksum.read_text(encoding="ascii").split() != [digest, name]:
            raise ValueError(f"配布物の SHA-256 が一致しません: {name}")
        assets.extend((archive, checksum))
        checksums.append(f"{digest}  {name}\n")
    manifest = directory / "SHA256SUMS"
    manifest.write_text("".join(checksums), encoding="ascii", newline="\n")
    return assets + [manifest]


def stable_version(tag):
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:\+[^\s]+)?", tag)
    return tuple(map(int, match.groups())) if match else None


def should_be_latest(tag, releases):
    current = stable_version(tag)
    if current is None:
        return False
    versions = [stable_version(release["tag_name"]) for release in releases
                if not release["draft"] and not release["prerelease"]]
    return all(version is None or version <= current for version in versions)


def publish(repository, tag, directory, source=SOURCE):
    validate_tag(tag)
    current = tag_commit(tag, source)
    if not current:
        raise ValueError(f"ローカルにリリース対象のタグがありません: {tag}")
    if run("git", "rev-parse", "HEAD", source=source).stdout.strip() != current:
        raise ValueError("リリース対象のタグと現在のコミットが一致しません")
    assets = validate_artifacts(directory)
    pages = json.loads(run("gh", "api", f"repos/{repository}/releases?per_page=100", "--paginate", "--slurp", source=source).stdout)
    releases = [release for page in pages for release in page]
    existing = next((release for release in releases if release["tag_name"] == tag), None)
    if existing and not existing["draft"]:
        published_names = {asset["name"] for asset in existing["assets"]}
        if not {asset.name for asset in assets} <= published_names:
            raise ValueError("公開済みリリースに不足する配布物があります。公開済みの内容は上書きしません")
        print(f"公開済みのリリースを保持します: {existing['html_url']}")
        return
    previous_tag, previous_commit = previous_release(releases, tag, current, source)
    notes = release_notes(repository, tag, current, previous_tag, previous_commit, source)
    prerelease = is_prerelease(tag)
    with tempfile.TemporaryDirectory(prefix="svcnest-release-") as temporary:
        notes_path = Path(temporary) / "notes.md"
        notes_path.write_text(notes, encoding="utf-8")
        common = ("--repo", repository, "--title", tag, "--notes-file", str(notes_path))
        if existing:
            run("gh", "release", "edit", tag, *common, f"--prerelease={str(prerelease).lower()}", source=source)
        else:
            options = ("--prerelease",) if prerelease else ()
            run("gh", "release", "create", tag, *common, "--draft", "--verify-tag", *options, source=source)
        # 途中でアップロードに失敗したら draft のまま残し、再実行で続きを完了できる。
        run("gh", "release", "upload", tag, *(str(asset) for asset in assets), "--repo", repository,
            "--clobber", source=source)
        latest = str(should_be_latest(tag, releases)).lower()
        run("gh", "release", "edit", tag, "--repo", repository, "--draft=false",
            f"--prerelease={str(prerelease).lower()}", f"--latest={latest}", source=source)
    print(f"リリースを公開しました: https://github.com/{repository}/releases/tag/{quote(tag, safe='')}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--assets-dir", type=Path, default=Path("dist"))
    args = parser.parse_args()
    try:
        publish(args.repository, args.tag, args.assets_dir.resolve())
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr.strip() or str(error)) from error
    except ValueError as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
