# Contributing

Workshop is a small personal project built as an overlay on the public
[Grok Build](https://github.com/xai-org/grok-build) tree. Bug reports and pull requests are welcome
on GitHub; keep them small and open them against `main`.

## Ground rules

- **Upstream crates keep their names.** `crates/codegen/xai-grok-*` and `crates/common/xai-*` are
  Grok Build's; Workshop lives in `crates/workshop-*` and in a short patch series. Do not rename,
  vendor or fork an upstream crate.
- **Editing an upstream file:** edit it in place (the tree keeps the patched state), run
  `scripts/regenerate-patches.sh`, and commit `patches/` together with the change. A new upstream
  path goes into the right group in `patches/groups.txt`. Gate-tagged patches are never commented
  out of `patches/series`.
- **The gates must stay green:** `cargo test -p workshop-gates` and `scripts/no-xai-scan.sh
  --sources` (the required `no-xai` check in CI). A change that re-enables an xAI endpoint, the
  upstream updater or login by default is not accepted.
- **Nothing a user reads names the other product**, except the labeled optional xAI card on
  `/auth`. The branding gate in `crates/workshop-gates/tests/branding.rs` checks the usual places.

See [`README.md`](README.md) for the layout, the build and the gates, and
[`docs/upstream-sync.md`](docs/upstream-sync.md) for how upstream releases come in.

## Security reports

Report vulnerabilities privately through the process in [`SECURITY.md`](SECURITY.md). Do not open
a public issue for them.

## License

By contributing you agree that your contribution is licensed under the Apache License, Version 2.0
(see [`LICENSE`](LICENSE)), the license of this repository and of the upstream tree it builds on.
