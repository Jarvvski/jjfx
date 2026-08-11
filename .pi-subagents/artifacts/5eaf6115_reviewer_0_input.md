# Task for reviewer

[Read from: /Users/jarvis/Code/personal/jjfx/AGENTS.md, /Users/jarvis/Code/personal/jjfx/CLAUDE.md, /Users/jarvis/Code/personal/jjfx/CONTEXT.md, /Users/jarvis/Code/personal/jjfx/src/lib.rs, /Users/jarvis/Code/personal/jjfx/src/main.rs, /Users/jarvis/Code/personal/jjfx/src/tui.rs, /Users/jarvis/Code/personal/jjfx/src/app.rs, /Users/jarvis/Code/personal/jjfx/crates/wsg/src/cli.rs, /Users/jarvis/Code/personal/jjfx/crates/wsg/tests/tui.rs, /Users/jarvis/Code/personal/jjfx/Cargo.toml, /Users/jarvis/Code/personal/jjfx/crates/wsg/Cargo.toml]

Standards review, read-only. Review the jjfx changes from revision loyolw through @ using jj diff --from loyolw --to @ and jj log -r 'loyolw..@'. Read AGENTS.md, CLAUDE.md, CONTEXT.md, and the TDD/codebase-design rules. Report under 400 words: documented-standard violations per file/hunk, plus judgment-call Fowler smells (mysterious name, duplication, feature envy, data clumps, primitive obsession, repeated switches, shotgun surgery, divergent change, speculative generality, message chains, middle man, refused bequest). Never use Git and do not modify files.

## Acceptance Contract
Acceptance level: attested
Completion is not accepted from prose alone. End with a structured acceptance report.

Criteria:
- criterion-1: Return concrete findings with file paths and severity when applicable

Required evidence: review-findings, residual-risks

Finish with a fenced JSON block tagged `acceptance-report` in this shape:
Use empty arrays when no items apply; array fields contain strings unless object entries are shown.
`criteriaSatisfied[].status` must be exactly one of: satisfied, not-satisfied, not-applicable.
`commandsRun[].result` must be exactly one of: passed, failed, not-run.
`manualNotes` and `notes` are optional strings; an empty string means no note and does not satisfy `manual-notes` evidence.
```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "specific proof"
    }
  ],
  "changedFiles": [
    "src/file.ts"
  ],
  "testsAddedOrUpdated": [
    "test/file.test.ts"
  ],
  "commandsRun": [
    {
      "command": "command",
      "result": "passed",
      "summary": "short result"
    }
  ],
  "validationOutput": [
    "validation output or concise summary"
  ],
  "residualRisks": [
    "none"
  ],
  "noStagedFiles": true,
  "diffSummary": "short description of the diff",
  "reviewFindings": [
    "blocker: file.ts:12 - issue found, or no blockers"
  ],
  "manualNotes": "anything else the parent should know"
}
```