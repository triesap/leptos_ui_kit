# leptos_ui_kit - agent instructions

## Scope

This repository is the standalone public `triesap/leptos_ui_kit` Rust
workspace.

`leptos_ui_kit` is a source-first, pure-CSS UI kit for Leptos
`0.9.0-alpha`. Its primary product is the `leptos_ui_kit` CLI, which installs
editable, app-owned Leptos component source into supported apps. Treat it as a
shadcn-style source installer for Leptos, not as a runtime component framework.

The MVP supported app shape is intentionally narrow:

```text
Cargo.toml
index.html
styles/app.css
src/
```

Generated components are installed under `src/components/ui`, exported through
`src/components/ui/mod.rs`, wired through `src/components/mod.rs`, and styled
with pure CSS in `styles/app.css`.

The supported app may be a plain single crate or a single-package workspace
root. Multi-member workspace installs remain out of scope.

Generated components are app-owned source. Consumers should install the CLI
from a pinned Git revision and commit generated files, `components.json`, and
the configured state/baseline directory; they should not add this crate family
to an app as the way to access generated components.

## Repository Map

- `crates/leptos_ui_kit` is the public facade crate.
- `crates/leptos_ui_kit_primitives` is reserved for render-feature-neutral
  shared runtime primitives.
- `crates/leptos_ui_kit_registry` owns strict config parsing, project
  detection, registry metadata, schema URL constants, dependency plans, and
  built-in registry item loading.
- `crates/leptos_ui_kit_codegen` owns dry-run planning, write transactions,
  path safety, CSS/module patching, install state, baselines, and command
  envelope types.
- `crates/leptos_ui_kit_cli` owns the `leptos_ui_kit` binary, the
  `cargo leptos_ui_kit` subcommand entrypoint, and command output.
- `crates/leptos_ui_kit_registry/registry` contains packaged built-in registry
  items and source assets.
- `schema/0.9.0-alpha` contains the public JSON schemas referenced by
  generated files and registry metadata.
- `tests/fixtures/homepage_trunk_csr` is the canonical MVP fixture for a
  supported Trunk CSR app.

## Product Contract

Keep these decisions stable unless the user explicitly approves a product
change:

- Leptos target is `0.9.0-alpha` while Leptos itself is alpha.
- This crate family version stays aligned with the Leptos line:
  `0.9.0-alpha` now, then `0.9.0` after a deliberate Leptos stable audit.
- Styling is pure CSS only.
- Tailwind is not supported.
- shadcn compatibility fields, alias shims, React/Radix runtime compatibility,
  RSC, TSX, and legacy config fields are not supported.
- The MVP does not mutate `Cargo.toml`; it emits dependency plans only.
- The MVP supports built-in registry items only.
- `components.json` is desired state. `add` records desired items and installs
  them; `sync` reconciles the app from desired state.
- The MVP supports Trunk CSR only; SSR, hydration, islands, multi-member
  workspace installs, and remote registries are future work, not silent
  compatibility paths.
- Generated source should be app-owned, readable, editable Rust and CSS.
- Generated component CSS classes use the `.luk-*` prefix and CSS custom
  properties use the `--luk-*` prefix.

## CLI Contract

The canonical binary name is `leptos_ui_kit`.

Supported MVP commands:

```bash
leptos_ui_kit info
leptos_ui_kit init
leptos_ui_kit view button
leptos_ui_kit add button
leptos_ui_kit sync
leptos_ui_kit migrate state-dir src/components/ui/_kit_state
leptos_ui_kit doctor --strict
cargo leptos_ui_kit doctor --strict
```

Write commands support `--dry-run`. Commands with structured output support
`--json`. Common flags include `--cwd`, `--quiet`, and `--verbose`.

Do not add duplicate command names, compatibility aliases, or interactive
prompts without explicit approval. Prefer deterministic JSON envelopes,
diagnostics, and change records.

## Config And Schema

The canonical `components.json` schema URL is:

```text
https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/components.schema.json
```

Keep schema URL constants, packaged registry JSON, tests, and files under
`schema/0.9.0-alpha` aligned. Do not introduce non-GitHub Pages schema domains
unless the domain exists and the user has approved the migration.

The config model is strict. Unknown fields should fail. Legacy shadcn/Tailwind
fields should fail.

`components.json` declares desired registry items, the pinned tool source, and
`state.dir`. The default state directory is `src/components/ui/_kit_state`.
That directory records installer state and baselines. Strict doctor checks
should fail when desired items are not installed, installed items are not
declared, config hashes drift, generated baselines drift, or installer metadata
is ignored by Git.

## Generated Source Rules

Generated source must compile against Leptos `0.9.0-alpha` and Rust 1.92.

Generated components should:

- use `leptos::prelude::*`
- render ordinary HTML elements where possible
- keep props explicit and small
- make form-capable components explicit about native HTML behavior such as
  button type
- accept Leptos-native reactive props for state that naturally changes at
  runtime
- avoid hidden runtime dependencies
- avoid Tailwind classes and framework-specific CSS tooling
- preserve accessibility semantics for the rendered element
- keep CSS in managed blocks delimited by `leptos-ui-kit:start` and
  `leptos-ui-kit:end`

If a generated file or managed CSS block is tracked in the configured
`state.dir` state file,
local edits must be detected through baselines instead of overwritten silently.

## File And Docs Policy

Do not create a `docs/**` tree in this repository.

Use these root files for public project documentation:

- `README.md` for user-facing overview and command basics
- `CONTRIBUTING.md` for contributor rules
- `CHANGELOG.md` for release notes
- `AGENTS.md` for agent/development instructions

If this repository is mounted inside a parent monorepo that keeps durable RCL
or handoff docs, put those docs in the parent repo, not here.

Do not add repo-local `.beads` state. If a parent repo uses Beads, that parent
owns coordination state.

## Validation

Use the narrowest validation that proves the change. For most code changes,
run:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
```

When validating package contents or release readiness, also run:

```bash
cargo package -p leptos_ui_kit_registry --list
cargo package --workspace --no-verify
cargo publish -p leptos_ui_kit_registry --dry-run
```

If this repo is being edited from an extbuild-enabled parent workspace, run
build, test, package, install, and generated-artifact commands through:

```bash
cargo extbuild run -- <command>
```

Run `cargo extbuild doctor` before the first extbuild-routed mutating build,
test, package, install, or generated-artifact command in that environment.

For schema edits, validate JSON syntax with `jq empty` or an equivalent JSON
parser and then run the Rust test lane.

For CLI integration edits, keep or extend
`tests/fixtures/homepage_trunk_csr` and
`crates/leptos_ui_kit_cli/tests/workflow.rs`.

## Release And Versioning

This repository is not ready for a full crates.io publish sequence until
internal crates are published in dependency order. A CLI publish dry-run may
fail if internal crate dependencies are not in the crates.io index yet; treat
that as a publish-order issue, not automatically as a code failure.

Before migrating from `0.9.0-alpha` to `0.9.0`, perform a Leptos API audit,
generated-source audit, fixture validation, schema review, package
verification, and changelog update.

## Definition Of Done

- The change preserves the strict MVP contract unless the user approved a
  product change.
- Affected registry JSON, schema files, generated assets, tests, and root
  public docs are updated together.
- Generated or packaged files still use valid schema URLs.
- Relevant validation passed, or a concrete blocker is reported.
- The final handoff states what changed, what validation ran, and any remaining
  release or product risk.
