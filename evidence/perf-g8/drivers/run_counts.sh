#!/bin/bash
# usage: run_counts.sh <task_id> <label>
TID=$1; LABEL=$2
perf stat -e probe_data_service:sqlite3_step,probe_data_service:sqlite3_prepare_v2,syscalls:sys_enter_pread64,syscalls:sys_enter_fsync -p 2497521 -- sleep 100 > stat-$LABEL.txt 2>stat-$LABEL.err &
SPID=$!
sleep 2
python3 one_call.py $TID 3 | tee calls-$LABEL.txt
sleep 1
kill -INT $SPID 2>/dev/null; wait $SPID 2>/dev/null
echo "=== stat $LABEL ==="; cat stat-$LABEL.txt
