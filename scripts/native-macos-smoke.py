"""専用の LaunchAgent を一時登録し、終了時に削除する macOS 実動作検証。"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time


PROGRAM = '''import argparse, fcntl, os, time
p = argparse.ArgumentParser()
p.add_argument("--port", type=int, required=True)
args = p.parse_args()
lock = open("instance.lock", "a")
try:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    print("OVERLAPPING_INSTANCE", flush=True)
    raise SystemExit(42)
print(f"ready port={args.port} pid={os.getpid()}", flush=True)
while True:
    time.sleep(0.1)
'''


def wait_for(condition):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError("Timed out waiting for native daemon state")


def main():
    if sys.platform != "darwin":
        raise SystemExit("This test requires macOS")
    if not shutil.which("uv"):
        raise SystemExit("This test requires uv in PATH")
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/release/svcnest")
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="svcnest-native-", dir="/private/tmp") as temp:
        root = Path(temp)
        home = root / "state"
        project = root / "project with spaces"
        child = project / "src/routes"
        child.mkdir(parents=True)
        (project / "main.py").write_text(PROGRAM)

        def cli(*args, cwd=project, check=True):
            result = subprocess.run([str(binary), "--home", str(home), *args], cwd=cwd, capture_output=True, text=True, timeout=45)
            if check and result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return result

        registration = plistlib.loads(cli("daemon", "install", "--dry-run").stdout.encode())
        label = registration["Label"]
        target = f"gui/{os.getuid()}/{label}"
        digest = 0xcbf29ce484222325
        for byte in str(home.resolve()).encode():
            digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
        runtime = Path(f"/tmp/svcnest-{os.getuid()}-{digest:016x}")
        installed = Path.home() / "Library/LaunchAgents" / f"{label}.plist"
        foreground = None
        try:
            cli("add", "api", "--stop-timeout", "500ms", "--env", "UV_OFFLINE=1", "--env", "UV_PYTHON_DOWNLOADS=never", "--env", f"UV_CACHE_DIR={root / 'uv-cache'}", "--", "uv", "run", "main.py", "--port", "8000")
            cli("enable", "--now")
            # ファイルが残ったまま OS 側の登録だけを外しても enable で復元できる。
            subprocess.run(["launchctl", "bootout", target], check=True, capture_output=True, timeout=30)
            cli("enable", "--now")
            subprocess.run(["launchctl", "print", target], check=True, capture_output=True, timeout=30)

            def status():
                return json.loads(cli("status", "--json", cwd=child).stdout)["services"][0]

            def running():
                value = status()
                return value if value["state"] == "running" else None

            first = wait_for(running)
            assert first["command"] == ["uv", "run", "main.py", "--port", "8000"]
            assert first["cwd"] == str(project.resolve())
            assert first["enabled"]
            wait_for(lambda: "ready port=8000" in cli("logs").stdout)
            cli("daemon", "stop")
            subprocess.run(["launchctl", "kickstart", target], check=True, capture_output=True, timeout=30)
            managed = wait_for(running)
            assert managed["pid"] != first["pid"]
            managed_daemon = int((runtime / "daemon.pid").read_text())
            native_state = subprocess.run(["launchctl", "print", target], check=True, capture_output=True, text=True, timeout=30).stdout
            assert f"pid = {managed_daemon}" in native_state

            # OS による daemon 再起動でも旧 runner と新 runner を重複させない。
            os.kill(managed_daemon, signal.SIGKILL)
            wait_for(lambda: (value := running()) and value["pid"] != managed["pid"])
            assert "OVERLAPPING_INSTANCE" not in cli("logs", "-n", "1000").stdout
            cli("restart", cwd=child)
            cli("stop")

            foreground = subprocess.Popen([str(binary), "--home", str(home), "run"], cwd=project, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_for(lambda: status()["state"] == "foreground")
            foreground.send_signal(signal.SIGINT)
            assert foreground.wait(timeout=10) == 130
            cli("remove", "--stop", "--purge")
            print(json.dumps({"success": True, "checks": ["uv registration", "child directory resolution", "enable --now", "stale OS registration repair", "native LaunchAgent autostart", "native daemon crash recovery", "restart", "foreground Ctrl+C"]}, ensure_ascii=False))
        finally:
            if foreground is not None and foreground.poll() is None:
                foreground.send_signal(signal.SIGINT)
                foreground.wait(timeout=10)
            cli("daemon", "stop", check=False)
            cli("daemon", "uninstall", check=False)
            if installed.exists():
                subprocess.run(["launchctl", "bootout", target], capture_output=True, timeout=30)
                installed.unlink(missing_ok=True)
            shutil.rmtree(runtime, ignore_errors=True)


if __name__ == "__main__":
    main()
