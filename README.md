# leptos_ui_kit

`leptos_ui_kit` is a source-first UI kit for Leptos `0.9.0-alpha`.
It provides a `leptos_ui_kit` CLI and a packaged registry for installing
editable, app-owned component source into Trunk CSR apps.

The MVP target is intentionally narrow:

```text
Cargo.toml
index.html
styles/app.css
src/
```

The project may be a plain single crate or a single-package workspace root.
Multi-member workspace installs are not supported.

Generated components are installed under `src/components/ui` and styled with
pure CSS in `styles/app.css`.
Installed components are declared in `components.json`, while the configured
state directory stores installer state and generated baselines. The default
state directory is `src/components/ui/_kit_state`. Commit both with the
generated source so `sync` and `doctor` can reconcile the app deterministically.

## Goals

- Provide a consistent, Leptos-native component source foundation.
- Install components as project-owned source files.
- Use a CLI to inspect, initialize, view, add, sync, and verify components.
- Keep styling simple with `.luk-*` CSS classes and `--luk-*` CSS variables.
- Keep generated source compatible with Leptos `0.9.0-alpha`.
- Store minimal configured state and baselines for idempotency and future
  conflict-aware updates.

## Install

Install the CLI from a pinned Git revision:

```bash
cargo install \
  --git https://github.com/triesap/leptos_ui_kit \
  --rev <rev> \
  --locked \
  leptos_ui_kit_cli
```

Apps do not add `leptos_ui_kit` as a runtime dependency or dev-dependency to
use generated components. The installed Rust and CSS are app-owned source.

## MVP Commands

```bash
leptos_ui_kit info
leptos_ui_kit init
leptos_ui_kit view button
leptos_ui_kit add button
leptos_ui_kit add collapsible
leptos_ui_kit add tabs
leptos_ui_kit add dialog
leptos_ui_kit sync
leptos_ui_kit migrate state-dir src/components/ui/_kit_state
leptos_ui_kit doctor --strict
cargo leptos_ui_kit doctor --strict
```

Write commands support `--dry-run`. Commands that emit machine-readable output
support `--json`.

`add button` records `button` in `components.json` and installs the generated
source. `sync` reconciles the app from `components.json`, and `doctor --strict`
verifies generated files, managed CSS blocks, desired state, and configured
installer metadata.

Use `leptos_ui_kit init --state-dir <path>` to create a new app with a
non-default state directory. Use `leptos_ui_kit migrate state-dir <path>` to
move an existing app's state and baselines explicitly.

The CLI emits dependency plans for:

```toml
leptos = "0.9.0-alpha"
leptos_router = "0.9.0-alpha"
web_ui_primitives = { git = "https://github.com/triesap/web_ui_primitives", rev = "<rev>", features = ["leptos"] }
```

`web_ui_primitives` is required only by primitive-backed items such as
`collapsible`, `tabs`, and `dialog`. The CLI does not mutate `Cargo.toml` in the
MVP; it reports dependency plans and `doctor --strict` verifies the consumer app
manifest.

## Built-In Components

The built-in `button` item installs `Button`, `ButtonVariant`, `ButtonSize`,
and `ButtonType`. `ButtonType` defaults to `Button`; use `ButtonType::Submit`
for form submit buttons. The `disabled` prop accepts static booleans or
reactive closures, and the generated CSS exposes `--luk-*` tokens for app-owned
theming without editing the managed CSS block.

The built-in `collapsible`, `tabs`, and `dialog` items install editable
component families backed by `web_ui_primitives` behavior contracts. These items
keep semantic DOM in generated app-owned source while delegating accessibility
state, keyboard behavior, focus management, dismissible overlay behavior,
presence, modal hiding, scroll lock, and ARIA attributes to the primitive
substrate.

Generated component CSS is emitted into managed `/* leptos-ui-kit:start ... */`
blocks in `styles/app.css`. App-specific styling should use `.luk-*` classes and
`--luk-*` CSS variables outside those managed blocks.

## Non-Goals

The MVP does not support Tailwind, shadcn compatibility, alias shims, React,
Radix runtime compatibility, SSR, hydration, islands, multi-member workspace
installs, remote registries, Cargo manifest mutation, telemetry, or generated
runtime component-library imports.

## Version Policy

While Leptos is on `0.9.0-alpha`, this crate family uses `0.9.0-alpha`.
After Leptos publishes full `0.9.0`, the crate family migrates deliberately to
`0.9.0` after an API audit, generated-source audit, fixture validation, and
package verification.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
