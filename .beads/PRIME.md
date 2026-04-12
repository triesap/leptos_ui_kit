# leptos_ui_kit beads prime

## workflow
- use beads as the live execution state for active work in this repo
- keep each active slice small enough to prove green with one dominant verify lane
- do not create markdown todo trackers when beads is active
- do not use `bd edit`

## start of session
- `bd ready --json`
- `bd show <id>`
- `bd update <id> --claim --json`

## planning rules
- treat `tmp/ui` and `tmp/leptos_shadcn_workspace` from the outer workspace as references only, not implementation authority
- keep durable planning and architecture docs under the outer workspace `docs/leptos_ui_kit`, not under this package repo's `docs/`
- preserve shadcn public contracts where they are part of the product surface, but re-implement runtime behavior natively for Leptos
- prefer single-crate install support first, while keeping routing and config models extensible for shared-ui and workspace installs later
- if new work appears during a slice, create it immediately and link it with `discovered-from:<parent-id>`

## verify lanes
- workspace/bootstrap and toolchain slices: `cargo metadata --format-version 1 --no-deps`
- runtime and cli slices: `cargo check`
- behavior and contract slices: `cargo test`

## end of session
- `bd close <id> --reason "..."`
