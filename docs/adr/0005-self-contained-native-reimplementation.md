# jjfx reimplements its capabilities natively and coexists with the bash tools

jjfx carries its own native (Rust) implementations of workspace lifecycle
(add/remove/list), the forge pipeline, and the maintenance operations `tidy` and
`tidyws`. It does not call `jj-ws` or `jj-forge`, and it does not delete or
replace them - every existing alias and script stays alive and usable
standalone. jjfx shells out only to the true primitives it does not own: `jj`,
`gh`, and `jj-spr`.

"Stronger and more resilient" is the point of reimplementing rather than
shelling to the bash helpers: atomic writes, structured errors, and forge/tidy
modeled as real state (feeding the work-lifecycle axis) instead of scraping
command stdout. The proven, workspace-safe revsets from `jj-forge` (weld
scoped to `::@`, push excluding `trunk()`/`conflicts()`, the `jj-spr` handoff)
are ported deliberately, not reinvented.

Status: accepted
