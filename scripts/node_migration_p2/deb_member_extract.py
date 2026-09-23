#!/usr/bin/env python3
# NM-024 bounded deb member extractor (read-only).
# Usage: deb_member_extract.py <deb-path> <member-path>
# Writes the member bytes to stdout. Fails closed: exits non-zero on
# malformed ar, missing data.tar.*, missing member, or oversize member.
import sys
import lzma
import tarfile
import io

MAX_MEMBER = 64 * 1024 * 1024  # 64 MiB bound


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: deb_member_extract.py <deb> <member-path>", file=sys.stderr)
        return 2
    deb_path, member = sys.argv[1], sys.argv[2]
    data = open(deb_path, "rb").read()
    if data[:8] != b"!<arch>\n":
        print("not an ar archive", file=sys.stderr)
        return 3
    off = 8
    payload = None
    while off + 60 <= len(data):
        header = data[off:off + 60]
        name = header[:16].decode("ascii", "replace").strip()
        size = int(header[48:58].decode("ascii").strip())
        body = data[off + 60:off + 60 + size]
        if name.rstrip("/").startswith("data.tar"):
            payload = body
            break
        off += 60 + size + (size % 2)
    if payload is None:
        print("no data.tar.* member", file=sys.stderr)
        return 4
    raw = lzma.decompress(payload) if payload[:6] == b"\xfd7zXZ\x00" else payload
    with tarfile.open(fileobj=io.BytesIO(raw)) as tf:
        entry = None
        for m in tf.getmembers():
            if m.name in (member, "./" + member, member.lstrip("./")):
                entry = m
                break
        if entry is None:
            print(f"member not found: {member}", file=sys.stderr)
            return 5
        if entry.size > MAX_MEMBER:
            print(f"member too large: {entry.size}", file=sys.stderr)
            return 6
        f = tf.extractfile(entry)
        if f is None:
            print("member is not a regular file", file=sys.stderr)
            return 7
        sys.stdout.buffer.write(f.read())
    return 0


if __name__ == "__main__":
    sys.exit(main())
