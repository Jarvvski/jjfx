# Task for reviewer

[Read from: /Users/jarvis/Code/personal/jjfx/.scratch/wsg-migration/issues/23-share-and-harden-jjfx-tui.md, /Users/jarvis/Code/personal/jjfx/.scratch/wsg-migration/issues/16-integrate-dispatch-into-jjfx.md, /Users/jarvis/Code/personal/jjfx/.scratch/wsg-migration/PRD.md, /Users/jarvis/Code/personal/jjfx/src/lib.rs, /Users/jarvis/Code/personal/jjfx/src/main.rs, /Users/jarvis/Code/personal/jjfx/src/tui.rs, /Users/jarvis/Code/personal/jjfx/src/app.rs, /Users/jarvis/Code/personal/jjfx/crates/wsg/src/cli.rs, /Users/jarvis/Code/personal/jjfx/crates/wsg/tests/tui.rs]

Spec review, read-only. Review the jjfx changes from revision loyolw through @ using jj diff --from loyolw --to @ and jj log -r 'loyolw..@'. Use .scratch/wsg-migration/issues/23-share-and-harden-jjfx-tui.md and its parent issue 16 plus PRD as the spec. Report under 400 words: missing/partial requirements, scope creep, and implementations that look wrong. Quote the relevant spec requirement for each finding. Never use Git and do not modify files.

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