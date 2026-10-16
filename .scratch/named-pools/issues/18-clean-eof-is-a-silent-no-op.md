# `clean`'s confirmation on EOF removes nothing and exits 0 without saying so

Type: task
Status: resolved
Blocked by: none

## Problem

Found by driving a throwaway Repository end to end. `clean` asks for confirmation, and when stdin is not a terminal it gets EOF - and then **removes nothing, exits 0, and prints no reason**:

    $ jjfx clean --pool bugs </dev/null
    Remove 1 workspace(s)?
      worker-685f6e
    Confirm? (y/n)          <- EOF here
    $ echo $?
    0

The prompt also goes to **stdout**, so `jjfx clean > out.txt` captures the prompt and the Workspace list in the command's output.

This is **pre-existing** - the confirmation predates the pool work - but the standard it should meet already exists in this codebase: the pool-growth prompt was fixed to treat "no answer" as a refusal that **says why**, rather than a silent decline. A non-interactive `clean` is the same situation and should behave the same way.

The distinction that matters: a user typing `n` chose not to act. A process that never answered did not choose anything, and a silent exit 0 makes "nothing happened" indistinguishable from "nothing needed doing".

## Solution

- **EOF is a refusal with a reason**: refuse (non-zero) and say no confirmation was received, the way the growth prompt does.
- An explicit `n` may keep exiting 0 if that is the existing contract - say which you chose in your report.
- Move the confirmation prompt to **stderr**, so stdout carries only the command's output.

## Acceptance Criteria

- [x] `clean` with stdin at EOF removes nothing and fails loudly with a reason.
- [x] The confirmation prompt is written to stderr, not stdout.
- [x] An interactive `y` still removes, and an interactive `n` still does not.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (`clean_command`), `src/engine/workspace.rs` (`plan_clean`)
- The pool-growth prompt's EOF handling, for the pattern to follow
