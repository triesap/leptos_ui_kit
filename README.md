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

## Goals

- Provide a consistent, Leptos-native component source foundation.
- Install components as project-owned source files.
- Use a CLI to inspect, initialize, view, add, and verify components.
- Keep styling simple with `.luk-*` CSS classes and `--luk-*` CSS variables.
- Keep generated source compatible with Leptos `0.9.0-alpha`.
- Store minimal `.leptos-ui` state and baselines for idempotency and future
  conflict-aware updates.

## MVP Commands

```bash
leptos_ui_kit info
leptos_ui_kit init
leptos_ui_kit view button
leptos_ui_kit add button
leptos_ui_kit doctor
```

Write commands support `--dry-run`. Commands that emit machine-readable output
support `--json`.

The CLI emits dependency plans for:

```toml
leptos = "0.9.0-alpha"
leptos_router = "0.9.0-alpha"
```

It does not mutate `Cargo.toml` in the MVP.

## Built-In Button

The built-in `button` item installs `Button`, `ButtonVariant`, `ButtonSize`,
and `ButtonType`. `ButtonType` defaults to `Button`; use `ButtonType::Submit`
for form submit buttons. The `disabled` prop accepts static booleans or
reactive closures, and the generated CSS exposes `--luk-*` tokens for app-owned
theming without editing the managed CSS block.

## Non-Goals

The MVP does not support Tailwind, shadcn compatibility, alias shims, React,
Radix runtime compatibility, SSR, hydration, islands, multi-member workspace
installs, remote registries, Cargo manifest mutation, telemetry, or complex
interactive primitives.

## Version Policy

While Leptos is on `0.9.0-alpha`, this crate family uses `0.9.0-alpha`.
After Leptos publishes full `0.9.0`, the crate family migrates deliberately to
`0.9.0` after an API audit, generated-source audit, fixture validation, and
package verification.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
