# leptos_ui_kit

Source-first, pure-CSS UI components for Leptos `0.9.0-alpha`.

The CLI installs editable Rust and CSS into your project. Generated files are
app-owned; the kit is not a runtime component dependency.

## Quick start

```bash
cargo install leptos_ui_kit_cli --locked
leptos_ui_kit init
leptos_ui_kit add button
leptos_ui_kit doctor --strict
```

Components are written to `src/components/ui` and managed CSS to
`styles/kit.css` by default. Configuration and lock state live in
`src/components/ui/_kit`. Commit these files with the generated source and CSS.

Use `info` to inspect the project, `view <item>` to preview source, `sync` to
reconcile installed items, and `doctor --strict` to detect drift. Write commands
accept `--dry-run`; structured output accepts `--json`. The CLI also supports
the `cargo leptos_ui_kit` entrypoint.

## Supported projects

- single-crate Trunk CSR
- single-crate native SSR
- single-crate browser hydration
- render-neutral shared libraries invoked from their package root

Final applications select one Leptos feature: `csr`, `ssr`, or `hydrate`.
Shared libraries leave that choice to the application. Rust 1.92 is supported.
Multi-member workspace-root installs, islands, Tailwind, and remote registries
are not supported.

The CLI reports required dependencies but does not edit `Cargo.toml`. All
generated Rust needs Leptos `0.9.0-alpha`; `router-link` also needs
`leptos_router`. `collapsible`, `dialog`, `menu`, and `tabs` need:

```toml
web_ui_primitives = { git = "https://github.com/triesap/web_ui_primitives", rev = "a7ad19e203c08be19040154fa6bce909701d402f", features = ["core", "leptos"] }
```

Run `info --json` for the exact dependency plan, apply it, then run
`doctor --strict`.

## Components

The registry includes `button`, `collapsible`, `tabs`, `dialog`, `menu`,
`field`, `status`, `spinner`, `anchor`, `router-link`, and the CSS-only `tokens`
foundation. Dependencies between items are installed automatically. Managed CSS
blocks are marked with `leptos-ui-kit:start` and `leptos-ui-kit:end` comments.

For SSR or hydration, wrap generated IDs in `KitIdProvider` so server and browser
IDs remain aligned. Dialogs move to the document body by default; pass a
`portal_mount` inside a nested theme when the dialog must inherit that scope, or
set `portal_reparent=false` to keep it inline. `MenuContent` supports a
`StrictPlacementSink` for CSP policies that forbid inline styles.

## Theming

The `tokens` item defines the semantic `--kit-*` variables used by every styled
component. The versioned contract is packaged at
`registry/contracts/theme-v1.json`; its schema is
`schema/0.9.0-alpha/theme-contract.schema.json`.

Load theme CSS after the generated kit stylesheet and application CSS last:

```html
<link data-trunk rel="css" href="styles/kit.css" />
<link data-trunk rel="css" href="styles/themes.css" />
<link data-trunk rel="css" href="styles/app.css" />
```

Themes override semantic variables and set `color-scheme`. They can apply to
the document root or a nested subtree:

```css
:root {
  color-scheme: light;
  --kit-color-primary: #1d4ed8;
}

[data-ui-theme="dark"] {
  color-scheme: dark;
  --kit-color-surface-raised: #172033;
  --kit-color-text: #f9fafb;
}
```

Use `:root`, a class, or an attribute selector for root overrides; `html` has
lower specificity than the foundation `:root` selector. Theme names, selection
state, and persistence belong to the application.

After upgrading, run `leptos_ui_kit sync`. Untouched managed blocks migrate
automatically. Edited blocks are preserved; move custom rules outside the
markers, restore the generated block, and run `sync` again.

## License

Contributions are described in `CONTRIBUTING.md`. Licensed under either the MIT
License or the Apache License, Version 2.0, at your option.
