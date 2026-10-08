"""macOS でサービスを動かしたまま異なるバージョンを cargo install する。"""

import ctypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib


def executable_path(pid):
    library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    library.proc_pidpath.restype = ctypes.c_int
    buffer = ctypes.create_string_buffer(4096)
    if library.proc_pidpath(pid, buffer, len(buffer)) <= 0:
        raise OSError(ctypes.get_errno(), "proc_pidpath failed")
    return Path(os.fsdecode(buffer.value))


def wait_for(condition):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError("Timed out waiting for installation update")


def main():
    if sys.platform != "darwin":
        raise SystemExit("This test requires macOS")
    source = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory(prefix="svcnest-install-update-") as temporary:
        root = Path(temporary)
        home = root / "state with spaces"
        install = root / "install with spaces"
        binary = install / "bin/svcnest"
        updated_source = root / "updated source"
        updated_source.mkdir()
        shutil.copytree(source / "src", updated_source / "src")
        manifest = (source / "Cargo.toml").read_text()
        original_version = tomllib.loads(manifest)["package"]["version"]
        major, minor, patch = original_version.split("-")[0].split(".")
        updated_version = f"{major}.{minor}.{int(patch) + 1}"
        (updated_source / "Cargo.toml").write_text(manifest.replace(f'version = "{original_version}"', f'version = "{updated_version}"', 1))
        lock = (source / "Cargo.lock").read_text()
        (updated_source / "Cargo.lock").write_text(lock.replace(f'name = "svcnest"\nversion = "{original_version}"', f'name = "svcnest"\nversion = "{updated_version}"', 1))

        def cargo_install(project):
            subprocess.run(["cargo", "install", "--offline", "--locked", "--force", "--path", str(project), "--root", str(install), "--target-dir", str(root / "build")], check=True, timeout=300)

        def cli(*args, check=True):
            result = subprocess.run([str(binary), "--home", str(home), *args], cwd=root, capture_output=True, text=True, timeout=45)
            if check and result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return result

        def service():
            return json.loads(cli("status", "api", "--json").stdout)["services"][0]

        runtime = None
        try:
            cargo_install(source)
            program = root / "probe.py"
            program.write_text("import os,time\nprint(f'install-update-ready pid={os.getpid()}',flush=True)\nwhile True: time.sleep(0.1)\n")
            cli("add", "api", "--restart", "never", "--stop-timeout", "500ms", "--", sys.executable, str(program))
            digest = 0xcbf29ce484222325
            for byte in str(home.resolve()).encode():
                digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
            runtime = Path(f"/tmp/svcnest-{os.geteuid()}-{digest:016x}")
            cli("start", "api")
            before = wait_for(lambda: (value := service())["state"] == "running" and value)
            wait_for(lambda: "install-update-ready" in cli("logs", "api").stdout)
            daemon_before = int((runtime / "daemon.pid").read_text())
            old_executable = executable_path(daemon_before)
            old_bytes = old_executable.read_bytes()
            assert old_executable.parent.parent == home.resolve() / "bin"
            cargo_install(updated_source)
            assert cli("--version").stdout.strip() == f"svcnest {updated_version}"
            cli("start", "api")
            assert service()["state"] == "running" and service()["pid"] == before["pid"]
            assert int((runtime / "daemon.pid").read_text()) == daemon_before
            assert executable_path(daemon_before) == old_executable
            assert old_executable.read_bytes() == old_bytes
            os.kill(before["pid"], 0)
            assert cli("logs", "api").stdout.count("install-update-ready") == 1
            cli("daemon", "stop")
            cli("start", "api")
            after = wait_for(lambda: (value := service())["state"] == "running" and value)
            daemon_after = int((runtime / "daemon.pid").read_text())
            new_executable = executable_path(daemon_after)
            assert daemon_after != daemon_before and after["pid"] != before["pid"]
            assert new_executable != old_executable
            assert new_executable.read_bytes() == binary.read_bytes()
            assert old_executable.read_bytes() == old_bytes
            version = subprocess.run([str(new_executable), "--version"], check=True, capture_output=True, text=True, timeout=30)
            assert version.stdout.strip() == f"svcnest {updated_version}"
            print(json.dumps({"success": True, "check": "macOS cargo install updates without stopping services", "from_version": original_version, "to_version": updated_version, "daemon_pid_preserved": daemon_before, "service_pid_preserved": before["pid"], "new_runtime_selected_after_restart": True}), flush=True)
        finally:
            if binary.exists() and home.exists():
                cli("daemon", "stop", check=False)
            if runtime is not None:
                shutil.rmtree(runtime, ignore_errors=True)


if __name__ == "__main__":
    main()
