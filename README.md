# leptos_ui_kit

Source-first UI components for Leptos `0.9.0-alpha`.

`leptos_ui_kit` provides a CLI and packaged registry for installing editable,
app-owned Rust and CSS into Trunk CSR apps.

## Install

```bash
cargo install leptos_ui_kit_cli --locked
```

Apps do not depend on `leptos_ui_kit` at runtime. The CLI installs source files
under `src/components/ui` and managed CSS blocks in `styles/kit.css` by
default. `kit.json` may select another safe stylesheet under `styles/`.

The installed binaries embed the built-in registry manifests, Rust and CSS
sources, theme contract, and public schemas in a deterministic catalog. They
do not require a package source checkout, build target, or Cargo cache at
runtime. Clean Cargo package installations also retain the complete package
Git revision reported by `--version --json`; dirty or wrong-crate package VCS
metadata is rejected rather than reported as trusted provenance.

The catalog is parsed and validated as one immutable snapshot on first use.
Validation covers every manifest and referenced source, all packaged schemas,
and exact parity between the theme contract and the sole built-in `:root`
token declarations. `doctor` reports any embedded-registry health failure in
both ordinary and strict modes using logical package paths.

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

A supported app provides Leptos with the `csr` feature as its base dependency
for generated Rust components:

```toml
[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
```

Only `collapsible`, `dialog`, `menu`, and `tabs` additionally require:

```toml
[dependencies]
web_ui_primitives = { version = "0.1.0", features = ["leptos"] }
```

`router-link` additionally requires:

```toml
[dependencies]
leptos_router = "0.9.0-alpha"
```

The CSS-only `tokens` item has no item-specific Cargo requirements. The
remaining Rust built-ins require only the Leptos base. The CLI resolves the
configured items and their registry dependency closure, merges the complete
Cargo requirement plan, and verifies that same closure-authoritative plan with
`doctor --strict`. It reports required manifest changes but never edits
`Cargo.toml`.

## Components

The built-in registry includes `button`, `collapsible`, `tabs`, `dialog`,
`menu`, `field`, `status`, `spinner`, `anchor`, `router-link`, and the
CSS-only `tokens` foundation item.

Generated source is app-owned. Managed CSS is delimited with
`leptos-ui-kit:start` and `leptos-ui-kit:end` markers.

## Theming

The `tokens` foundation owns the canonical semantic `--kit-*` token contract.
Every styled component directly depends on it, so adding a component installs
the tokens managed block before the component styles. The machine-readable
contract is packaged at `registry/contracts/theme-v1.json` and its public JSON
schema is published at
`schema/0.9.0-alpha/theme-contract.schema.json`.

Load application theme CSS after the generated kit stylesheet, then keep
application rules last:

```html
<link data-trunk rel="css" href="styles/kit.css" />
<link data-trunk rel="css" href="styles/themes.css" />
<link data-trunk rel="css" href="styles/app.css" />
```

Themes own semantic values and their `color-scheme` declaration. Component
styles resolve those values at the property that uses them, so a nested theme
scope works without component-level root aliases:

```css
:root {
  color-scheme: light;
  --kit-color-primary: #1d4ed8;
  --kit-focus-ring: #2563eb;
}

[data-ui-theme="dark"] {
  color-scheme: dark;
  --kit-color-surface-raised: #172033;
  --kit-color-text: #f9fafb;
  --kit-color-border: #374151;
}
```

Existing component variables remain optional escape hatches. For example,
`--kit-button-gap`, `--kit-dialog-background`, and `--kit-spinner-track-color`
can still be set by an app, but their defaults now fall back to semantic tokens
or component-local structural values.

`DialogContent` normally portals to the document body. For a dialog opened
inside a nested theme scope, mount it below that scope instead:

```rust
use web_ui_primitives::leptos::PortalMount;

let portal_mount: PortalMount = /* an element below the themed scope */;

view! {
  <DialogContent portal_mount=portal_mount>
    <p>"Dialog content"</p>
  </DialogContent>
}
```

Omit `portal_mount` to keep the body default. Theme selection, named-theme
state, and persistence belong to the consuming application rather than this
kit.

### Migrating existing generated CSS

Run `leptos_ui_kit sync` after upgrading. An untouched tracked managed block is
rewritten to the semantic fallback form, the `tokens` block is installed, and
the lock and desired state are reconciled. The configured stylesheet is
reconciled as one transaction: the foundation block is placed before its
generated dependents, while later application-owned declarations remain after
the generated defaults and retain cascade precedence. A locally edited managed
block is never overwritten. Move custom declarations outside the managed
markers (for example, into `styles/themes.css`), restore or reinstall the
generated block, run `sync`, then reapply the application-owned overrides after
the configured kit stylesheet.

## Version

Package version `0.1.0` targets Leptos `0.9.0-alpha`.

## Contributing

See `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
