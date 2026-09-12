#!/usr/bin/env python3
import json, sys
from datetime import datetime, timezone
from importlib.metadata import version
from eltdx import TdxClient
symbol=sys.argv[1]
period=sys.argv[2]
market_symbol={"000001":"sz000001","600000":"sh600000"}.get(symbol)
if market_symbol is None: raise SystemExit(f"unsupported symbol: {symbol}")
if period not in {"day", "year"}: raise SystemExit(f"unsupported period: {period}")
client=TdxClient()
try:
    series=client.bars.get(market_symbol, period=period, adjust="qfq", count=50)
    bars=[{"date":b.time.date().isoformat(),"open":str(b.open),"high":str(b.high),"low":str(b.low),"close":str(b.close),"volume":str(b.volume_lots),"amount":str(b.amount)} for b in series.bars]
    print(json.dumps({"source":"tdx","source_period":period,"source_adjustment":"qfq","symbol":symbol,"acquired_at":datetime.now(timezone.utc).isoformat().replace("+00:00","Z"),"adapter_version":version("eltdx"),"bars":bars},ensure_ascii=False))
finally:
    try: client.close()
    except Exception: pass
