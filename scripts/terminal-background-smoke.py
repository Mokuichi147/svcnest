"""端末セッションを閉じても background サービスが稼働し続けることを確認する。"""

import argparse
import json
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="svcnest-terminal-") as temporary:
        root = Path(temporary)
        home = root / "state"
        project = root / "project"
        project.mkdir()
        program = project / "probe.py"
        program.write_text("import time\nprint('background-ready',flush=True)\nwhile True: time.sleep(0.1)\n")

        def cli(*args, check=True):
            result = subprocess.run([str(binary), "--home", str(home), *args], cwd=project, capture_output=True, text=True, timeout=30)
            if check and result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return result

        session, master = pty.fork()
        if session == 0:
            try:
                cli("add", "api", "--restart", "never", "--", sys.executable, str(program))
                cli("start")
                print("TERMINAL-READY", flush=True)
                while True:
                    time.sleep(1)
            except Exception as error:
                print(f"TERMINAL-ERROR {error}", flush=True)
                os._exit(1)
        data = bytearray()
        try:
            deadline = time.monotonic() + 30
            while b"TERMINAL-READY" not in data:
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"Terminal startup timed out: {data.decode(errors='replace')}")
                if select.select([master], [], [], 0.2)[0]:
                    data.extend(os.read(master, 8192))
            before = json.loads(cli("status", "--json").stdout)["services"][0]
            assert before["state"] == "running"
            os.close(master)
            master = None
            # 接続元の端末セッションは終了し、daemon / runner / target は独立して残る。
            deadline = time.monotonic() + 10
            while not os.waitpid(session, os.WNOHANG)[0]:
                if time.monotonic() >= deadline:
                    raise RuntimeError("Terminal session did not end after disconnect")
                time.sleep(0.05)
            session = None
            after = json.loads(cli("status", "--json").stdout)["services"][0]
            assert after["state"] == "running" and after["pid"] == before["pid"]
            os.kill(after["pid"], 0)
            cli("stop")
            print(json.dumps({"success": True, "check": "background survives controlling-terminal disconnect"}))
        finally:
            if master is not None:
                os.close(master)
            if session is not None:
                try:
                    os.kill(session, 15)
                    os.waitpid(session, 0)
                except ProcessLookupError:
                    pass
            cli("daemon", "stop", check=False)
            if home.exists():
                digest = 0xcbf29ce484222325
                for byte in str(home.resolve()).encode():
                    digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
                shutil.rmtree(f"/tmp/svcnest-{os.geteuid()}-{digest:016x}", ignore_errors=True)


if __name__ == "__main__":
    main()
