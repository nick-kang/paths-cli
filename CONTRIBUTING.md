# Contributing

## Setup

Install Git, Rust through rustup, Node.js 26, and npm. The repository's `rust-toolchain.toml` selects stable Rust with Clippy and rustfmt; `.node-version` specifies Node 26. Use the pnpm version pinned in `package.json` (currently 12.0.0).

```sh
git clone https://github.com/nick-kang/paths-cli.git
cd paths-cli
npm install --global pnpm@12.0.0
pnpm install --frozen-lockfile --ignore-scripts
```

Rust dependencies are locked in `Cargo.lock`; development dependencies are locked in `pnpm-lock.yaml`. Commit the corresponding lockfile when changing dependencies. TypeScript scripts run directly with Node; no compilation step or Python installation is needed.

## Development and checks

Run the CLI locally:

```sh
cargo run --locked -- --help
cargo run --locked -- react@19.0.0 --json
```

Before opening a pull request, run the checks used by CI:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --locked --release
node scripts/package.ts
node tests/smoke.ts
```

Use `cargo fmt --all` to apply formatting. Clippy's `all` and `pedantic` groups are enforced, `unwrap` and `expect` are denied, and unsafe Rust is forbidden.

The Rust tests cover dependency selection, repository resolution, and cache behavior. The smoke test uses real npm and pnpm installations against a temporary local registry and Git repository, then installs the packed CLI into an isolated global prefix. It does not change your normal global installation. Both package managers must be on `PATH`.

`node scripts/package.ts` stages the release binary for your current platform under `bin/`. Rebuild and stage it again after changing Rust code. Local packaging and smoke tests require one of the supported platforms listed in [README.md](README.md).

## Pull requests

Create a branch and open a pull request against `main`. Explain the problem, the resulting behavior, and how you verified it. Add a focused regression check for behavior changes or bug fixes. Keep generated binaries, npm tarballs, and `node_modules` out of commits.

Preserve the CLI's best-effort behavior: individual dependency failures must not stop the remaining requests, approximate source matches must be identified, and existing modified checkouts must not be overwritten. Package-manager support is limited to npm and pnpm.

## Deployment

Maintainers deploy by publishing a GitHub release. The [publish workflow](.github/workflows/publish.yml) builds and tests macOS and Linux on x64 and ARM64, plus Windows x64. After all five jobs pass, it verifies the binaries and publishes one npm package with provenance.

Pushes to `main`, pull requests, and manual workflow runs perform checks without publishing. Draft releases and prereleases do not publish to npm. Pushing a tag alone does not publish either; the trigger is publishing a non-prerelease GitHub release.

### Prepare a version

Choose an unused version, such as `0.1.2`:

1. Set the same version in `package.json` and the `[package]` section of `Cargo.toml`.
2. Run `cargo check` to update the root package version in `Cargo.lock`.
3. Run the checks above, review the version changes, and commit all three version files.
4. Merge or push the release commit to `main` and confirm its CI checks pass.

The release tag must be `v` followed by that exact version. For example, version `0.1.2` requires tag `v0.1.2`.

### Publish the release

Use GitHub's Releases page to publish a release targeting the prepared commit, or use the authenticated GitHub CLI after the version bump is on `main`:

```sh
gh release create v0.1.2 --repo nick-kang/paths-cli --target main --title v0.1.2 --generate-notes
```

Replace the example version with the version you prepared. Include user-facing changes in the release notes. Watch the release-triggered run in [GitHub Actions](https://github.com/nick-kang/paths-cli/actions/workflows/publish.yml); publication is complete only when its `publish` job succeeds.

### npm authentication

npm trusted publishing is already configured for package `paths-cli`, repository `nick-kang/paths-cli`, and workflow filename `publish.yml`, with no GitHub environment restriction. The workflow uses GitHub OIDC and needs `id-token: write`; it does not require an `NPM_TOKEN` secret or an interactive npm login.

If the repository or workflow filename changes, a package maintainer must update the [npm trusted publisher configuration](https://docs.npmjs.com/trusted-publishers/) to match. An authenticated maintainer can inspect it with `npm trust list paths-cli`.

### Verify or recover

After publication, verify the version, provenance metadata, and installation:

```sh
npm view paths-cli@0.1.2 version dist.attestations --json
npm install --global paths-cli@0.1.2
paths --version
```

npm may take a few minutes to make an accepted publication available. If the workflow fails, inspect its logs and check whether the version is already on npm before retrying. Published npm versions cannot be reused; fixes to a published release require a new version and tag.

Avoid publishing a package assembled from a local build: `scripts/package.ts` stages only your current platform's binary. The release workflow assembles all five binaries and checks them with `node scripts/package.ts --verify` before publishing.
