# Contributing

Thanks for your interest in contributing to leptos_ui_kit.

## Ways to help

- Report bugs and regressions
- Improve documentation and examples
- Improve the MVP `button`, registry, codegen, and CLI flows
- Expand accessibility and keyboard coverage

## Development setup

This repository is a Rust workspace. Run the normal validation lane with:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
```

Transaction changes must also keep the codegen all-target suite green on
Linux, macOS, and Windows. The repository workflow runs that portable matrix.

## Packaging validation

Changes to package manifests, include lists, embedded assets, provenance, or
the CLI installation flow also require the package lane:

```bash
cargo package --workspace --allow-dirty --no-verify --locked
```

Run both slow package acceptance tests explicitly:

```bash
cargo test -p leptos_ui_kit_registry --test package_source \
  packaged_sources_build_with_cargo_vcs_provenance_outside_and_inside_hostile_git -- \
  --ignored --exact --nocapture

cargo test -p leptos_ui_kit_cli --test packaged_runtime \
  installed_binaries_run_after_package_source_and_build_state_are_deleted -- \
  --ignored --exact --nocapture
```

These ignored tests create and extract package archives in isolated workspaces.
They prove that packaged crates use only local package inputs and that installed
binaries keep working after package source and build state are deleted. Run
them from a clean Git worktree; archive metadata marked dirty is rejected
intentionally because its base revision does not identify the packaged bytes.

## MVP constraints

- Target Leptos `0.9.0-alpha`.
- Use pure CSS only.
- Do not add Tailwind support.
- Do not add shadcn compatibility shims, alias maps, legacy config fields, or
  duplicate command names.
- Do not make the CLI mutate `Cargo.toml` in the MVP.
- Keep runtime primitives render-feature neutral.

## Pull request checklist

- Keep changes focused and well-scoped
- Add or update tests when behavior changes
- Keep public APIs documented
- Avoid introducing new unsafe code

## Code style

- Use idiomatic Rust
- Prefer small, composable helpers
- Favor clear, explicit APIs over cleverness

## Accessibility

All components should follow WAI-ARIA APG patterns where applicable.
If behavior changes affect keyboard interaction or focus, include tests.

## License

By contributing, you agree that your contributions are released under the
project license (MIT OR Apache-2.0).
