# Claude `result` events drop their reported usage

Status: ready-for-human
Type: bug
Blocked by: none

## Question

Should a Claude `result` event's reported `usage` populate the Run result?

Split out of ticket 28 (item D9). The code contradicts the glossary, but a
migration note can be read as documenting the omission deliberately, so it needs
an intent decision.

## What the code does today

`ClaudeEvent` (`src/engine/run_log.rs:552-567`) - the `result` event DTO - has
**no `usage` field**, so serde silently drops it. `parse_claude_result`
(`:1492-1513`) sets duration (`:1503-1505`), turns (`:1506-1508`) and cost
(`:1509-1511`) but never calls `with_usage`.

`ClaudeUsage` exists (`:602-620`) but is only consumed for assistant and user
messages (`:1419`, `:1457`), never for the result.

**Minimal scenario.** The line
`{"type":"result","subtype":"success","is_error":false,"result":"done","duration_ms":2500,"num_turns":3,"total_cost_usd":0.125,"usage":{"input_tokens":150,"output_tokens":70,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}`
decodes to `succeeded()` with duration, turns and cost, and `usage() == None` -
the `usage` object is discarded.

## Docs

- Contradicted: `CONTEXT.md:144-147` - "**Run result**: The structured completion
  of a Run: whether it succeeded or failed, plus **usage**, duration, turns, and
  cost **when reported**."
- Claude Code's stream-json `result` object does carry `usage` (and
  `modelUsage`), so "when reported" applies.
- Ambiguity flag: `.scratch/wsg-migration/issues/09-port-logs-sessions-and-worker-actions.md:98`
  says final scanning "preserves Claude duration, turns, cost, and failure
  details **plus all Codex usage counters**" - enumerating Claude's result
  *without* usage, which can be read as intentional.

## Correction to the original claim

The ticket said a test locks the omission in. It does not.
`tests/run_log.rs:398-423` (`final_result_scans_the_entire_log`) and
`tests/run_log.rs:693-712` (`claude_success_result_normalizes_completion_metrics`)
never include a `usage` field in their input, so they cannot lock in "omission
when reported" - they only pin duration/turns/cost parsing.
`tests/agent_runtime.rs:1610` likewise. The omission is simply **untested**.
(Contrast `tests/run_log.rs:425-454` `final_result_preserves_codex_usage`, which
does assert Codex usage.)

## Options

1. **Populate it.** Add `usage` to `ClaudeEvent` and call `.with_usage(...)` in
   `parse_claude_result`. Satisfies CONTEXT; needs new tests (none exist). Note
   per-message usage is currently attached to *activities*, so a result-level
   total is additive, not a replacement.
2. **Keep the omission (status quo).** Amend `CONTEXT.md:144-147` to scope
   "usage" to runtimes that report it in the terminal event, matching the
   migration note. No code change.
3. **Derive it.** Sum the assistant-message `ClaudeUsage` values into the result
   instead of reading the result event. Works even if the result event lacks
   usage, but diverges from Claude's own totals and risks double counting.

## Acceptance Criteria

- [ ] The chosen rule is recorded in `## Comments` (or in CONTEXT).
- [ ] A test pins whichever behaviour is chosen - the current omission is
      untested, so either way this needs a new test.
- [ ] If the code changes, `mise run check` is green.
