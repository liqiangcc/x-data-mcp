import json, socket, sys, time
SOCK="/root/x-perf-g8/svc/run/mcp.sock"
s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(SOCK)
f=s.makefile("r",encoding="utf-8"); n=0
def call(name,args):
    global n; n+=1
    s.sendall((json.dumps({"jsonrpc":"2.0","id":n,"method":"tools/call","params":{"name":name,"arguments":args}},separators=(",",":"))+"\n").encode())
    while True:
        r=json.loads(f.readline())
        if r.get("id")!=n: continue
        return r["result"].get("structuredContent")
tid=sys.argv[1]; reps=int(sys.argv[2]) if len(sys.argv)>2 else 1
res=[]
for i in range(reps):
    t0=time.time(); p=call("get_task_progress",{"task_id":tid,"page_size":100}); ms=(time.time()-t0)*1000
    res.append({"i":i,"ms":round(ms,1)})
    print(f"call {i}: {ms:.0f}ms", flush=True)
print(json.dumps(res))
