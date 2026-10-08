"""実際の status / list 出力を公開 v1 JSON Schema で検証する。"""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

from jsonschema import Draft202012Validator, FormatChecker


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    binary = parser.parse_args().binary.resolve(strict=True)
    schema = json.loads((Path(__file__).resolve().parents[1] / "schema/status-v1.json").read_text())
    Draft202012Validator.check_schema(schema)
    formats = FormatChecker()
    if "date-time" not in formats.checkers:
        raise RuntimeError("Install scripts/requirements-test.txt including format validation")
    validator = Draft202012Validator(schema, format_checker=formats)
    validated = 0
    with tempfile.TemporaryDirectory(prefix="svcnest-schema-") as temporary:
        root = Path(temporary)
        home = root / "state"
        project = root / "project"
        project.mkdir()
        program = project / "probe.py"
        program.write_text("import os,sys,time\nmode=os.environ['SCHEMA_MODE']\nprint('ready',flush=True)\nif mode=='fail': sys.exit(7)\ntime.sleep(2 if mode=='brief' else 120)\n")

        def cli(*args, check=True):
            result = subprocess.run([str(binary), "--home", str(home), *args], cwd=project, capture_output=True, text=True, timeout=45)
            if check and result.returncode:
                raise RuntimeError(f"{args}: {result.stderr}")
            return result

        def snapshot(command="status"):
            nonlocal validated
            value = json.loads(cli(command, "--json").stdout)
            validator.validate(value)
            validated += 1
            return value

        def wait_state(expected):
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                value = snapshot()
                if value["services"][0]["state"] == expected:
                    return value
                time.sleep(0.05)
            raise RuntimeError(f"State {expected} was not observed")

        def register(mode, policy):
            cli("add", "api", "--replace", "--restart", policy, "--stop-timeout", "300ms", "--env", f"SCHEMA_MODE={mode}", "--", sys.executable, str(program))

        foreground = None
        try:
            register("long", "never")
            wait_state("stopped")
            snapshot("list")
            cli("start")
            wait_state("running")
            snapshot("list")
            cli("stop")
            wait_state("stopped")
            register("brief", "never")
            foreground = subprocess.Popen([str(binary), "--home", str(home), "run"], cwd=project, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_state("foreground")
            snapshot("list")
            assert foreground.wait(timeout=15) == 0
            wait_state("stopped")
            register("fail", "never")
            cli("start")
            wait_state("failed")
            snapshot("list")
            register("fail", "always")
            cli("start")
            wait_state("backoff")
            snapshot("list")
            cli("stop")
            wait_state("stopped")
            print(json.dumps({"success": True, "validated_outputs": validated, "states": ["stopped", "running", "foreground", "failed", "backoff"]}))
        finally:
            if foreground is not None and foreground.poll() is None:
                foreground.wait(timeout=15)
            cli("daemon", "stop", check=False)
            if sys.platform != "win32":
                digest = 0xcbf29ce484222325
                for byte in str(home.resolve()).encode():
                    digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
                import os
                shutil.rmtree(f"/tmp/svcnest-{os.geteuid()}-{digest:016x}", ignore_errors=True)


if __name__ == "__main__":
    main()
