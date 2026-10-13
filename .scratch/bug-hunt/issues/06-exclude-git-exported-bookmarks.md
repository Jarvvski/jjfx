# Exclude git-exported bookmarks from the "pushed" work state

Status: wontfix
Type: task
Blocked by: none

## Outcome

**Refuted - not a bug.** `remote_bookmarks()` already excludes the colocated
`git` remote, so `work.rs:420-433` does exactly what its comment claims.

Evidence (`jj 0.46.0`, colocated repo, `origin` remote configured, an unpushed
local bookmark present):

```
$ jj log -r 'remote_bookmarks()' --no-graph -T 'change_id.short() ++ "\n"'
          # selects nothing - the unpushed bookmark is only on @git
$ jj log -r 'remote_bookmarks(remote=git)' --no-graph -T 'change_id.short() ++ "\n"'
pvonnzkumunl
pozmnnnrkxmn
```

An unpushed local bookmark therefore reads `Dirty`, and only a bookmark on a
real remote reads `Pushed`. The original sweep misread the
`remote_bookmarks` **template keyword** (which also omits `@git` when listing a
selected commit) as the revset's selection.

## Follow-up

None. If jj ever changes this default, `work.rs` would need an explicit
`remote_bookmarks(remote=...)` filter - but that is a future jj upgrade concern,
covered by the version-coupling ticket (24).
