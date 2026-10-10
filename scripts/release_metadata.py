"""リリースのバージョンと配布対象を共通の定義から読み込む。"""

import argparse
import json
from pathlib import Path
import tomllib


SOURCE = Path(__file__).resolve().parents[1]


def release_targets():
    return json.loads((SOURCE / ".github/release-targets.json").read_text(encoding="utf-8"))


def release_version():
    manifest = tomllib.loads((SOURCE / "Cargo.toml").read_text(encoding="utf-8"))
    lock = tomllib.loads((SOURCE / "Cargo.lock").read_text(encoding="utf-8"))
    name, version = manifest["package"]["name"], manifest["package"]["version"]
    if not any(package["name"] == name and package["version"] == version for package in lock["package"]):
        raise ValueError("Cargo.toml と Cargo.lock のパッケージバージョンが一致しません")
    return version


def validate_tag(tag):
    version = release_version()
    if tag != f"v{version}":
        raise ValueError(f"タグ {tag} は Cargo.toml のバージョン {version} と一致しません")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    args = parser.parse_args()
    validate_tag(args.tag)
    print(json.dumps({"include": release_targets()}, ensure_ascii=True))


if __name__ == "__main__":
    main()
