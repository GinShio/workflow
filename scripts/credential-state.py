#!/usr/bin/env python3
"""Create workflow credential state atomically with private permissions."""

from __future__ import annotations

import argparse
import base64
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import stat
import tempfile
from typing import Iterator


def ensure_private_parent(path: Path) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)


def atomic_write(path: Path, text: str) -> None:
    ensure_private_parent(path)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary_path = Path(temporary)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary_path, path)
        os.chmod(path, 0o600)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    except BaseException:
        try:
            os.close(fd)
        except OSError:
            pass
        temporary_path.unlink(missing_ok=True)
        raise


@contextmanager
def exclusive_lock(path: Path) -> Iterator[None]:
    ensure_private_parent(path)
    lock_path = path.with_name(f"{path.name}.lock")
    flags = (
        os.O_CREAT
        | os.O_RDWR
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )
    fd = os.open(lock_path, flags, 0o600)
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError(f"{lock_path} is not a regular file")
        os.fchmod(fd, 0o600)
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def read_regular_file(path: Path) -> str:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    fd = os.open(path, flags)
    metadata = os.fstat(fd)
    if not stat.S_ISREG(metadata.st_mode):
        os.close(fd)
        raise ValueError(f"{path} is not a regular file")
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, encoding="utf-8") as stream:
        value = stream.read().strip()
    if not value:
        raise ValueError(f"{path} is empty")
    return value


def persistent_secret_unlocked(path: Path, byte_count: int) -> str:
    if path.exists() or path.is_symlink():
        return read_regular_file(path)
    value = secrets.token_hex(byte_count)
    atomic_write(path, f"{value}\n")
    return value


def persistent_secret(path: Path, byte_count: int) -> str:
    with exclusive_lock(path):
        return persistent_secret_unlocked(path, byte_count)


def qbittorrent_hash(secret_path: Path, cache_path: Path) -> str:
    password = persistent_secret(secret_path, 12)
    source_hash = hashlib.sha256(password.encode()).hexdigest()

    with exclusive_lock(cache_path):
        if cache_path.exists() or cache_path.is_symlink():
            try:
                cached = json.loads(read_regular_file(cache_path))
                if (
                    cached.get("source_sha256") == source_hash
                    and isinstance(cached.get("encoded"), str)
                    and cached["encoded"]
                ):
                    return cached["encoded"]
            except (json.JSONDecodeError, OSError, TypeError, ValueError):
                pass

        salt = os.urandom(16)
        derived = hashlib.pbkdf2_hmac(
            "sha512", password.encode(), salt, 100_000
        )
        encoded = (
            f"@ByteArray({base64.b64encode(salt).decode()}:"
            f"{base64.b64encode(derived).decode()})"
        )
        atomic_write(
            cache_path,
            json.dumps(
                {"source_sha256": source_hash, "encoded": encoded},
                sort_keys=True,
            )
            + "\n",
        )
        return encoded


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subcommands = parser.add_subparsers(dest="command", required=True)

    secret_parser = subcommands.add_parser("secret")
    secret_parser.add_argument("path", type=Path)
    secret_parser.add_argument("--bytes", type=int, default=12)

    qbittorrent_parser = subcommands.add_parser("qbittorrent-hash")
    qbittorrent_parser.add_argument("secret_path", type=Path)
    qbittorrent_parser.add_argument("cache_path", type=Path)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.command == "secret":
        if args.bytes <= 0:
            raise ValueError("--bytes must be positive")
        result = persistent_secret(args.path, args.bytes)
    else:
        result = qbittorrent_hash(args.secret_path, args.cache_path)
    print(result)


if __name__ == "__main__":
    main()
