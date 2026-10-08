"""実時間で再起動間隔、60 秒の安定稼働、10 回制限を確認する。"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time


PROGRAM = """\
import json, sys, time
from pathlib import Path

mode, destination = sys.argv[1:]
events = Path(destination)
previous = events.read_text().splitlines() if events.exists() else []
attempt = sum(json.loads(line)['phase'] == 'start' for line in previous) + 1

def record(phase):
    with events.open('a') as output:
        output.write(json.dumps({'attempt': attempt, 'phase': phase, 'time': time.monotonic()}) + '\\n')

record('start')
if mode == 'reset' and attempt == 3:
    time.sleep(61)
elif mode == 'reset' and attempt == 4:
    time.sleep(240)
else:
    time.sleep(0.05)
record('exit')
sys.exit(7)
"""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="svcnest-policy-") as temporary:
        root = Path(temporary)
        home = root / "state"
        program = root / "probe.py"
        program.write_text(PROGRAM)
        destinations = {mode: root / f"{mode}.jsonl" for mode in ("limit", "reset")}

        def cli(*args, check=True):
            result = subprocess.run([str(binary), "--home", str(home), *args], cwd=root, capture_output=True, text=True, timeout=45)
            if check and result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return result

        def events(mode):
            path = destinations[mode]
            return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

        def interval(observed, attempt, expected):
            previous = next(event for event in observed if event["attempt"] == attempt - 1 and event["phase"] == "exit")
            current = next(event for event in observed if event["attempt"] == attempt and event["phase"] == "start")
            elapsed = current["time"] - previous["time"]
            assert expected - 0.1 <= elapsed <= expected + 10, (attempt, expected, elapsed)
            return round(elapsed, 3)

        try:
            # restart の指定を省略し、標準の on-failure を実行する。
            for mode, destination in destinations.items():
                cli("add", mode, "--stop-timeout", "500ms", "--", sys.executable, str(program), mode, str(destination))
                cli("start", mode)
            deadline = time.monotonic() + 270
            reset_checked = False
            limit_checked = False
            limit_intervals = []
            last_count = -1
            while time.monotonic() < deadline:
                limited = events("limit")
                count = sum(event["phase"] == "start" for event in limited)
                if count != last_count:
                    print(json.dumps({"check": "restart-limit", "attempts": count}), flush=True)
                    last_count = count
                if not reset_checked:
                    reset = events("reset")
                    if any(event["attempt"] == 4 and event["phase"] == "start" for event in reset):
                        initial_intervals = [interval(reset, 2, 1), interval(reset, 3, 2)]
                        stable_start = next(event["time"] for event in reset if event["attempt"] == 3 and event["phase"] == "start")
                        stable_exit = next(event["time"] for event in reset if event["attempt"] == 3 and event["phase"] == "exit")
                        assert stable_exit - stable_start >= 60
                        reset_interval = interval(reset, 4, 1)
                        cli("stop", "reset")
                        status = json.loads(cli("status", "reset", "--json").stdout)["services"][0]
                        assert status["state"] == "stopped" and status["pid"] is None
                        reset_checked = True
                        print(json.dumps({"check": "stable-reset", "initial_intervals": initial_intervals, "stable_seconds": round(stable_exit - stable_start, 3), "reset_interval": reset_interval}), flush=True)
                if len(limited) == 22:
                    status = json.loads(cli("status", "limit", "--json").stdout)["services"][0]
                    if status["state"] == "failed":
                        assert status["reason"] == "restart-limit"
                        assert status["restarts"] == 10 and status["pid"] is None
                        assert status["last_exit_code"] == 7
                        expected = [1, 2, 4, 8, 16, 30, 30, 30, 30, 30]
                        limit_intervals = [interval(limited, attempt, delay) for attempt, delay in enumerate(expected, 2)]
                        limit_checked = True
                        break
                time.sleep(0.1)
            assert reset_checked and limit_checked, "Restart policy verification did not finish within 270 seconds"
            time.sleep(2)
            assert len(events("limit")) == 22
            assert sum(event["phase"] == "start" for event in events("reset")) == 4
            print(json.dumps({"success": True, "check": "restart-policy", "limit_intervals": limit_intervals, "restarts": 10, "reason": "restart-limit"}), flush=True)
        finally:
            cli("daemon", "stop", check=False)
            if sys.platform != "win32" and home.exists():
                digest = 0xcbf29ce484222325
                for byte in str(home.resolve()).encode():
                    digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
                shutil.rmtree(f"/tmp/svcnest-{os.geteuid()}-{digest:016x}", ignore_errors=True)


if __name__ == "__main__":
    main()
