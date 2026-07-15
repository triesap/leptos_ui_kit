# leptos_ui_kit

Source-first UI components for Leptos `0.9.0-alpha`.

`leptos_ui_kit` provides a CLI and packaged registry for installing editable,
app-owned Rust and CSS into Trunk CSR apps.

## Install

```bash
cargo install leptos_ui_kit_cli --locked
```

Apps do not depend on `leptos_ui_kit` at runtime. The CLI installs source files
under `src/components/ui` and managed CSS blocks in `styles/kit.css`.

## Commands

```bash
leptos_ui_kit info
leptos_ui_kit init
leptos_ui_kit view button
leptos_ui_kit add button
leptos_ui_kit add collapsible
leptos_ui_kit add tabs
leptos_ui_kit add dialog
leptos_ui_kit sync
leptos_ui_kit doctor --strict
cargo leptos_ui_kit doctor --strict
```

Write commands support `--dry-run`. Structured output commands support `--json`.

## Supported App Shape

```text
Cargo.toml
index.html
styles/kit.css
src/
```

The app may be a single crate or a single-package workspace root.
Multi-member workspace installs are not supported.

## Dependency Plan

Primitive-backed components require:

```toml
[dependencies]
leptos = "0.9.0-alpha"
leptos_router = "0.9.0-alpha"
web_ui_primitives = { version = "0.1.0", features = ["leptos"] }
```

The CLI reports required dependencies and verifies them with `doctor --strict`.
It does not mutate `Cargo.toml`.

## Components

The built-in registry includes `button`, `collapsible`, `tabs`, `dialog`,
`menu`, `field`, `status`, `spinner`, `anchor`, and `router-link`.

Generated source is app-owned. Managed CSS is delimited with
`leptos-ui-kit:start` and `leptos-ui-kit:end` markers.

## Version

Package version `0.1.0` targets Leptos `0.9.0-alpha`.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
