# Workshop release channel

Machine-readable release pointers, updated by the release workflow. Do not edit by hand;
to roll a channel back, revert the commit that moved it.

| File | Purpose |
|---|---|
| `stable.json`, `alpha.json` | Channel manifests: version, per-platform asset URLs and SHA-256 (`channel-manifest.schema.json`) |
| `install.sh` | Installer: `curl -fsSL https://raw.githubusercontent.com/vagdotdev/grokbuildfork/release-channel/install.sh \| sh` (`WORKSHOP_CHANNEL=alpha` for alpha) |

Releases: https://github.com/vagdotdev/grokbuildfork/releases
