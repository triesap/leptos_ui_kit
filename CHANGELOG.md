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
- Add a Radroots-shaped workflow fixture that compiles the generated `Button`
  in a wasm Trunk CSR app.
