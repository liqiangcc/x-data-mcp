#!/usr/bin/env bash
set -euo pipefail

[[ $# == 1 && "$1" =~ ^[a-zA-Z0-9][a-zA-Z0-9._-]*$ ]] \
    || { echo "usage: $0 <run-id>" >&2; exit 2; }

run_id=$1
root=$(git rev-parse --show-toplevel)
session="x-data-ai-$run_id"
prefix="/tmp/x-data-daily-sync-$run_id"
runtime_config="$prefix-config.json"
output="$prefix-output.txt"
conversation="$prefix-conversation.json"
exit_code="$prefix-exit-code.txt"

python3 "$root/scripts/check_daily_sync_eval.py"
[[ -z "$(git -C "$root" status --porcelain)" ]] \
    || { echo "worktree must be clean" >&2; exit 1; }
tmux has-session -t "$session" 2>/dev/null \
    && { echo "tmux session already exists: $session" >&2; exit 1; }
for path in "$runtime_config" "$output" "$conversation" "$exit_code"; do
    [[ ! -e "$path" ]] || { echo "output already exists: $path" >&2; exit 1; }
done

cp "$root/.devin/evals/daily-sync-config.json" "$runtime_config"
tmux new-session -d -s "$session" \
    "cd '$root'; set +e; devin --config '$runtime_config' --permission-mode auto -p --prompt-file '$root/.devin/evals/daily-ai-sync-v2.txt' --export '$conversation' > '$output' 2>&1; code=\$?; printf '%s\\n' \"\$code\" > '$exit_code'; exit \"\$code\""

printf 'session=%s\noutput=%s\nconversation=%s\nexit_code=%s\nruntime_config=%s\n' \
    "$session" "$output" "$conversation" "$exit_code" "$runtime_config"
