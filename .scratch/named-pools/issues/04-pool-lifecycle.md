# Pool lifecycle: create, destroy, and size `default`

Type: task
Status: ready-for-agent
Blocked by: 01, 02

## Problem

`jjfx pool <N>` resizes the one pool, `pool destroy` tears it down, and `pool list` lists it. With named pools these become questions of *which* pool, and of where a pool's existence actually lives.

The design settled both (*Where are pools declared?* and *What happens when a Worker Pool is removed?*):

- **The declaration is the source of truth and the verbs edit it.** `pool create` and `pool destroy` write the `[pools]` section rather than a parallel state, so there is no "which wins on conflict" to answer.
- **`jjfx pool <N>` keeps sizing `default`**, which is implicit whenever `[pools]` is absent - the additive stance.
- **`default` is not removable.** A Repository always has at least one pool, and `jjfx pool <N>` targets it forever. A Repository that wants only named lanes resizes `default` to zero instead.

## Scope

- `jjfx pool create <name> ...` writes the declaration; `jjfx pool destroy <name>` removes it.
- `jjfx pool <N>` sizes `default`; sizing a named pool is explicit (`--pool <name>`, or the pool is named).
- `pool list` (and its `--json`) lists every pool, naming `default` when it is implicit.
- Destroying `default` is refused with the reason above.
- The verbs remain usable on a Repository that never declares a pool, because `default` is implicit.

## Acceptance Criteria

- [ ] `pool create` / `pool destroy` edit the `[pools]` declaration, and a Repository with no declaration is unchanged.
- [ ] `jjfx pool <N>` still sizes `default`.
- [ ] `pool list` names every pool, including an implicit `default`.
- [ ] Destroying `default` is refused, naming the reason.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`PoolCommand`, `resize_pool_command`, `pool_list_command`, `destroy_pool_command`)
- `.scratch/multi-pool/issues/05-where-pools-are-declared.md` and `10-what-removing-a-pool-does.md`
