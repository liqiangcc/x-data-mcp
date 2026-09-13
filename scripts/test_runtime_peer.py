#!/usr/bin/env python3
"""Root-only bounded UID boundary test on a NEW synthetic runtime directory.

No accounts are created. No source is called. Never targets a deployed database.
"""
import argparse
import json
import os
import pathlib
import pwd
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--bin-dir', type=pathlib.Path, required=True)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise SystemExit('NOT_RUN: this explicit cross-UID test requires root')
    nobody = pwd.getpwnam('nobody')
    assert nobody.pw_uid != 0
    binaries = args.bin_dir.resolve()
    probe = r'''
import socket,sys
s=socket.socket(socket.AF_UNIX);s.settimeout(3)
try:
    s.connect(sys.argv[1])
except PermissionError:
    assert sys.argv[2]=='filesystem'
    print('filesystem_refused')
else:
    assert sys.argv[2]=='peer'
    assert s.recv(1)==b''
    print('peer_refused')
'''
    with tempfile.TemporaryDirectory(prefix='x-data-mcp-peer-') as tmp:
        root = pathlib.Path(tmp)
        db, endpoint = root / 'db.sqlite', root / 'mcp.sock'
        subprocess.run([binaries / 'task_sqlite', 'init', db], check=True, capture_output=True)
        db.chmod(0o600)
        service = subprocess.Popen([binaries / 'data_service', db, endpoint], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 10
            while True:
                assert service.poll() is None, service.stderr.read()
                with socket.socket(socket.AF_UNIX) as check:
                    try:
                        check.connect(str(endpoint))
                        break
                    except (FileNotFoundError, ConnectionRefusedError):
                        assert time.monotonic() < deadline
                        time.sleep(0.02)
            def different_uid(mode):
                result = subprocess.run([sys.executable, '-c', probe, str(endpoint), mode], user=nobody.pw_uid, group=nobody.pw_gid, extra_groups=[], env={"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}, capture_output=True, text=True, timeout=5)
                assert result.returncode == 0, (result.stdout, result.stderr)
                assert result.stdout.strip() == mode + '_refused'
            different_uid('filesystem')
            # Test-only weakening of the private temp fixture allows the second
            # defense (actual SO_PEERCRED) to be exercised independently.
            root.chmod(0o711)
            endpoint.chmod(0o666)
            try:
                different_uid('peer')
            finally:
                endpoint.chmod(0o600)
                root.chmod(0o700)
            request = json.dumps(dict(jsonrpc='2.0', id=1, method='initialize', params={})) + '\n'
            owner = subprocess.run([binaries / 'mcp_connect', endpoint], input=request, capture_output=True, text=True, timeout=5)
            assert owner.returncode == 0, owner.stderr
            assert json.loads(owner.stdout)['result']['serverInfo']['name'] == 'x-data-mcp'
            with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True) as c:
                assert c.execute('select count(*) from requests').fetchone()[0] == 0
                assert c.execute('select count(*) from tasks').fetchone()[0] == 0
        finally:
            service.terminate()
            try:
                status = service.wait(timeout=15)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait()
                raise
            assert status == 0, service.stderr.read()
    print(json.dumps(dict(status='PASS', checks=['actual_foreign_uid_filesystem_refusal', 'actual_foreign_uid_so_peercred_refusal', 'owner_access_preserved_without_business_writes'], live_source='NOT_RUN', chatgpt_consumer='NOT_RUN', production='NOT_PRODUCTION_READY')))


if __name__ == '__main__':
    main()
