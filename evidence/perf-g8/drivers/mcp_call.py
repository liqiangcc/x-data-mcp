#!/usr/bin/env python3
"""usage: mcp_call.py <sock> <tool> '<json-args>' [--list]"""
import json, socket, sys
sock_path, tool = sys.argv[1], sys.argv[2]
args = json.loads(sys.argv[3]) if len(sys.argv) > 3 else {}
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.connect(sock_path)
f = s.makefile("r", encoding="utf-8")
if tool == "--list":
    req = {"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
else:
    req = {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}
s.sendall((json.dumps(req,separators=(",",":"))+"\n").encode())
while True:
    line = f.readline()
    if not line: print("EOF"); sys.exit(2)
    resp = json.loads(line)
    if resp.get("id") != 1: continue
    print(json.dumps(resp, indent=1, ensure_ascii=False)); break
