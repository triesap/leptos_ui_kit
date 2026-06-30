# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project adheres to
Semantic Versioning.

## [0.9.0-alpha] - Unreleased

- Align the crate family with the Leptos `0.9.0-alpha` version line.
- Define the MVP as a source-first `leptos_ui_kit` CLI and packaged registry for
  pure-CSS Trunk CSR apps.
- Scope the MVP to built-in `button`, strict config, no Tailwind, no shadcn
  compatibility, no Cargo manifest mutation, and no SSR/hydration/islands
  support.
- Rename the installed CLI binary to `leptos_ui_kit`.
- Support single-package workspace-root Trunk CSR apps.
- Add `ButtonType`, reactive `disabled`, and app class passthrough to the
  generated `Button`.
- Expand generated button CSS tokens for app-owned theming.
- Tighten `.leptos-ui` state hash validation and exact managed CSS block
  doctor checks.
- Add desired-state `components.json` items, `sync`, strict doctor checks for
  desired/install drift, and the `cargo leptos_ui_kit` subcommand entrypoint.
- Add a Radroots-shaped workflow fixture that compiles the generated `Button`
  in a wasm Trunk CSR app.
- Add dependency-plan metadata for pinned git/rev primitive dependencies without
  mutating consumer `Cargo.toml`.
- Add multi-file generated component targets and accessibility contract
  metadata for composite component families.
- Add primitive-backed generated `Collapsible`, `Tabs`, and `Dialog` component
  families using `web_ui_primitives`.
- Extend the workflow fixture to install Button, Collapsible, Tabs, and Dialog
  together, run `sync`, pass strict doctor, and compile for
  `wasm32-unknown-unknown`.
- Change the canonical installer state directory to
  `src/components/ui/_kit`, with strict `components.json` `state.dir`
  validation and configurable init support.
- Change the generated stylesheet default to `styles/kit.css`, allow
  `components.json` to choose another safe CSS file under `styles/`, and emit
  generated selectors and variables with the `kit` prefix.
- Add `leptos_ui_kit migrate state-dir <path>` for explicit state and baseline
  migrations.
- Add top-level and command-specific help plus `--version` output.
- Keep generated `Button` option enums warning-clean for consumer binary apps.
