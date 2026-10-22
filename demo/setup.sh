#!/usr/bin/env bash
# Build the throwaway jj repository that demo/jjfx.tape records.
#
# Run from anywhere:
#   bash demo/setup.sh
#
# Requires jj 0.46.x (jjfx reads jj's store through jj-lib 0.46.0) and a
# release build at target/release/jjfx (`cargo build --release -p jjfx`).
# The fixture is gitignored; re-running this script replaces it.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
demo="$root/demo"
fixture="$demo/fixture"
home="$demo/home"
jj_bin=${JJ:-jj}

if ! command -v "$jj_bin" >/dev/null 2>&1; then
  echo "error: jj is not on PATH. jjfx is coupled to jj 0.46.0 (mise.toml)." >&2
  exit 1
fi
case "$("$jj_bin" --version)" in
  "jj 0.46."*) ;;
  *)
    echo "error: need jj 0.46.x, found $("$jj_bin" --version)" >&2
    exit 1
    ;;
esac
# Pin the jj that just answered, so the TUI's later `jj` calls use the same
# binary. A shim earlier on PATH must not be able to answer differently.
jj_dir=$(dirname "$(type -P "$jj_bin")")
jj_dir=$(cd "$jj_dir" && pwd)
if [[ ! -x "$root/target/release/jjfx" ]]; then
  echo "error: $root/target/release/jjfx is missing." >&2
  echo "       cargo build --release -p jjfx" >&2
  exit 1
fi

rm -rf "$fixture" "$demo/login" "$demo/docs" "$home"
mkdir -p "$home/config" "$home/state"

cfg="$home/jj.toml"
cat >"$cfg" <<'EOF'
[user]
name = "jjfx demo"
email = "demo@jjfx.local"

[signing]
behavior = "drop"
EOF

export JJ_CONFIG="$cfg"
# Keep the recording off the machine's jjfx config and UI state.
export XDG_CONFIG_HOME="$home/config"
export XDG_STATE_HOME="$home/state"

"$jj_bin" git init "$fixture"
cd "$fixture"

mkdir -p src
cat >src/main.rs <<'EOF'
fn main() {
    println!("hello from jjfx");
}
EOF
"$jj_bin" describe -m "Sketch the command"

"$jj_bin" new
cat >src/lib.rs <<'EOF'
pub fn run() -> i32 {
    0
}
EOF
"$jj_bin" describe -m "Split the library out of main"
"$jj_bin" bookmark create main -r @

# Each feature workspace is a child of main, so the default workspace stays
# on trunk (idle) and the two edits read as ready to forge.
"$jj_bin" workspace add --name login -r main "$demo/login"
(
  cd "$demo/login"
  cat >src/login.rs <<'EOF'
pub fn login(user: &str) -> bool {
    !user.is_empty()
}
EOF
  "$jj_bin" describe -m "Fix the login bug"
)

"$jj_bin" workspace add --name docs -r main "$demo/docs"
(
  cd "$demo/docs"
  cat >NOTES.md <<'EOF'
# Notes

The pool dispatches one ticket per workspace.
EOF
  "$jj_bin" describe -m "Note how dispatch works"
)

cat >"$demo/.env" <<EOF
export JJ_CONFIG="$cfg"
export XDG_CONFIG_HOME="$home/config"
export XDG_STATE_HOME="$home/state"
export PATH="$jj_dir:$root/target/release:\$PATH"
export DEMO_ROOT="$fixture"
export PS1='$ '
EOF

echo "demo ready: $fixture"
