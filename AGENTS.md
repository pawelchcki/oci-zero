# Agent & contributor guide

Guidance for humans and AI agents working in this repository.

## Pull request titles

**PR titles must follow [Conventional Commits](https://www.conventionalcommits.org/).**
This is not cosmetic: PRs are squash-merged, so the **PR title becomes the commit
message on `main`**, and [release-plz](https://release-plz.dev) parses those commit
messages to decide version bumps and to generate each crate's changelog. A sloppy
title produces a wrong version bump and a bad changelog entry.

### Format

```
<type>[optional (scope)][optional !]: <description>
```

- Use the imperative mood, lower-case, no trailing period.
- Add `!` (or a `BREAKING CHANGE:` footer) for breaking changes.

### Types and how release-plz treats them

| Type       | Meaning                                   | Version effect (pre-1.0) |
| ---------- | ----------------------------------------- | ------------------------ |
| `feat`     | New user-facing capability                | minor bump               |
| `fix`      | Bug fix                                    | patch bump               |
| `perf`     | Performance improvement                   | patch bump               |
| `refactor` | Behaviour-preserving code change          | patch bump               |
| `docs`     | Documentation only                        | no release               |
| `test`     | Tests only                                | no release               |
| `ci`       | CI / workflow changes                     | no release               |
| `build`    | Build system or dependency changes        | no release               |
| `chore`    | Maintenance not covered above             | no release               |

A `feat!:` / `fix!:` (or `BREAKING CHANGE:` footer) triggers a breaking bump.

### Scopes

Prefer a crate name as the scope so changelogs attribute correctly:
`oci-zero`, `gzip-zero`, `zstd-zero`. Example: `fix(gzip-zero): reject trailer with wrong ISIZE`.

### Examples

```
feat(oci-zero): stream blob digests without buffering
fix(gzip-zero): reject members with a corrupt header CRC
perf(zstd-zero): avoid recomputing the FSE table per block
docs: document the release-plz workflow in AGENTS.md
chore: bump miniz_oxide to 0.9.1
feat(oci-zero)!: rename Decoder::decode to Decoder::step
```

PR titles are enforced in CI by `.github/workflows/pr-title.yml`.

## Releasing

Releases are automated by release-plz (`.github/workflows/release-plz.yml`):

1. Merges to `main` cause release-plz to open/update a **Release PR** that bumps
   versions and updates changelogs based on the merged commit messages.
2. Merging that Release PR publishes any bumped crates to crates.io via
   [trusted publishing](https://crates.io/docs/trusted-publishing) (OIDC — no
   stored registry token) and creates git tags and GitHub Releases.

Do not run `cargo publish` by hand; let the pipeline do it.

## Crates

- `oci-zero` — no-std, allocation-free streaming core for OCI registries (published)
- `gzip-zero` — no-std streaming gzip decoder (published)
- `zstd-zero` — no-std streaming Zstandard decoder (published)
- `oci-zero-no-std-extract`, `oci-zero-web` — examples/tools, `publish = false`
