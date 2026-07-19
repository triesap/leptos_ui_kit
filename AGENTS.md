# leptos_ui_kit - agent instructions

## Scope

This is the public `triesap/leptos_ui_kit` Rust workspace.

`leptos_ui_kit` is a source-first, pure-CSS UI kit for Leptos `0.9.0-alpha`.
Its primary product is the `leptos_ui_kit` CLI, which installs editable,
app-owned Leptos component source into supported Trunk CSR apps.

Treat generated components as app-owned source, not as runtime imports from this
crate family.

## Repository Map

- `crates/leptos_ui_kit` is the public facade crate.
- `crates/leptos_ui_kit_primitives` owns render-feature-neutral shared
  primitives.
- `crates/leptos_ui_kit_registry` owns config parsing, project detection,
  registry metadata, schema URL constants, dependency plans, and built-in
  registry item loading.
- `crates/leptos_ui_kit_codegen` owns dry-run planning, write transactions,
  path safety, CSS/module patching, lock metadata, and command envelope types.
- `crates/leptos_ui_kit_codegen_platform` is the narrow audited Windows FFI
  boundary for capability-relative transaction filesystem operations. It is
  the only workspace crate permitted to contain unsafe code; consumers use its
  safe handle-based API.
- `crates/leptos_ui_kit_cli` owns the `leptos_ui_kit` binary, the
  `cargo leptos_ui_kit` entrypoint, and command output.
- `crates/leptos_ui_kit_registry/registry` contains built-in registry items and
  source assets.
- `schema/0.9.0-alpha` contains the public JSON schemas.
- `tests/fixtures/homepage_trunk_csr` is the canonical Trunk CSR fixture.
- `crates/leptos_ui_kit_cli/tests/fixtures/homepage_trunk_csr` is its
  package-local mirror for extracted-package and installed-runtime acceptance.

## Product Contract

- Package versioning uses normal SemVer.
- The current Leptos compatibility target is `0.9.0-alpha`.
- Styling is pure CSS only.
- Tailwind is not supported.
- shadcn compatibility fields, alias shims, React/Radix runtime compatibility,
  TSX, and legacy config fields are not supported.
- The CLI does not mutate `Cargo.toml`; it emits and verifies dependency plans.
- Built-in registry items are the only supported registry source.
- The supported app may be a single crate or a single-package workspace root.
- SSR, hydration, islands, multi-member workspace installs, and remote
  registries are future work.
- Generated CSS classes use `.kit-*`; CSS custom properties use `--kit-*`.

## CLI Contract

Supported commands:

```bash
leptos_ui_kit info
leptos_ui_kit init
leptos_ui_kit view button
leptos_ui_kit add button
leptos_ui_kit sync
leptos_ui_kit doctor --strict
cargo leptos_ui_kit doctor --strict
```

Write commands support `--dry-run`. Structured output commands support `--json`.
Prefer deterministic JSON envelopes, diagnostics, and change records.

## Config And Schema

The canonical `kit.json` schema URL is:

```text
https://triesap.github.io/leptos_ui_kit/schema/0.9.0-alpha/kit.schema.json
```

Keep schema URL constants, packaged registry JSON, tests, and files under
`schema/0.9.0-alpha` aligned. The config model is strict: unknown fields and
legacy compatibility fields must fail.

## Generated Source Rules

Generated source must compile against Leptos `0.9.0-alpha` and Rust 1.92.

Generated components should:

- use `leptos::prelude::*`
- render ordinary HTML elements where possible
- keep props explicit and small
- make native HTML behavior explicit for form-capable components
- accept Leptos-native reactive props where state naturally changes
- avoid hidden runtime dependencies
- avoid Tailwind classes and framework-specific CSS tooling
- preserve accessibility semantics
- keep CSS in managed blocks delimited by `leptos-ui-kit:start` and
  `leptos-ui-kit:end`

## File And Docs Policy

Do not create a `docs/**` tree in this repository.

Use these root files for public project documentation:

- `README.md`
- `CONTRIBUTING.md`
- `CHANGELOG.md`
- `SECURITY.md`
- `AGENTS.md`

Do not add repo-local coordination database state.

## Validation

Use the narrowest validation that proves the change. For most code changes,
run:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
```

For schema edits, validate JSON syntax and run the Rust test lane.
For CLI integration edits, keep or extend `tests/fixtures/homepage_trunk_csr`,
its package-local mirror under
`crates/leptos_ui_kit_cli/tests/fixtures/homepage_trunk_csr`, and
`crates/leptos_ui_kit_cli/tests/workflow.rs`. Keep both fixture copies aligned;
the workflow parity test must continue to guard their exact contents.

For package manifests, include lists, embedded assets, provenance, or CLI
installation changes, also run:

```bash
cargo package --workspace --allow-dirty --no-verify --locked

cargo test -p leptos_ui_kit_registry --test package_source \
  packaged_sources_build_with_cargo_vcs_provenance_outside_and_inside_hostile_git -- \
  --ignored --exact --nocapture

cargo test -p leptos_ui_kit_cli --test packaged_runtime \
  installed_binaries_run_after_package_source_and_build_state_are_deleted -- \
  --ignored --exact --nocapture
```

The two ignored tests are mandatory slow lanes for packaging changes; ordinary
workspace tests compile them but do not execute them. Run the slow lanes from a
clean Git worktree because dirty Cargo VCS metadata is rejected intentionally.

## Definition Of Done

- The strict product contract is preserved unless a change is explicitly
  approved.
- Affected registry JSON, schema files, generated assets, tests, and public
  docs are updated together.
- Generated or packaged files still use valid schema URLs.
- Relevant validation passed, or a concrete blocker is reported.
