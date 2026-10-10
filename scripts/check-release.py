"""一時 Git 履歴と模擬 GitHub を使い、リリースの差分・配布物・公開順序を検証する。"""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import release_metadata


SOURCE = release_metadata.SOURCE
TARGETS = release_metadata.release_targets()
VERSION = release_metadata.release_version()
TAG = "v" + VERSION
spec = importlib.util.spec_from_file_location("publisher", SOURCE / "scripts/publish-release.py")
publisher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publisher)


def release(tag, *, draft=False, prerelease=False, date="2026-10-01T00:00:00Z", assets=()):
    return {"tag_name": tag, "draft": draft, "prerelease": prerelease, "published_at": date,
            "assets": [{"name": name} for name in assets],
            "html_url": f"https://github.com/example/svcnest/releases/tag/{tag}"}


class FakeGithub:
    def __init__(self, releases=(), fail_upload=False):
        self.releases = list(releases)
        self.fail_upload = fail_upload
        self.calls = []
        self.notes = ""

    def run(self, *args, **kwargs):
        if args[0] != "gh":
            return self.real_run(*args, **kwargs)
        self.calls.append(args)
        if args[1] == "api":
            return subprocess.CompletedProcess(args, 0, json.dumps([self.releases]), "")
        command, tag = args[2:4]
        if "--notes-file" in args:
            self.notes = Path(args[args.index("--notes-file") + 1]).read_text(encoding="utf-8")
        if command == "create":
            self.releases.append(release(tag, draft=True, prerelease="--prerelease" in args))
        current = next(item for item in self.releases if item["tag_name"] == tag)
        if command == "upload":
            if self.fail_upload:
                self.fail_upload = False
                raise subprocess.CalledProcessError(1, args, stderr="アップロードの模擬失敗")
            current["assets"] = [{"name": Path(arg).name} for arg in args[4:] if Path(arg).is_file()]
        elif command == "edit" and "--draft=false" in args:
            current["draft"] = False
        return subprocess.CompletedProcess(args, 0, "", "")


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="svcnest-release-check-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve() / "checkout"
        self.root.mkdir()
        self.upstream = self.root.parent / "upstream.git"
        self.assets = self.root / "dist"
        self.assets.mkdir()
        # 実ユーザーの Git 設定、フック、署名設定へ依存しない一時リポジトリを使う。
        env = {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull}
        self.environment = patch.dict(os.environ, env)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.git("init", "-q")
        self.git("config", "user.name", "svcnest-test")
        self.git("config", "user.email", "svcnest-test@example.invalid")
        (self.root / ".github").mkdir()
        (self.root / ".github/release-targets.json").write_text(json.dumps(TARGETS), encoding="utf-8")
        self.set_version("0.9.0")
        self.commit("初期版")
        self.git("tag", "v0.9.0")
        self.set_version(VERSION + "-rc.1")
        self.commit("PATH の自動設定を追加")
        self.git("tag", TAG + "-rc.1")
        self.set_version(VERSION)
        self.commit("引用符と [特殊文字] を修正")
        self.git("tag", TAG)
        self.git("init", "--bare", "-q", str(self.upstream))
        self.git("remote", "add", "origin", str(self.upstream))
        self.git("push", "-q", "origin", "HEAD:refs/heads/main")
        self.git("fetch", "-q", "--no-tags", "origin", "+refs/heads/main:refs/remotes/origin/main")
        for row in TARGETS:
            name = f"svcnest-{row['target']}{row['extension']}"
            archive = self.assets / name
            archive.write_bytes(f"配布物 {row['target']}".encode())
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            (self.assets / (name + ".sha256")).write_text(f"{digest}  {name}\n", encoding="ascii")
        self.metadata = patch.object(release_metadata, "SOURCE", self.root)
        self.metadata.start()
        self.addCleanup(self.metadata.stop)

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True, text=True,
                              encoding="utf-8", capture_output=True, timeout=30).stdout.strip()

    def set_version(self, version):
        (self.root / "Cargo.toml").write_text(f'[package]\nname = "svcnest"\nversion = "{version}"\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_text(f'[[package]]\nname = "svcnest"\nversion = "{version}"\n', encoding="utf-8")

    def commit(self, subject):
        self.git("add", ".")
        self.git("-c", "commit.gpgsign=false", "commit", "-qm", subject)

    def publish(self, fake, tag=TAG):
        fake.real_run = publisher.run
        with patch.object(publisher, "run", fake.run), contextlib.redirect_stdout(io.StringIO()):
            publisher.publish("example/svcnest", tag, self.assets, self.root)

    def test_tag_and_lock_versions_must_match(self):
        release_metadata.validate_tag(TAG)
        with self.assertRaisesRegex(ValueError, "一致しません"):
            release_metadata.validate_tag("v99.0.0")
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "svcnest"\nversion = "0.0.0"\n')
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            release_metadata.validate_tag(TAG)

    def test_main_history_accepts_annotated_tags_and_older_main_commits(self):
        self.git("tag", "-af", TAG, "-m", "main の公開用タグ")
        self.assertEqual(release_metadata.validate_main_history(TAG, self.root), self.git("rev-parse", "HEAD"))
        self.assertEqual(release_metadata.validate_main_history(TAG + "-rc.1", self.root),
                         self.git("rev-parse", TAG + "-rc.1"))

    def test_unmerged_branch_tag_blocks_metadata_and_all_github_operations(self):
        self.git("checkout", "-b", "feature")
        (self.root / "unmerged.txt").write_text("main に未反映の変更\n", encoding="utf-8")
        self.commit("別ブランチの変更")
        self.git("tag", "-f", TAG)
        # バージョン一致だけでは拒否できないタグでも、main への未反映を検出する。
        release_metadata.validate_tag(TAG)
        with patch.object(sys, "argv", ["release_metadata.py", "--tag", TAG]):
            with self.assertRaisesRegex(ValueError, "origin/main の履歴に含まれていません"):
                release_metadata.main()
        fake = FakeGithub()
        with self.assertRaisesRegex(ValueError, "origin/main の履歴に含まれていません"):
            self.publish(fake)
        self.assertEqual(fake.calls, [])
        self.assertFalse((self.assets / "SHA256SUMS").exists())

    def test_missing_main_reference_is_rejected(self):
        self.git("update-ref", "-d", "refs/remotes/origin/main")
        with self.assertRaisesRegex(ValueError, "origin/main を確認できません"):
            release_metadata.validate_main_history(TAG, self.root)

    def test_missing_remote_main_blocks_publication(self):
        self.git("--git-dir", str(self.upstream), "update-ref", "-d", "refs/heads/main")
        fake = FakeGithub()
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish(fake)
        self.assertEqual(fake.calls, [])

    def test_main_is_checked_again_before_publishing_the_uploaded_draft(self):
        fake = FakeGithub()
        before_upload = fake.run

        def change_main_after_upload(*args, **kwargs):
            result = before_upload(*args, **kwargs)
            if args[:3] == ("gh", "release", "upload"):
                self.git("--git-dir", str(self.upstream), "update-ref", "refs/heads/main",
                         self.git("rev-parse", "v0.9.0"))
            return result

        fake.run = change_main_after_upload
        with self.assertRaisesRegex(ValueError, "origin/main の履歴に含まれていません"):
            self.publish(fake)
        self.assertTrue(fake.releases[-1]["draft"])
        self.assertFalse(any("--draft=false" in call for call in fake.calls))

    def test_previous_stable_release_includes_direct_commits_since_before_rc(self):
        releases = [release("v0.9.0"), release(TAG + "-rc.1", prerelease=True, date="2026-10-08T00:00:00Z")]
        current = publisher.tag_commit(TAG, self.root)
        previous, commit = publisher.previous_release(releases, TAG, current, self.root)
        self.assertEqual(previous, "v0.9.0")
        notes = publisher.release_notes("example/svcnest", TAG, current, previous, commit, self.root)
        self.assertIn("PATH の自動設定を追加", notes)
        self.assertIn("引用符と \\[特殊文字\\] を修正", notes)
        self.assertNotIn("- 初期版", notes)
        self.assertIn(f"compare/v0.9.0...{TAG}", notes)
        for row in TARGETS:
            self.assertIn(f"svcnest-{row['target']}{row['extension']}", notes)

    def test_first_release_lists_history_without_a_previous_release(self):
        notes = publisher.release_notes("example/svcnest", TAG, publisher.tag_commit(TAG, self.root), source=self.root)
        self.assertIn("初回リリース", notes)
        self.assertIn("初期版", notes)
        self.assertNotIn("/compare/", notes)

    def test_prerelease_can_compare_against_previous_prerelease(self):
        self.git("tag", "v1.0.0-rc.0", "v0.9.0")
        releases = [release("v0.9.0"), release("v1.0.0-rc.0", prerelease=True, date="2026-10-08T00:00:00Z")]
        previous, _ = publisher.previous_release(releases, TAG + "-rc.1", publisher.tag_commit(TAG, self.root), self.root)
        self.assertEqual(previous, "v1.0.0-rc.0")

    def test_drafts_and_missing_or_unrelated_tags_are_not_comparison_bases(self):
        self.git("checkout", "--orphan", "unrelated")
        self.commit("別の履歴")
        self.git("tag", "v8.0.0")
        self.git("checkout", "--detach", TAG)
        releases = [release("v0.9.0"), release("v8.0.0", date="2026-10-08T00:00:00Z"),
                    release("missing", date="2026-10-09T00:00:00Z"), release(TAG + "-rc.1", draft=True)]
        previous, _ = publisher.previous_release(releases, TAG, publisher.tag_commit(TAG, self.root), self.root)
        self.assertEqual(previous, "v0.9.0")

    def test_missing_or_damaged_assets_block_all_github_operations(self):
        archive = next(self.assets.glob("*.tar.gz"))
        archive.write_bytes(b"damaged")
        fake = FakeGithub()
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            self.publish(fake)
        self.assertEqual(fake.calls, [])
        archive.unlink()
        with self.assertRaisesRegex(ValueError, "不足"):
            self.publish(fake)
        self.assertEqual(fake.calls, [])

    def test_uploads_all_assets_before_publishing(self):
        fake = FakeGithub([release("v0.9.0")])
        self.publish(fake)
        self.assertEqual([call[2] for call in fake.calls if call[1] == "release"], ["create", "upload", "edit"])
        self.assertIn("--draft", fake.calls[1])
        self.assertIn("--verify-tag", fake.calls[1])
        self.assertIn("--draft=false", fake.calls[-1])
        self.assertIn("--latest=true", fake.calls[-1])
        self.assertEqual(len(fake.releases[-1]["assets"]), len(TARGETS) * 2 + 1)
        self.assertFalse(fake.releases[-1]["draft"])
        self.assertEqual(len((self.assets / "SHA256SUMS").read_text().splitlines()), len(TARGETS))

    def test_failed_upload_stays_draft_and_rerun_completes_it(self):
        fake = FakeGithub(fail_upload=True)
        with self.assertRaises(subprocess.CalledProcessError):
            self.publish(fake)
        self.assertTrue(fake.releases[-1]["draft"])
        self.assertFalse(any("--draft=false" in call for call in fake.calls))
        self.publish(fake)
        self.assertFalse(fake.releases[-1]["draft"])
        self.assertEqual(sum(call[1:3] == ("release", "create") for call in fake.calls), 1)

    def test_prerelease_is_not_latest(self):
        tag = TAG + "-rc.1"
        self.git("checkout", "--detach", tag)
        fake = FakeGithub([release("v0.9.0")])
        self.publish(fake, tag)
        self.assertIn("--prerelease", fake.calls[1])
        self.assertIn("--latest=false", fake.calls[-1])
        self.assertIn("--prerelease=true", fake.calls[-1])

    def test_backport_does_not_replace_a_newer_stable_latest(self):
        self.assertFalse(publisher.should_be_latest("v1.0.1", [release("v2.0.0")]))
        self.assertTrue(publisher.should_be_latest("v2.0.1", [release("v2.0.0")]))
        self.assertFalse(publisher.is_prerelease("v2.0.1+build-123"))
        self.assertTrue(publisher.should_be_latest("v2.0.1+build-123", [release("v2.0.0")]))
        self.assertTrue(publisher.is_prerelease("v2.0.1-rc.1+build-123"))

    def test_binary_version_mismatch_blocks_packaging(self):
        package = runpy.run_path(str(SOURCE / "scripts/package-release.py"))
        binary = self.root / "binary"
        binary.write_bytes(b"binary fixture")
        argv = ["package-release.py", "--target", TARGETS[0]["target"], "--binary", str(binary), "--output-dir", str(self.assets)]
        with patch.object(sys, "argv", argv), patch("subprocess.check_output", return_value="svcnest 0.0.0\n"):
            with self.assertRaisesRegex(SystemExit, "一致しません"):
                package["main"]()

    def test_published_release_is_not_modified_or_replaced(self):
        names = [asset.name for asset in publisher.validate_artifacts(self.assets)]
        fake = FakeGithub([release(TAG, assets=names)])
        self.publish(fake)
        self.assertEqual(len(fake.calls), 1)
        fake.releases[0]["assets"].pop()
        with self.assertRaisesRegex(ValueError, "不足する配布物"):
            self.publish(fake)
        self.assertFalse(any(call[1] == "release" for call in fake.calls))

    def test_wrong_checkout_blocks_publication(self):
        self.git("checkout", "--detach", "v0.9.0")
        self.set_version(VERSION)
        fake = FakeGithub()
        with self.assertRaisesRegex(ValueError, "現在のコミット"):
            self.publish(fake)
        self.assertEqual(fake.calls, [])

    def test_tar_and_zip_contain_binary_license_and_valid_checksum(self):
        package = runpy.run_path(str(SOURCE / "scripts/package-release.py"))
        binary = self.root / "binary"
        binary.write_bytes(b"binary fixture")
        # Windows と同様に元ファイルに Unix の実行権限がなくても梱包できる。
        binary.chmod(0o644)
        for row in TARGETS:
            with self.subTest(target=row["target"]):
                argv = ["package-release.py", "--target", row["target"], "--binary", str(binary), "--output-dir", str(self.assets)]
                with patch.object(sys, "argv", argv), patch("subprocess.check_output", return_value=f"svcnest {VERSION}\n"), contextlib.redirect_stdout(io.StringIO()):
                    package["main"]()
                archive = self.assets / f"svcnest-{row['target']}{row['extension']}"
                filename = "svcnest.exe" if row["extension"] == ".zip" else "svcnest"
                if row["extension"] == ".zip":
                    with zipfile.ZipFile(archive) as contents:
                        self.assertEqual(set(contents.namelist()), {filename, "LICENSE"})
                        self.assertEqual(contents.read(filename), binary.read_bytes())
                else:
                    with tarfile.open(archive) as contents:
                        self.assertEqual(set(contents.getnames()), {filename, "LICENSE"})
                        self.assertEqual(contents.extractfile(filename).read(), binary.read_bytes())
                        self.assertEqual(contents.getmember(filename).mode, 0o755)
                        self.assertEqual(contents.getmember("LICENSE").mode, 0o644)
        publisher.validate_artifacts(self.assets)


if __name__ == "__main__":
    unittest.main(verbosity=2)
