# Dependency updates

Dependabot (`.github/dependabot.yml`) looks for updates every Monday and opens pull requests into `develop`: Rust crates, Go modules, the flake's inputs and the GitHub Actions. A new version is proposed only once it has been out for three days, Dependabot's default cooldown. GitHub reads the configuration from `main`.

- **Groups:** one pull request for all patch updates and one for all minor updates of each language; each major update gets its own.
- **Left out:** the iced, iced_android and android-activity forks, which move together by hand ([release.md](../packaging/release.md#the-android-forks)), the rclone fork and resty, pinned through `replace` ([release.md](../packaging/release.md#the-rclone-fork)), and every `github.com/ProtonMail/*` module: they come with go-proton-api and follow its versions, and some are Proton's own `-proton` builds, which an upstream release would replace. The AppImage's base image `ubuntu:22.04` sets its minimum glibc and stays as it is.

## Checks and merging

`.github/workflows/check.yml` runs on GitHub's runners for every pull request into `develop`, Dependabot's and anyone else's:

1. **vendorHash** (Dependabot only): if `src/go/go.sum` changed, it builds once with `lib.fakeHash`, puts the hash the build names into `nix/package.nix`, and pushes that commit onto the pull request.
2. **Build and tests:** `nix build .#syncative-source`, which also runs `cargo test`, and `go test ./...` in `src/go`.
3. **Merge** (Dependabot only, after both pass): squashed into `develop` if every update in the pull request is safe by its version number:
   - a patch update (`1.4.2 → 1.4.3`, `0.13.1 → 0.13.2`)
   - a minor update from 1.0 on (`1.4.2 → 1.5.0`), which semver keeps compatible
   - a flake input moving to a newer commit, which has no version number; the build has to pass

   A major update, a minor update below 1.0 (`0.13 → 0.14`, where semver allows breaking changes), a pre-release or an update without readable details stays open for review.

The tests don't cover the providers, the GUI or Android at run time, so an update that passed can still break something. It reaches users only with a release: test `develop` before releasing, and `git revert` the squashed commit of an update that broke it.
