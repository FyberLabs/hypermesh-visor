#!/usr/bin/env python3
"""Scoped flat-file publication with no symlinks, CAS, and atomic replacement.

Mount-level read-only grants and filesystem quotas are the security boundary.
The helper is the cooperative conflict protocol, not a sandbox for a write grantee.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import stat
import sys

MAX_FILE = 16 * 1024 * 1024
MAX_TOTAL = 240 * 1024 * 1024  # leave space within the example's 256 MiB mount
MAX_ENTRIES = 4096

def component(value):
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}', value):
        raise ValueError('invalid resource or file name; flat names only')
    return value

def read_file(root, name):
    try:
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=root)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > MAX_FILE:
            raise ValueError('resource must be a bounded regular file with one link')
        data = stream.read(MAX_FILE + 1)
        if len(data) > MAX_FILE:
            raise ValueError('resource exceeds size limit')
        return data

def digest(data):
    return 'missing' if data is None else hashlib.sha256(data).hexdigest()

def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('get', 'put'))
    parser.add_argument('resource', type=component)
    parser.add_argument('name', type=component)
    parser.add_argument('--expected', help='put requires missing or the current SHA-256')
    args = parser.parse_args()
    if args.action == 'put' and not (args.expected == 'missing' or re.fullmatch(r'[0-9a-f]{64}', args.expected or '')):
        parser.error('put requires --expected missing or SHA-256')
    # Reject resource-root symlinks as well as file symlinks.
    base = os.open('/resources', os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        root = os.open(args.resource, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=base)
    finally:
        os.close(base)
    try:
        if args.action == 'get':
            data = read_file(root, args.name)
            if data is None:
                raise ValueError('resource file not found')
            sys.stdout.buffer.write(data)
            return
        data = sys.stdin.buffer.read(MAX_FILE + 1)
        if len(data) > MAX_FILE:
            raise ValueError('publication exceeds file limit')
        lockfd = os.open('.visor.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o660, dir_fd=root)
        try:
            if not stat.S_ISREG(os.fstat(lockfd).st_mode) or os.fstat(lockfd).st_nlink != 1:
                raise ValueError('invalid resource lock')
            fcntl.flock(lockfd, fcntl.LOCK_EX)
            previous = read_file(root, args.name)
            if digest(previous) != args.expected:
                raise ValueError('publication conflict; read and reconcile before retrying')
            total = count = 0
            for name in os.listdir(root):
                info = os.stat(name, dir_fd=root, follow_symlinks=False)
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                    raise ValueError('resource contains unsupported entry')
                count += 1
                total += info.st_size
            if count >= MAX_ENTRIES or total - len(previous or b'') + len(data) > MAX_TOTAL:
                raise ValueError('resource publication quota exceeded')
            temporary = '.visor-publish-' + secrets.token_hex(16)
            try:
                fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o660, dir_fd=root)
                with os.fdopen(fd, 'wb') as stream:
                    stream.write(data)
                    stream.flush()
                    os.fsync(stream.fileno())
                os.rename(temporary, args.name, src_dir_fd=root, dst_dir_fd=root)
                os.fsync(root)
            finally:
                try:
                    os.unlink(temporary, dir_fd=root)
                except FileNotFoundError:
                    pass
            print(json.dumps({'sha256': digest(data), 'bytes': len(data)}))
        finally:
            os.close(lockfd)
    finally:
        os.close(root)

if __name__ == '__main__':
    try:
        run()
    except (OSError, ValueError) as error:
        # Never include file contents or expanded paths in diagnostics.
        print(str(error) if isinstance(error, ValueError) else 'resource access refused', file=sys.stderr)
        raise SystemExit(1)
