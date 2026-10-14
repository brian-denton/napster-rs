# Releasing napster-rs

## Cutting a release

1. Set `version` in the workspace `Cargo.toml`. Clients compare it with
   the tag to decide whether an update is newer.
2. Commit and push.
3. Tag the commit and push the tag:

   ```bash
   git tag -a v0.5.0 -m "napster-rs v0.5.0"
   ```
   ```bash
   git push origin v0.5.0
   ```

Pushing the commit and pushing the tag each start a run. The commit's run
only tests. The tag's run does the release.

The tag run:

1. tests on Windows, macOS and Linux;
2. builds the three packages;
3. signs each package, writing a `<package>.sig` next to it;
4. publishes a GitHub release with the packages and their signatures.

Running clients find the release on their next start, or through **Help >
Check for Updates...**.

## The signing key

Updates are signed with Ed25519. The two halves live in different places:

| Half | Where it lives |
|---|---|
| Public key | `RELEASE_PUBLIC_KEY` in `crates/napster-core/src/update.rs`, compiled into every client |
| Secret key | The `NAPSTER_SIGNING_KEY` Actions secret on GitHub, plus the maintainer's backup |

The release fails if the secret is missing, so an unsigned package is
never published. Clients ignore releases without a valid `.sig` for their
platform.

### Keep a backup

The secret key was generated with `napster-sign keygen` and saved to
`~/.napster-rs/release-signing.key` on the maintainer's machine. Store a
copy in a password manager.

If the key is lost, releases can't be signed. Installed clients will then
never accept another update, and their users have to reinstall by hand
once you switch to a new key.

### Rotating the key

Rotate the key if it leaks, or if you lose it.

1. Generate a new key pair:

   ```bash
   cargo run -p napster-sign -- keygen ~/.napster-rs/release-signing-2.key
   ```

2. Put the printed public key in `RELEASE_PUBLIC_KEY`.
3. Store the new secret on GitHub:

   ```bash
   gh secret set NAPSTER_SIGNING_KEY < ~/.napster-rs/release-signing-2.key
   ```

4. Release.

Clients carrying the old key will reject packages signed with the new
one. If the old key is still trustworthy, you can make the move without a
manual reinstall: publish one release built with the new public key, but
signed with the old key. Then switch the secret.

### Checking a package by hand

```bash
cargo run -p napster-sign -- verify aiFkY0TOpiOanimBRxp9THtriZPc5x7pZ3RsROOptfo= napster-rs-windows-x64.zip
```
