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
The registry includes:

- actions and navigation: `anchor`, `button`, `router-link`, and `tabs`;
- disclosure and overlays: `collapsible`, `dialog`, and `menu`;
- forms and selection: `field`, `checkbox`, `radio`, and `switch`;
- surfaces and identity: `avatar`, `badge`, and `card`;
- feedback and display: `alert`, `progress`, `separator`, `skeleton`, `spinner`,
  and `status`;
- foundations: `identity` and the CSS-only `tokens` item.

Higher-level patterns stay compositional: an accordion is a set of
`collapsible` items, notifications use `alert` or `status`, and breadcrumbs or
pagination compose `anchor`, `router-link`, and `button`. Native semantic HTML
remains the default for data tables and document structure.

## Themes

The `tokens` item defines the semantic `--kit-*` variables used by components.
Load application theme CSS after `styles/kit.css`. Theme selectors own their
values and `color-scheme`; theme selection and persistence remain application
concerns.

Nested theme scopes work directly. When a dialog must inherit a nested scope,
pass a `portal_mount` inside it. After upgrading the kit, run `sync`; untouched
managed blocks migrate automatically and edited blocks remain unchanged.

### Component customization

Theme tokens describe portable design decisions. Optional component custom
properties provide the runtime CSS API and are governed separately by
`registry/contracts/component-customization-v1.json`.

Radius customization follows this precedence:

```text
component property -> semantic role -> --kit-radius-default -> reference radius
```

For example, an application may set `--kit-radius-default` once, refine all
controls with `--kit-radius-control`, and still override one component with
`--kit-button-radius`. Shape-critical geometry such as the spinner remains
circular unless its exact component property is set explicitly. Component
radius properties accept the complete CSS `border-radius` value, including
multi-corner and elliptical forms.

```css
:root {
  --kit-radius-default: 0.375rem;
  --kit-radius-control: 0.25rem;
  --kit-radius-overlay: 0.75rem;
  --kit-button-radius: 999px;
}
```

Unset properties preserve the pre-contract component shapes. Invalid custom
property values follow normal CSS computed-value behavior; the kit does not
register them with `@property`, so full `border-radius` grammar remains valid.

See [CONTRIBUTING.md](CONTRIBUTING.md).

Licensed under either the MIT License or the Apache License, Version 2.0, at your option.
