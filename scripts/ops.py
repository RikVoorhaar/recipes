#!/usr/bin/env python3
"""Safe filesystem and SQLite operations for recipe-app."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tarfile
import tempfile


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def resolved(path: Path) -> Path:
    return path.resolve(strict=False)


def overlaps(left: Path, right: Path) -> bool:
    left, right = resolved(left), resolved(right)
    return left == right or left in right.parents or right in left.parents


def require_disjoint(path: Path, sources: tuple[Path, ...], description: str) -> None:
    for source in sources:
        if overlaps(path, source):
            raise ValueError(f"{description} overlaps source path {source}")


def backup(database: Path, content: Path, output: Path) -> None:
    if not database.is_file() or not content.is_dir():
        raise ValueError("source database and content directory must exist")
    sources = (database, content)
    require_disjoint(output, sources, "backup destination")
    if output.exists() or output.is_symlink():
        raise ValueError(f"backup destination already exists: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".recipe-backup-", dir=output.parent) as temp:
        stage = Path(temp)
        require_disjoint(stage, sources, "backup staging directory")
        require_disjoint(stage, (output,), "backup staging directory")
        db_copy = stage / "recipes.sqlite3"
        with sqlite3.connect(database) as source, sqlite3.connect(db_copy) as target:
            source.backup(target)
        with sqlite3.connect(f"file:{db_copy}?mode=ro", uri=True) as check:
            if check.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise ValueError("SQLite integrity check failed")
        content_archive = stage / "content.tar.gz"
        with tarfile.open(content_archive, "w:gz") as archive:
            for path in sorted(content.rglob("*")):
                if path.is_symlink():
                    raise ValueError(f"symlinks are not supported in content: {path}")
                if path.is_file():
                    archive.add(path, arcname=path.relative_to(content))
        manifest = {
            "format": 1,
            "database_sha256": digest(db_copy),
            "content_archive_sha256": digest(content_archive),
        }
        (stage / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        os.chmod(stage, 0o700)
        os.replace(stage, output)


def restore(backup_dir: Path, destination: Path) -> None:
    if destination.exists() or destination.is_symlink():
        raise ValueError(f"restore destination must not exist: {destination}")
    manifest = json.loads((backup_dir / "manifest.json").read_text())
    database = backup_dir / "recipes.sqlite3"
    archive = backup_dir / "content.tar.gz"
    if digest(database) != manifest["database_sha256"]:
        raise ValueError("database checksum mismatch")
    if digest(archive) != manifest["content_archive_sha256"]:
        raise ValueError("content archive checksum mismatch")
    destination.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=f".{destination.name}.restore-", dir=destination.parent))
    try:
        shutil.copy2(database, stage / "recipes.sqlite3")
        content = stage / "content"
        content.mkdir()
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                target = (content / member.name).resolve()
                if not target.is_relative_to(content.resolve()) or not (member.isfile() or member.isdir()):
                    raise ValueError("unsafe member in content archive")
            tar.extractall(content)
        with sqlite3.connect(stage / "recipes.sqlite3") as check:
            if check.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                raise ValueError("restored SQLite integrity check failed")
        os.replace(stage, destination)
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    make = commands.add_parser("backup")
    make.add_argument("--database", type=Path, required=True)
    make.add_argument("--content", type=Path, required=True)
    make.add_argument("--output", type=Path, required=True)
    put_back = commands.add_parser("restore")
    put_back.add_argument("--backup", type=Path, required=True)
    put_back.add_argument("--destination", type=Path, required=True)
    copy = commands.add_parser("dev-copy")
    copy.add_argument("--database", type=Path, required=True)
    copy.add_argument("--content", type=Path, required=True)
    copy.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "backup":
            backup(args.database, args.content, args.output)
        elif args.command == "restore":
            restore(args.backup, args.destination)
        else:
            sources = (args.database, args.content)
            require_disjoint(args.destination, sources, "development destination")
            if args.destination.exists() or args.destination.is_symlink():
                raise ValueError(f"development destination already exists: {args.destination}")
            args.destination.parent.mkdir(parents=True, exist_ok=True)
            with tempfile.TemporaryDirectory(prefix=".recipe-dev-copy-", dir=args.destination.parent) as temp:
                staging = Path(temp)
                require_disjoint(staging, sources, "development staging directory")
                require_disjoint(staging, (args.destination,), "development staging directory")
                bundle = staging / "backup"
                prepared = staging / "prepared"
                backup(args.database, args.content, bundle)
                restore(bundle, prepared)
                # Scrub credentials before the complete tree becomes visible.
                with sqlite3.connect(prepared / "recipes.sqlite3") as connection:
                    connection.execute("DELETE FROM api_tokens")
                    if connection.execute("PRAGMA integrity_check").fetchone()[0] != "ok":
                        raise ValueError("development SQLite integrity check failed")
                os.replace(prepared, args.destination)
        return 0
    except (OSError, ValueError, sqlite3.Error, tarfile.TarError, KeyError) as error:
        print(f"ops: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
