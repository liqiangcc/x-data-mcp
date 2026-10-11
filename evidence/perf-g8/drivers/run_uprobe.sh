#!/bin/bash
D=/root/x-perf-g8/probe-xp011
TID=$1; LABEL=$2; N=${3:-4}
perf record -e 'xp011:*' -p 2497521 -o $D/uprobe-$LABEL.data -- sleep 120 2>$D/uprobe-$LABEL.log &
RP=$!
sleep 2
python3 $D/one_call.py $TID $N | tee $D/calls-uprobe-$LABEL.txt
wait $RP
echo "done $LABEL"; tail -2 $D/uprobe-$LABEL.log
