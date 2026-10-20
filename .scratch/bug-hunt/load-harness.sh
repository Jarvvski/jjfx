#!/bin/sh
# The load harness the bug-hunt flakes were reproduced with.
#
# Reconstructed: there was never a committed script. Tickets 35 and 42 describe
# the harness only as ad-hoc shell ("many concurrent test binaries plus CPU
# burners"), so this is that harness rebuilt from their descriptions - N
# concurrent copies of one test binary, some continuous full-suite unit-test
# binaries, and CPU burners - with the one thing the ad-hoc version got wrong
# made explicit (see SIGNING below).
#
# Usage:
#   load-harness.sh [rounds] [copies] [unit] [burners] [signing] [filter]
#
#   rounds   default 1     how many rounds to run
#   copies   default 16    concurrent copies of the test binary per round
#   unit     default 3     continuous `cargo test --lib` binaries
#   burners  default 48    `yes > /dev/null` CPU burners
#   signing  default off   `off` sets JJFX_TEST_SIGNING_OFF=1 for the copies,
#                          `on` leaves it unset
#   filter   default the ticket-43 test
#
# SIGNING is the important knob. `mise run test` - the project's gate - sets
# JJFX_TEST_SIGNING_OFF=1, so the engine's `jj` invocations pass
# `--config signing.behavior=drop` and test commits are never signed. A bare
# `cargo test`/test binary does not, so `jj workspace add` signs the workspace's
# working-copy commit on the developer's configured GPG key. On a machine whose
# key is on a smartcard (as this one's is) that is a single serialised device:
# every concurrent copy queues on it, and `jj workspace add` becomes linear in
# the number of concurrent copies. `off` therefore reproduces the *gate*, and
# `on` reproduces the ad-hoc harness that saw the flakes. See ticket 43.
#
# Requires the mise environment (the `jj` on PATH is a mise shim).
set -eu

ROUNDS=${1:-1}
COPIES=${2:-16}
UNIT=${3:-3}
BURNERS=${4:-48}
SIGNING=${5:-off}
FILTER=${6:-concurrent_provisioning_neither_deadlocks_nor_corrupts_the_cache}

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
BIN=$(cd "$ROOT" && mise exec -- cargo test --test repository --no-run 2>&1 \
  | sed -n 's/.*Executable .* (\(.*\))/\1/p')
LIB=$(cd "$ROOT" && mise exec -- cargo test --lib --no-run 2>&1 \
  | sed -n 's/.*Executable .* (\(.*\))/\1/p')
[ -x "$BIN" ] || { echo "could not locate the repository test binary" >&2; exit 1; }
[ -x "$LIB" ] || { echo "could not locate the lib test binary" >&2; exit 1; }

OUT=$(mktemp -d)
echo "binary: $BIN"
echo "logs:   $OUT"

pids=""
for _ in $(seq 1 "$BURNERS"); do
  yes > /dev/null & pids="$pids $!"
done
for _ in $(seq 1 "$UNIT"); do
  ( while :; do (cd "$ROOT" && mise exec -- "$LIB" > /dev/null 2>&1); done ) &
  pids="$pids $!"
done
trap 'for p in $pids; do kill $p 2>/dev/null; done' EXIT

round=1
while [ "$round" -le "$ROUNDS" ]; do
  start=$(date +%s)
  cpids=""
  i=1
  while [ "$i" -le "$COPIES" ]; do
    (
      cd "$ROOT"
      if [ "$SIGNING" = "off" ]; then JJFX_TEST_SIGNING_OFF=1; export JJFX_TEST_SIGNING_OFF; fi
      s=$(date +%s)
      mise exec -- "$BIN" --exact "$FILTER" > "$OUT/r$round-$i.log" 2>&1 && rc=0 || rc=$?
      e=$(date +%s)
      printf '%s %s\n' "$rc" "$((e - s))" > "$OUT/r$round-$i.status"
    ) &
    cpids="$cpids $!"
    i=$((i + 1))
  done
  for p in $cpids; do wait "$p"; done
  end=$(date +%s)

  failures=0
  max=0
  i=1
  while [ "$i" -le "$COPIES" ]; do
    read -r rc w < "$OUT/r$round-$i.status" || { rc=missing; w=0; }
    if [ "$rc" != "0" ]; then failures=$((failures + 1)); fi
    if [ "$w" -gt "$max" ]; then max=$w; fi
    i=$((i + 1))
  done
  echo "round=$round copies=$COPIES unit=$UNIT burners=$BURNERS signing=$SIGNING \
wall=$((end - start))s max_copy=${max}s failures=$failures"
  round=$((round + 1))
done
