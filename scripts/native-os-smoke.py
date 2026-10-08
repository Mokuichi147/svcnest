"""ユーザーの OS 自動起動を一時登録し、起動・修復・回復を実動作で確認する。"""

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
import xml.etree.ElementTree as ET


PROGRAM = '''import argparse, os, time
p = argparse.ArgumentParser()
p.add_argument("--port", type=int, required=True)
args = p.parse_args()
lock = open("instance.lock", "a+b")
try:
    if os.name == "nt":
        import msvcrt
        lock.write(b"0")
        lock.flush()
        lock.seek(0)
        msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
except OSError:
    print("OVERLAPPING_INSTANCE", flush=True)
    raise SystemExit(42)
print(f"ready port={args.port} pid={os.getpid()}", flush=True)
while True:
    time.sleep(0.1)
'''


def execute(*args, check=True):
    result = subprocess.run(args, capture_output=True, text=True, timeout=45)
    if check and result.returncode:
        raise RuntimeError(f"{args}: {result.stdout} {result.stderr}")
    return result


def wait_for(condition, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError("Timed out waiting for native daemon state")


class Integration:
    def __init__(self, home, definition):
        self.home = home
        digest = 0xcbf29ce484222325
        for byte in str(home).encode():
            digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
        self.label = f"svcnest-{digest:016x}"
        if sys.platform == "darwin":
            registration = plistlib.loads(definition.encode())
            self.label = registration["Label"]
            self.target = f"gui/{os.getuid()}/{self.label}"
            self.installed = Path.home() / "Library/LaunchAgents" / f"{self.label}.plist"
        elif sys.platform == "linux":
            self.target = f"{self.label}.service"
            config = Path(os.environ.get("XDG_CONFIG_HOME", ""))
            if not config.is_absolute():
                config = Path.home() / ".config"
            self.installed = config / "systemd/user" / self.target
            execute("systemctl", "--user", "show-environment")
        else:
            self.target = self.label
            self.installed = home / "daemon-task.xml"
            task = ET.fromstring(definition)
            namespace = {"t": "http://schemas.microsoft.com/windows/2004/02/mit/task"}
            assert task.findtext("t:Principals/t:Principal/t:RunLevel", namespaces=namespace) == "LeastPrivilege"
            assert task.findtext("t:Principals/t:Principal/t:LogonType", namespaces=namespace) == "InteractiveToken"
            user = task.findtext("t:Principals/t:Principal/t:UserId", namespaces=namespace)
            assert user == task.findtext("t:Triggers/t:LogonTrigger/t:UserId", namespaces=namespace)
        self.runtime = home / "runtime" if sys.platform == "win32" else Path(f"/tmp/svcnest-{os.getuid()}-{digest:016x}")

    def detach_registration(self, check=True):
        if sys.platform == "darwin":
            execute("launchctl", "bootout", self.target, check=check)
        elif sys.platform == "linux":
            execute("systemctl", "--user", "disable", "--now", self.target, check=check)
        else:
            execute("schtasks.exe", "/Delete", "/TN", self.target, "/F", check=check)

    def registered(self):
        if sys.platform == "darwin":
            execute("launchctl", "print", self.target)
        elif sys.platform == "linux":
            execute("systemctl", "--user", "is-enabled", self.target)
        else:
            execute("schtasks.exe", "/Query", "/TN", self.target, "/XML")

    def start(self):
        if sys.platform == "darwin":
            execute("launchctl", "kickstart", self.target)
        elif sys.platform == "linux":
            execute("systemctl", "--user", "start", self.target)
        else:
            execute("schtasks.exe", "/Run", "/TN", self.target)

    def assert_managed(self, pid):
        if sys.platform == "darwin":
            assert f"pid = {pid}" in execute("launchctl", "print", self.target).stdout
        elif sys.platform == "linux":
            assert int(execute("systemctl", "--user", "show", self.target, "--property=MainPID", "--value").stdout) == pid
        else:
            # Task の実行状態は言語依存の表示文字列ではなく COM の数値で確認する。
            script = f"$s = New-Object -ComObject Schedule.Service; $s.Connect(); $t = $s.GetFolder('\\').GetTask('{self.target}'); if ($t.State -ne 4) {{ throw 'Task is not running' }}"
            execute("powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script)

    def crash(self, pid):
        if sys.platform == "win32":
            execute("taskkill.exe", "/PID", str(pid), "/F")
        else:
            os.kill(pid, signal.SIGKILL)

    def cleanup(self):
        if self.installed.exists():
            self.detach_registration(check=False)
            self.installed.unlink(missing_ok=True)
            if sys.platform == "linux":
                execute("systemctl", "--user", "daemon-reload", check=False)
                execute("systemctl", "--user", "reset-failed", self.target, check=False)
        if sys.platform != "win32":
            shutil.rmtree(self.runtime, ignore_errors=True)


def interrupt_foreground(foreground):
    if sys.platform != "win32":
        foreground.send_signal(signal.SIGINT)
        return
    # テスト用の新しい console にだけ Ctrl+C を送り、呼び出し側の端末を巻き込まない。
    script = """\
import ctypes, sys, time
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.FreeConsole()
if not kernel.AttachConsole(int(sys.argv[1])):
    raise ctypes.WinError(ctypes.get_last_error())
kernel.SetConsoleCtrlHandler(None, True)
if not kernel.GenerateConsoleCtrlEvent(0, 0):
    raise ctypes.WinError(ctypes.get_last_error())
time.sleep(0.25)
kernel.FreeConsole()
"""
    execute(sys.executable, "-c", script, str(foreground.pid))


def main():
    if sys.platform not in ("darwin", "linux", "win32"):
        raise SystemExit("This test requires macOS, systemd Linux, or Windows")
    if not shutil.which("uv"):
        raise SystemExit("This test requires uv in PATH")
    parser = argparse.ArgumentParser()
    suffix = ".exe" if sys.platform == "win32" else ""
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / f"target/release/svcnest{suffix}")
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="svcnest-native-") as temporary:
        root = Path(temporary)
        home = root / "state with spaces"
        project = root / "project with spaces"
        child = project / "src/routes"
        child.mkdir(parents=True)
        (project / "main.py").write_text(PROGRAM)

        def cli(*args, cwd=project, check=True):
            result = subprocess.run([str(binary), "--home", str(home), *args], cwd=cwd, capture_output=True, text=True, timeout=45)
            if check and result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return result

        canonical_home = Path(json.loads(cli("daemon", "status", "--json").stdout)["home"])
        integration = Integration(canonical_home, cli("daemon", "install", "--dry-run").stdout)
        foreground = None
        try:
            cli("add", "api", "--stop-timeout", "500ms", "--env", "UV_OFFLINE=1", "--env", "UV_PYTHON_DOWNLOADS=never", "--env", f"UV_PYTHON={sys.executable}", "--env", f"UV_CACHE_DIR={root / 'uv-cache'}", "--", "uv", "run", "main.py", "--port", "8000")
            cli("enable", "--now")
            # 定義ファイルが残った状態の OS 登録を、enable が修復できることを確認する。
            integration.detach_registration()
            cli("enable", "--now")
            integration.registered()

            def status():
                return json.loads(cli("status", "--json", cwd=child).stdout)["services"][0]

            def running():
                value = status()
                return value if value["state"] == "running" else None

            def ready_count():
                text = cli("logs", "-n", "1000").stdout
                assert "OVERLAPPING_INSTANCE" not in text
                return text.count("ready port=8000 pid=")

            first = wait_for(running)
            assert first["command"] == ["uv", "run", "main.py", "--port", "8000"]
            assert Path(first["cwd"]).samefile(project)
            assert Path(first["resolved_executable"]).samefile(shutil.which("uv"))
            assert first["enabled"]
            observed_ready = wait_for(ready_count)
            cli("daemon", "stop")
            integration.start()
            managed = wait_for(running)
            assert managed["pid"] != first["pid"]
            observed_ready = wait_for(lambda: (count := ready_count()) > observed_ready and count)
            managed_daemon = int((integration.runtime / "daemon.pid").read_text())
            integration.assert_managed(managed_daemon)
            integration.crash(managed_daemon)
            # Windows の Task Scheduler は最短 1 分の再起動間隔を持つ。
            recovery_timeout = 120 if sys.platform == "win32" else 30
            recovered = wait_for(lambda: (value := running()) and value["pid"] != managed["pid"] and value, timeout=recovery_timeout)
            observed_ready = wait_for(lambda: (count := ready_count()) > observed_ready and count)
            new_daemon = int((integration.runtime / "daemon.pid").read_text())
            assert new_daemon != managed_daemon
            integration.assert_managed(new_daemon)
            assert "OVERLAPPING_INSTANCE" not in cli("logs", "-n", "1000").stdout
            cli("restart", cwd=child)
            restarted = wait_for(running)
            assert restarted["pid"] != recovered["pid"]
            observed_ready = wait_for(lambda: (count := ready_count()) > observed_ready and count)
            cli("stop")
            flags = subprocess.CREATE_NEW_CONSOLE if sys.platform == "win32" else 0
            foreground = subprocess.Popen([str(binary), "--home", str(home), "run"], cwd=project, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=flags)
            wait_for(lambda: status()["state"] == "foreground")
            interrupt_foreground(foreground)
            assert foreground.wait(timeout=10) == 130
            cli("remove", "--stop", "--purge")
            print(json.dumps({"success": True, "platform": sys.platform, "checks": ["uv registration", "child directory resolution", "enable --now", "stale OS registration repair", "native autostart", "native daemon crash recovery", "restart", "foreground Ctrl+C"]}, ensure_ascii=False))
        finally:
            if foreground is not None and foreground.poll() is None:
                interrupt_foreground(foreground)
                foreground.wait(timeout=10)
            cli("daemon", "stop", check=False)
            cli("daemon", "uninstall", check=False)
            integration.cleanup()


if __name__ == "__main__":
    main()
