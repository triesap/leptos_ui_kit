# leptos_ui_kit

Source-first UI components for Leptos `0.9.0-alpha`.

`leptos_ui_kit` provides a CLI and packaged registry for installing editable,
app-owned Rust and CSS into Leptos applications.

## Install

```bash
cargo install leptos_ui_kit_cli --locked
```

The package name is the positional `cargo install` argument. No
`--state-dir`, migration subcommand, or separate `components.json` file is
used.

Apps do not depend on `leptos_ui_kit` at runtime. The CLI installs source files
under `src/components/ui` and managed CSS blocks in `styles/kit.css` by
default. The committed desired-state file is
`src/components/ui/_kit/kit.json`; `init`, `add`, and `sync` maintain the
corresponding `src/components/ui/_kit/kit.lock.json`. `kit.json` may select
another safe stylesheet under `styles/`. Commit both files and the generated
app-owned source and stylesheet so `sync` and `doctor --strict` can detect
drift deterministically.

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

## Supported Project Shapes

```text
Cargo.toml
index.html
styles/kit.css
src/
```

The app may be a single crate or a single-package workspace root.
Generated source may also target a library crate when `kit.json` declares
`project.kind` as `shared-library-crate`; that target omits `indexHtml` and
does not patch Trunk HTML. Invoke generation from the package root even when
the library is a member of a larger workspace. Multi-member workspace-root
installs are not supported.

The project and dependency contracts distinguish source compatibility from a
selected final delivery mode. Trunk CSR, native SSR, and browser hydration
deliveries are supported compatibility targets; a shared-library target does
not force one of those modes into downstream consumers.

The corresponding `project.kind` values are
`single-crate-trunk-csr`, `single-crate-native-ssr`,
`single-crate-browser-hydration`, and `shared-library-crate`. Final delivery
configs require the matching `leptos.renderMode` (`csr`, `ssr`, or `hydrate`);
shared-library configs omit `leptos.renderMode`. `info --json` reports the
configured render contract, the detected dependency selection, and the
qualified render mode independently so a missing, mismatched, or mixed mode
cannot be mistaken for a neutral shared-library dependency.

## Dependency Plan

A supported final delivery provides Leptos with exactly one of `csr`,
`hydrate`, or `ssr` as its base dependency for generated Rust components. A
render-neutral shared library leaves that selection to its final consumer.
For example, a Trunk CSR delivery uses:

```toml
[dependencies]
leptos = { version = "0.9.0-alpha", features = ["csr"] }
```

Only `collapsible`, `dialog`, `menu`, and `tabs` additionally require:

```toml
[dependencies]
web_ui_primitives = { git = "https://github.com/triesap/web_ui_primitives", rev = "a7ad19e203c08be19040154fa6bce909701d402f", features = ["core", "leptos"] }
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
CSS-only `tokens` foundation item. The generated `identity` support item is
installed automatically with components that emit related element IDs.

Generated source is app-owned. Managed CSS is delimited with
`leptos-ui-kit:start` and `leptos-ui-kit:end` markers.

The registry root publishes a fail-closed compatibility contract for Leptos
`0.9.0-alpha`, `web_ui_primitives` `0.2.0`, Presence ABI 2, identity ABI 1,
cascade-layer ABI 1, and portal ABI 1. Menu and dialog surfaces bind both
completion and cancellation events, so interrupted transitions and animations
cannot strand presence state.

SSR/hydration compatibility additionally requires owner/request-stable
generated IDs, hydration-safe portal structure, and a strict placement sink
for CSP deployments that reject inline style attributes. Registry capability,
generated source, fixtures, and dependency plans advance together when those
ABIs change.

Wrap a rendered app or request subtree in `KitIdProvider` when generated IDs
can appear. The provider owns deterministic per-prefix ordinals for that
Leptos owner/request, so independent SSR requests cannot share counters and
the hydration walk reproduces the server IDs. Each affected component still
accepts its existing explicit ID override. The owner-scoped fallback keeps
standalone component examples usable, but an app-level provider is the
canonical SSR/hydration composition:

```rust
view! {
  <KitIdProvider>
    <App />
  </KitIdProvider>
}
```

`MenuContent` retains the inline-style placement adapter for qualified CSR
consumers. A strict-CSP delivery selects an authorized stylesheet sink and
uses a stable validated ID. In that mode the menu emits
`data-web-ui-placement-id` and omits the inline `style` attribute:

```rust
use web_ui_primitives::leptos::{
    PlacementSink, PlacementStyleId, PlacementStyleNonce, StrictPlacementSink,
};

let placement_sink = PlacementSink::StrictStylesheet(
    StrictPlacementSink::new(
        PlacementStyleId::new("account-menu-placement")?,
    )
    .authorized(PlacementStyleNonce::new(csp_nonce)?),
);

view! {
  <MenuRoot id="account-menu">
    <MenuTrigger>"Account"</MenuTrigger>
    <MenuContent placement_sink=placement_sink>
      // Menu items.
    </MenuContent>
  </MenuRoot>
}
```

The nonce must be produced by the server’s CSP boundary. Missing or invalid
authorization fails closed; callers must not substitute `unsafe-inline` or
arbitrary CSP fragments.

## Theming

The `tokens` foundation owns the canonical semantic `--kit-*` token contract.
Every styled component directly depends on it, so adding a component installs
the tokens managed block before the component styles. The machine-readable
contract is packaged at `registry/contracts/theme-v1.json` and its public JSON
schema is published at
`schema/0.9.0-alpha/theme-contract.schema.json`.

Generated CSS declares the stable cascade order
`leptos-ui-kit.tokens`, `leptos-ui-kit.themes`, then
`leptos-ui-kit.components`. Token defaults live in the token layer and
component rules live in the component layer; the theme compiler owns the
middle theme layer.

Load application theme CSS after the generated kit stylesheet, then keep
application rules last:

```html
<link data-trunk rel="css" href="styles/kit.css" />
<link data-trunk rel="css" href="styles/themes.css" />
<link data-trunk rel="css" href="styles/app.css" />
```

Themes own semantic values and their `color-scheme` declaration. Component
styles resolve those values at the property that uses them, so a nested theme
scope works without component-level root aliases. The following fragment is
intentionally abbreviated; omitted contract tokens inherit the foundation
defaults:

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

The normative v1 foundation selector is `:root`. Source order resolves rules
only when their specificity is equal: an `html` rule has lower specificity and
cannot override `:root` merely by appearing later. Root-level application
themes should therefore use `:root`, a class, or an attribute selector with
equal or greater specificity. Changing the foundation to `:where(:root)` would
be a versioned theme-contract decision.

Existing component variables remain optional escape hatches. For example,
`--kit-button-gap`, `--kit-dialog-background`, and `--kit-spinner-track-color`
can still be set by an app, but their defaults now fall back to semantic tokens
or component-local structural values.

`DialogContent` renders one deterministic portal container for SSR and
hydration, then reparents that same container to the document body after the
hydration walk. Set `portal_reparent=false` to retain it inline. For a dialog
opened inside a nested theme scope, place a stable mount element below that
scope and look it up without assuming that a browser DOM exists:

```rust
use web_ui_primitives::leptos::PortalMount;

#[cfg(target_arch = "wasm32")]
fn themed_dialog_mount() -> Option<PortalMount> {
    leptos::web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("dark-theme-dialog-mount"))
}

#[cfg(not(target_arch = "wasm32"))]
fn themed_dialog_mount() -> Option<PortalMount> {
    Some(())
}

view! {
  <DialogRoot>
    <DialogTrigger>"Open themed dialog"</DialogTrigger>
    <DialogContent portal_mount=themed_dialog_mount()>
      <DialogTitle>"Theme settings"</DialogTitle>
      <DialogDescription>"Update the settings for this theme."</DialogDescription>
      <DialogClose>"Close"</DialogClose>
    </DialogContent>
  </DialogRoot>
}
```

The wasm helper returns the real scoped element when it is present. Its `None`
fallback preserves the component's document-body behavior, while the
non-browser helper supplies the host `PortalMount` so the explicit call site
also compiles during host checks. Omit `portal_mount` entirely at call sites
that always use the body default.

Keep a scoped portal mount outside transformed, filtered, perspective, clipping,
and containment ancestors that would change fixed-position containing blocks
or clip the dialog overlay. Theme selection, named-theme state, and persistence
belong to the consuming application rather than this kit.

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

Licensed under either the MIT License or the Apache License, Version 2.0, at
your option.
