# Releasing tolk-inspect

Releases are built from a clean GitHub-hosted runner and published to npm by
`.github/workflows/release.yml`.

## One-time setup

1. Create or claim the public `tolk-inspect` package on npm and enable account 2FA.
2. Create a protected GitHub environment named `npm`. Add required reviewers if desired.
3. In the npm package settings, configure GitHub as a trusted publisher:
   - owner: `racinette`
   - repository: `tolk-inspect`
   - workflow: `release.yml`
   - environment: `npm`
   - allowed action: direct publish
4. Remove long-lived npm publication tokens after trusted publishing succeeds.

The release workflow requires no `NPM_TOKEN`. Its OIDC identity produces npm provenance
automatically for a public repository and package.

## Prepare a release

1. Update `[workspace.package].version` in `Cargo.toml`.
2. Update `packages/tolk-inspect/package.json` with the same version.
3. Run `npm install --package-lock-only --ignore-scripts` and `cargo check --workspace` to
   refresh both lockfiles.
4. Move the pending changelog entries under the version and release date.
5. Run the complete local gate:

   ```bash
   npm ci
   export WASI_SDK_PATH=/path/to/wasi-sdk
   npm run release:check
   ```

6. Commit and push the release preparation. The normal CI workflow must pass.

`npm run check:versions` checks the npm manifest, npm lockfile, Cargo workspace, and both
Cargo packages. The package's `prepack` hook always rebuilds Rust/WASM and TypeScript, so a
clean publication cannot reuse a missing or stale generated binary.

## Publish

1. Create a GitHub release targeting the reviewed commit with tag `v<version>`.
2. Publish the GitHub release. Prereleases are intentionally not published by the workflow.
3. Approve the protected `npm` environment deployment if required.
4. Confirm the workflow completed and npm displays provenance for the new version.
5. Smoke-test the registry artifact from an empty directory:

   ```bash
   npm init -y
   npm install tolk-inspect@<version>
   node --input-type=module -e 'import { versionInfo } from "tolk-inspect"; console.log(versionInfo())'
   ```

6. Confirm the reported package version and Acton revision, then close the milestone.

Never reuse or move a published version tag. Fix a bad release with a new patch version.
