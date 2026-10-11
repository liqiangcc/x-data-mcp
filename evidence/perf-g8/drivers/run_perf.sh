#!/bin/bash
D=/root/x-perf-g8/probe-xp011
TID=$1; LABEL=$2
perf record -p 2497521 -g --call-graph dwarf,8192 -F 99 -o $D/perf-$LABEL.data -- sleep 115 2>$D/perf-$LABEL.log &
RP=$!
sleep 2
python3 $D/one_call.py $TID 4 | tee $D/calls-perf-$LABEL.txt
wait $RP
echo "perf record done: $LABEL"
tail -3 $D/perf-$LABEL.log
