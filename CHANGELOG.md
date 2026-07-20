# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog and this project adheres to
Semantic Versioning.

## [Unreleased]

- Add a typed `shared-library-crate` project target that omits Trunk HTML
  mutation while preserving source-contained component and stylesheet output.
- Upgrade primitive-backed generated components and dependency plans to
  `web_ui_primitives` `0.2.0`.
- Freeze Presence ABI 2, cascade-layer ABI 1, and portal ABI 1 in the typed
  registry compatibility contract.
- Bind `transitioncancel` and `animationcancel` for generated menu and dialog
  presence, and place generated CSS in explicit kit token/component layers.
- Make installer writes advisory-locked, no-follow, no-clobber, journaled, and
  recoverable, with independent backups and finish-only committed cleanup.
- Publish every generated public file with deterministic POSIX mode `0644`
  while keeping transaction stages and recovery state private.
- Embed the complete built-in registry, Rust and CSS sources, theme contract,
  and package-local public schemas in one deterministic runtime catalog.
- Resolve package provenance from Cargo VCS metadata as a complete Git
  revision and expose stable logical locators instead of build-machine paths.
- Reject dirty, malformed, or wrong-crate Cargo VCS metadata instead of
  attributing changed archive bytes to an unqualified base revision.
- Expose typed logical built-in asset failures through public registry,
  registry-health, and theme-contract errors, and deprecate the physical-root
  content-hash compatibility API. Exhaustive error matches must handle the new
  variants.
- Add isolated archive acceptance proving both installed binaries keep working
  after package source, build targets, and Cargo caches are deleted.
- Pin the extracted package-only dependency graph independently from
  source-only ABI tests that consume unpublished upstream revisions.
- Add the packaged v1 semantic theme contract and CSS-only `tokens` foundation
  item, installed before every styled built-in component.
- Replace component `:root` aliases with property-local semantic and structural
  fallbacks while preserving existing component override names.
- Add scoped-theme support for `DialogContent` through its optional portal
  mount, while retaining the document-body default.
- Reconcile existing untouched generated CSS through `sync`, preserve edited
  managed blocks as conflicts, and extend strict doctor and fixture coverage.
- Reconcile each configured stylesheet atomically so the `tokens` foundation
  precedes its generated dependents while later application CSS keeps cascade
  precedence.
- Reorder untouched lock-owned component blocks when a registry upgrade adds a
  dependency edge, while continuing to reject locally edited managed blocks.
- Preserve explicit `--kit-menu-item-radius` values verbatim and apply derived
  radius arithmetic only to the semantic fallback.
- Route collapsible, dialog, menu, and tabs trigger border widths through the
  shared `--kit-border-width` semantic token.
- Report managed-CSS ownership, marker, and normalization failures against the
  configured stylesheet path and reject config/lock path disagreement.
- Make doctor validate config, lock, generated targets, ownership indexes, CSS
  order, and Cargo requirements from one resolved registry closure.
- Extend runtime package health checks across the registry root, manifests,
  referenced sources, theme contract, public schema, and their shared identity
  metadata.
- Enforce exact theme-contract, tokens-manifest, and built-in `:root` CSS
  declaration/default parity in the cached runtime registry health snapshot.
- Document theme stylesheet ordering, nested scopes, dependency requirements,
  and migration guidance.
- Document the complete committed `kit.json`/`kit.lock.json` workflow, exact
  `:root` specificity contract, safe scoped portal mounting, and the
  intentionally abbreviated public theme example.

## [0.1.0] - 2026-07-15

- Initial crates.io release.
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
- Add `src/components/ui/_kit/kit.lock.json` hash validation and exact managed
  CSS block doctor checks.
- Add desired-state `src/components/ui/_kit/kit.json` items, `sync`, strict
  doctor checks for desired/install drift, and the `cargo leptos_ui_kit`
  subcommand entrypoint.
- Add a Radroots-shaped workflow fixture that compiles the generated `Button`
  in a wasm Trunk CSR app.
- Add dependency-plan metadata for crates.io primitive dependencies without
  mutating consumer `Cargo.toml`.
- Add multi-file generated component targets and accessibility contract
  metadata for composite component families.
- Add primitive-backed generated `Collapsible`, `Tabs`, and `Dialog` component
  families using `web_ui_primitives`.
- Extend the workflow fixture to install Button, Collapsible, Tabs, and Dialog
  together, run `sync`, pass strict doctor, and compile for
  `wasm32-unknown-unknown`.
- Change canonical kit metadata to `src/components/ui/_kit/kit.json` and
  `src/components/ui/_kit/kit.lock.json`.
- Change the generated stylesheet default to `styles/kit.css`, allow
  `src/components/ui/_kit/kit.json` to choose another safe CSS file under
  `styles/`, and emit generated selectors and variables with the `kit` prefix.
- Add top-level and command-specific help plus `--version` output.
- Keep generated `Button` option enums warning-clean for consumer binary apps.
