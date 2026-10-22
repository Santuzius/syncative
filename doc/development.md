# Development conventions

## Branches and versions
- Work happens on `develop` or a feature branch; `main` only moves at releases ([release.md](release.md)).
- The version lives in `[workspace.package]` of the root `Cargo.toml`; the app and the Go crate inherit it.
- A version bump is a commit of its own: `:bookmark: Bump crate to X.Y.Z`.
- If a bump to X was undone (reset or rebase) before X shipped, the next bump goes to the same X, not to a new patch level: a version marks what ships. `Cargo.lock` must follow `Cargo.toml` (`cargo metadata` or `cargo check`).

## Commits
- One subject line, `:gitmoji: Imperative summary`, usually without a body. The subject describes the change itself and still reads well months later, never a plan step such as "Phase 2:".
- One commit per logically separate change.
- Pick the most specific [gitmoji](https://gitmoji.dev), not `:sparkles:` by default:

| Gitmoji | For |
|---|---|
| `:sparkles:` | genuinely new functionality |
| `:bug:` | a bug fix |
| `:recycle:` | refactoring without a change in behaviour, even when it adds files |
| `:building_construction:` | architecture: new layers, reorganised modules, changed patterns |
| `:art:` | code structure or formatting |
| `:truck:` | moving or renaming files and folders |
| `:lipstick:` | anything that only changes how the UI looks (styling, fonts, icons, glyphs) |
| `:children_crossing:` | usability |
| `:zap:` | performance |
| `:passport_control:` | sign-in, permissions |
| `:closed_lock_with_key:` | stored secrets (keyring entries, credential storage) |
| `:bricks:` | build, CI, packaging and dev tooling |
| `:package:` | bundled files and packages |
| `:clown_face:` | mocks and stubs |
| `:construction:` | work in progress |
| `:fire:` / `:coffin:` | removing code / removing dead code |
| `:heavy_plus_sign:` / `:heavy_minus_sign:` / `:arrow_up:` | adding / removing / upgrading a dependency |
| `:memo:` | documentation and comments |
| `:page_facing_up:` | licences and credits |
| `:bookmark:` | version bumps |

## Files
- Helper scripts go into `scripts/`, temporary files into `temp/` (ignored by git) or `/tmp`.
- **`src/go/proton-api/` is read-only.** It is the `ProtonMail/go-proton-api` submodule and must keep tracking upstream unchanged. Syncative's additions live in the sibling package `src/go/proton-ext/`:
  - Methods on upstream types become free functions taking the type as a parameter.
  - Fields the upstream types lack are parsed from the raw JSON in `proton-ext`.
  - If something truly can't be done without changing the submodule, the feature is dropped instead.
- Forks of iced, iced_android and android-activity: see [android/README.md](../android/README.md) and [release.md](release.md#the-android-forks).

## UI text
- Explanations and lists in dialogs the user should read use the body size `TEXT` in the normal text colour, lists as indented bullets (`widgets::bullet`), like the Google Drive client-ID steps in *Add remote*.
- The small muted `CAPTION` is only for secondary fine print such as copyright lines.

## The rename from Celeste
The app was called Celeste until 0.22.2 and became Syncative in 0.23.0 (2026-10-12), because upstream Celeste keeps that name. Older commits, issues and notes say Celeste.
- Data folder `~/.local/share/celeste` → `~/.local/share/syncative`, moved at the first start by `src/infrastructure/persistence/migrations/celeste_to_syncative.rs`. Keyring service `Celeste Keys` → `Syncative Keys`, each entry moved when first read.
- Android app id `io.github.santuzius.celeste` → `io.github.santuzius.syncative`: a new app, installed alongside the old one.
- The GitHub repository is `Santuzius/syncative`; the test variables are `SYNCATIVE_TEST_*`.
