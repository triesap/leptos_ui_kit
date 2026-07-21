# leptos_ui_kit

`leptos_ui_kit` installs editable, pure-CSS UI components for Leptos
`0.9.0-alpha`. Generated Rust and CSS belong to the application; the kit is not
a runtime component dependency.

## Usage

```text
cargo install leptos_ui_kit_cli --locked
leptos_ui_kit init
leptos_ui_kit add <item>
leptos_ui_kit sync
leptos_ui_kit doctor --strict
```

Run these commands from the application directory. Components are written to
`src/components/ui`, managed CSS to `styles/kit.css`, and kit state to
`src/components/ui/_kit` by default. Commit all generated files. Write commands
support `--dry-run`; `info`, `view`, and JSON output are also available.

## Projects

The CLI supports Trunk CSR, native SSR, browser hydration, and render-neutral
shared libraries. Final applications select one matching Leptos feature:
`csr`, `ssr`, or `hydrate`. Shared libraries select none.

The CLI reports the exact Cargo dependency plan but does not edit `Cargo.toml`.
The registry includes `button`, `collapsible`, `tabs`, `dialog`, `menu`,
`field`, `status`, `spinner`, `anchor`, `router-link`, and the CSS-only `tokens`
foundation.

## Themes

The `tokens` item defines the semantic `--kit-*` variables used by components.
Load application theme CSS after `styles/kit.css`. Theme selectors own their
values and `color-scheme`; theme selection and persistence remain application
concerns.

Nested theme scopes work directly. When a dialog must inherit a nested scope,
pass a `portal_mount` inside it. After upgrading the kit, run `sync`; untouched
managed blocks migrate automatically and edited blocks remain unchanged.

See [CONTRIBUTING.md](CONTRIBUTING.md).

Licensed under either the MIT License or the Apache License, Version 2.0, at your option.
