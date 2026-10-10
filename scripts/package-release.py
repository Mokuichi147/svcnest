"""ネイティブでビルドした実行ファイルと SHA-256 をリリース用に梱包する。"""

import argparse
import hashlib
from pathlib import Path
import subprocess
import tarfile
import zipfile

from release_metadata import release_targets, release_version


TARGETS = tuple(row["target"] for row in release_targets())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--output-dir", type=Path, default=Path("dist"))
    args = parser.parse_args()
    source = Path(__file__).resolve().parents[1]
    version = release_version()
    windows = args.target.endswith("windows-msvc")
    filename = "svcnest.exe" if windows else "svcnest"
    binary = (args.binary or source / "target" / args.target / "release" / filename).resolve(strict=True)
    actual = subprocess.check_output([str(binary), "--version"], text=True, timeout=30).strip()
    if actual != f"svcnest {version}":
        raise SystemExit(f"バイナリと Cargo.toml のバージョンが一致しません: {actual} / {version}")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    extension = next(row["extension"] for row in release_targets() if row["target"] == args.target)
    archive = args.output_dir / f"svcnest-{args.target}{extension}"
    if windows:
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as package:
            package.write(binary, filename)
            package.write(source / "LICENSE", "LICENSE")
    else:
        with tarfile.open(archive, "w:gz") as package:
            package.add(binary, arcname=filename)
            package.add(source / "LICENSE", arcname="LICENSE")
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
    print(archive)


if __name__ == "__main__":
    main()
