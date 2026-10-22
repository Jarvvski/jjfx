#!/bin/sh
# Ticket 44 harness: do concurrent jj operations across Workspaces diverge the
# shared operation log and swallow a committed change?
#
# jj's operation log is lock-free: many `jj` processes share one repository and
# one log, each sees a consistent view, and jj reconciles divergent operations on
# the next command. jjfx is built to drive exactly that. This harness drives the
# jj operations jjfx issues concurrently across N Workspaces and reports, per
# round, whether a change id has ended up **divergent** (two commits, one change
# id - the shape the `dogfood/13` sighting saw) or whether a marker a Workspace
# committed has stopped being reachable.
#
# Usage: op-log-harness.sh [rounds] [workspaces] [mode] [undos]
#   rounds      default 15   rounds to run
#   workspaces  default 6    concurrent Workspaces, each committing every round
#   mode        default mixed
#                 mixed       the jjfx operation set: prepare/new+commit, weld,
#                             absorb, reset, describe, plus repository-wide tidy
#                             and lift_all
#                 undo-cross  hammer `jj undo --allow-cross-workspace` (the flag
#                             jjfx's rollback used to pass) while Workers commit
#                 undo-plain  hammer a plain `jj undo` (the fix) - jj refuses a
#                             cross-Workspace undo, so it cannot diverge a sibling
#   undos       default 15   repository-wide undos per round (undo-cross/plain)
#
# Exit status is 0 when no divergence and no lost marker was seen.
#
# Reproduces the ticket-44 finding: `undo-cross` diverges a Worker's commit in a
# few rounds; `undo-plain` does not (a plain undo only undoes an operation created
# in the Workspace it runs in, and refuses otherwise).
set -u

ROUNDS=${1:-15}
WORKSPACES=${2:-6}
MODE=${3:-mixed}
UNDOS=${4:-15}

BASE=$(mktemp -d "${TMPDIR:-/tmp}/jjfx-oplog-XXXXXX")
cat > "$BASE/jj.toml" <<'CFG'
[user]
name = "jjfx op-log harness"
email = "harness@example.com"
[signing]
behavior = "drop"
# A local `main` stands in for `trunk()` so no network fetch is needed; the
# repository never inherits the developer's jj config.
[revset-aliases]
"trunk()" = "main"
CFG
export JJ_CONFIG=$BASE/jj.toml
export JJFX_TEST_SIGNING_OFF=1

REPO=$BASE/repo
mkdir -p "$REPO"
cd "$REPO" || exit 1
jj git init --colocate . >/dev/null 2>&1
printf 'seed\n' > seed.txt
jj describe -m seed >/dev/null 2>&1
jj bookmark create main -r @ >/dev/null 2>&1

echo "repo: $REPO"
echo "mode: $MODE workspaces: $WORKSPACES rounds: $ROUNDS undos: $UNDOS"

WORKSPACES_DIRS=""
i=1
while [ "$i" -le "$WORKSPACES" ]; do
  jj workspace add --name "w$i" "$BASE/w$i" >/dev/null 2>&1
  WORKSPACES_DIRS="$WORKSPACES_DIRS $BASE/w$i"
  i=$((i + 1))
done

# Each Workspace commits a marker it is then asked to keep. A round must never
# make that content unreachable.
for d in $WORKSPACES_DIRS; do
  w=$(basename "$d")
  (
    cd "$d" || exit
    jj new main -m "keep-$w" >/dev/null 2>&1
    printf 'keep-%s\n' "$w" > "keep-$w.txt"
    jj commit -m "keep-$w" >/dev/null 2>&1
    jj bookmark create "keep-$w" -r '@-' >/dev/null 2>&1
  )
done

worker_commit() {
  d=$1
  r=$2
  (
    cd "$d" || exit
    w=$(basename "$d")
    jj new main -m "c-$w-$r" >/dev/null 2>&1
    printf 'c-%s-%s\n' "$w" "$r" > "c-$w-$r.txt"
    jj commit -m "c-$w-$r" >/dev/null 2>&1
  )
}

worker_forge_op() {
  d=$1
  r=$2
  (
    cd "$d" || exit
    case $((r % 3)) in
      0) jj rebase --skip-emptied -s 'roots(mutable() & mine() & ::@)' -d 'trunk()' >/dev/null 2>&1 ;;
      1) jj absorb --into 'mutable() & ::@ & ~::(trunk())' >/dev/null 2>&1 ;;
      2) jj restore >/dev/null 2>&1; jj new main >/dev/null 2>&1 ;;
    esac
  )
}

undo_loop() {
  n=0
  while [ "$n" -lt "$UNDOS" ]; do
    case $MODE in
      undo-cross) jj undo --allow-cross-workspace >/dev/null 2>&1 ;;
      undo-plain) jj undo >/dev/null 2>&1 ;;
    esac
    n=$((n + 1))
  done
}

divergent() {
  jj --ignore-working-copy log -r 'all()' --no-graph \
    -T 'change_id.short() ++ "\n"' 2>/dev/null | sort | uniq -d | wc -l | tr -d ' '
}

missing_markers() {
  have=$(jj --ignore-working-copy log -r 'all()' --no-graph -T 'description ++ "\n"' 2>/dev/null)
  miss=0
  for d in $WORKSPACES_DIRS; do
    w=$(basename "$d")
    printf '%s' "$have" | grep -q "keep-$w" || miss=$((miss + 1))
  done
  echo "$miss"
}

total_div=0
total_miss=0
r=1
while [ "$r" -le "$ROUNDS" ]; do
  pids=""
  for d in $WORKSPACES_DIRS; do
    ( worker_commit "$d" "$r" ) &
    pids="$pids $!"
  done
  case $MODE in
    mixed)
      for d in $WORKSPACES_DIRS; do
        ( worker_forge_op "$d" "$r" ) &
        pids="$pids $!"
      done
      if [ $((r % 4)) -eq 0 ]; then
        ( cd "$REPO" && jj abandon 'mutable() & empty() & description(exact:"") ~ @ ~ bookmarks() ~ tags()' >/dev/null 2>&1 ) &
        pids="$pids $!"
        ( cd "$REPO" && jj rebase --skip-emptied -s 'roots(mutable() & mine() & ::working_copies())' -d 'trunk()' >/dev/null 2>&1 ) &
        pids="$pids $!"
      fi
      ;;
    undo-cross | undo-plain)
      ( cd "$REPO" && undo_loop ) &
      pids="$pids $!"
      ;;
  esac
  for p in $pids; do wait "$p"; done

  d=$(divergent)
  m=$(missing_markers)
  total_div=$((total_div + d))
  total_miss=$((total_miss + m))
  echo "round=$r divergent_change_ids=$d missing_keep_markers=$m"
  r=$((r + 1))
done

echo "RESULT mode=$MODE rounds=$ROUNDS workspaces=$WORKSPACES divergent_total=$total_div missing_markers_total=$total_miss"
[ "$total_div" -eq 0 ] && [ "$total_miss" -eq 0 ]
