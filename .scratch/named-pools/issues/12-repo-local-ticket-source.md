# A repository-local `[tickets]` source selection

Type: task
Status: resolved
Blocked by: 01

## Problem

The `todo-md` test-drive wanted `[tickets] source = "local"` per Repository and could not have it: `RepoConfig` accepted only `[checks]`, so the agent had to fake `XDG_CONFIG_HOME` to make its ticket source local. That wall is the reason the multi-pool effort's sharpest fork existed at all.

**ADR 0014** removes it. The capability rule permits a repository-local file to **select among capabilities the user config already granted**, and a Ticket source is exactly that - a reference to something the user config defines, granting nothing. So a Repository may choose *which* configured source it uses, while the source's own definition (and anything credential-bearing) stays user-global.

This is a follow-on, not a prerequisite: nothing in the named-pools work needs it. It is filed because the capability rule makes it possible and because it is a real wall a real test-drive hit.

## Scope

- `RepoConfig` grows a `[tickets]` section that may **select** a source (`source = "local"` / `"linear"`), and may not define one or carry credentials.
- The selected source's details stay user-global; a repository-local attempt to define them is a loud error, per the capability rule.
- The `deny_unknown_fields` error text and `docs/configuration.md` move with it.

## Acceptance Criteria

- [ ] A Repository can select `local` or `linear` for itself without touching the user config.
- [ ] A repository-local attempt to define a source's details or credentials is a loud error naming the file.
- [ ] An unknown key inside `[tickets]` is still a loud error.
- [ ] `docs/configuration.md` documents the selection and the capability rule behind it.
- [ ] `mise run check` is green.

## References

- `src/config.rs` (`RepoConfig`, `TicketsConfig`, `TicketSource`)
- `docs/adr/0014-repository-local-pool-declarations.md`
- `.scratch/dispatch-ergonomics/map.md` ("Deliberately not filed" - `[tickets]` cannot be repository-local)
