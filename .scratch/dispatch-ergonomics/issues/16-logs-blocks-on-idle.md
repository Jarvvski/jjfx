# `jjfx logs` still blocks on a stale idle Worker

Type: task
Status: ready-for-agent
Blocked by: none

## Question

*`jjfx logs` follows a finished Worker forever* stopped follow mode when a Run reached a terminal state, and when a Worker left the Pool mid-follow. It did **not** cover the `Idle` state, and said so rather than widening scope silently: a Worker reset to `idle` keeps its `log_file` — nothing clears it — so `jjfx logs <idle-worker>` still blocks forever on a log that will never grow again.

That is the same hang, one state over. And `idle` is the state a Worker sits in *between* Runs, so it is plausibly the case a caller hits most often.

The reason it was left alone is a real trade-off rather than an oversight: `idle` is not necessarily final. A Worker can be re-dispatched and the same `log_file` is reused, so stopping at `idle` would make `logs` return just before an imminent busy transition. Decide which is right:

- **Follow the current Run, not the Worker** — an idle Worker has no Run, so `logs` returns; a caller that wants the next Run re-attaches, and `jjfx wait` is the purpose-built way to wait for one. This makes the rule a single sentence and removes the hang.
- **Keep blocking through `idle`**, and solve the hang another way — a timeout, or an explicit flag. More faithful to "follow this Worker's output", at the cost of a mode that can block indefinitely with nothing to say.

The first is recommended. `logs` should answer "what did this Run print", and the ticket that filed this one already established that a finished thing must not hold a caller forever.

## Answer

<!-- recorded on resolution -->
