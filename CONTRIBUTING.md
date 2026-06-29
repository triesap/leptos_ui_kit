# Contributing

Thanks for your interest in contributing to leptos_ui_kit.

## Ways to help

- Report bugs and regressions
- Improve documentation and examples
- Improve the MVP `button`, registry, codegen, and CLI flows
- Expand accessibility and keyboard coverage

## Development setup

This repository is a Rust workspace. Typical tasks:

- `cargo fmt`
- `cargo check`
- `cargo test`

When working from a parent repository that provides a build wrapper, use that
parent repository's command routing instructions.

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
