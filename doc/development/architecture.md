# Architecture

Syncative is one process: the Rust code in `src/`, with the Go code in `src/go/` linked in as a static archive. On Android, a thin Java layer in `android/` adds what only Android's own classes can do.

## Parts of the process
- **Window** (`app`, `screens`, `widgets`): the iced GUI. On the desktop it can close while the process stays in the tray; on Android the native library keeps running after the activity is gone.
- **Sync engine** (`engine`): its own thread with a small tokio runtime, so syncing goes on without a window. The window sends it `engine::Command`s and watches `engine::Snapshot`s. The engine only reads the database; the window writes it and then sends `Command::Reload`.
- **Go library** (`src/go/`, Rust bindings in the `syncative-go` crate in `src/go/src/`): rclone with its backends, Syncative's own Proton Drive backend and the network drive mounts. Rust calls it through rclone's RPC interface (`syncative_go::rpc`) and a few C entry points of its own.
- **Java** (`android/app/src/main/java/…/syncative/`):
  - `SyncativeApplication` sets up the environment and loads the native library in every process.
  - `SyncativeActivity` hosts the iced window and adds permissions, the folder picker, the file dialogs and files shared from other apps.
  - `SyncService` keeps the process alive while Syncative is not on screen; `BootReceiver` starts it after a reboot.
  - `NetworkDriveProvider` shows the network drive folders to other apps.
  - `Bridge` holds the static helpers the native library calls.

## Start-up
`lib.rs` sets up everything once per process (`services()`), whoever starts it: the window, `--save`, or on Android the sync service.
1. It takes over stderr to catch rclone's rate-limit warnings, and moves data left by Celeste, the previous name.
2. It makes sure only one process owns the data directory (`infrastructure::single_instance`); a second start hands its request to the first and exits.
3. It starts the Go library and points rclone at its config file, which lives in `$XDG_RUNTIME_DIR`: rclone encrypts it under a password kept in the keyring, which also holds the durable copy.
4. It opens the SQLite database and runs the migrations.
5. It builds the `ClientRouter`, which sends each remote's calls to rclone or to the Proton client, and resumes the saved Proton sessions.
6. It starts the network monitor on the desktop and then the engine.

## Layers
The Rust code in `src/` is split into layers; each uses only the layers named in its row:

| Layer | Holds | Uses |
|---|---|---|
| `domain` | types and port traits (`domain::ports`), no I/O | nothing |
| `infrastructure` | adapters: database, rclone and Go, keyring, files outside sync folders, D-Bus, JNI | `domain` |
| `engine` | the sync thread: scheduling, one pass per remote, the network drives; `engine/folder` syncs one folder | `domain`, `infrastructure`; `engine/folder` only the port traits |
| `services` | the features around the sync: sign-in, sharing, preferences, folder helpers | `domain`, `infrastructure`, `engine` |
| `app`, `screens`, `widgets` | the window | everything below |
| `inbound` | entry points through which the platform calls Syncative on its own: Android's JNI callbacks from the sync service, the network watch, shared files and the document provider | everything below `app` |

- `engine/folder` reaches the remote and the database only through `BackendClient` and `Repository`, so its tests run it against the fakes in `test_support`. A unit test (`the_folder_sync_uses_no_adapter_directly`) fails once a file there names `infrastructure` or the Go library.
- The window talks to the engine through its commands; only `engine::shutdown()`, which unmounts the network drives on quit, is called directly.
- An adapter that reports on its own gets its handlers from whoever starts it: `lib.rs` hands the desktop portal's network monitor the functions of `services::network`, and the second-start socket yields requests that the window handles. JNI entry points that only answer a call Syncative made, such as a picked folder, stay with their adapter.
- The one exception is the Proton client, which calls into sign-in, the network state and the network drives until its session moves to Go ([todo.md](../todo.md)).

## Where to look
| Task | Start in |
|---|---|
| How a folder is compared and synced | `engine/folder` (snapshot, planner, applier) and [remote-changes.md](remote-changes.md) |
| When passes run, cadence, offline | `engine/core.rs`, `services::preferences::power`, `services::network` |
| A new provider | `domain::remote::ProviderKind`, `services::auth`, and the Go side in `src/go/rclone/` or `src/go/proton/` |
| Database tables | `infrastructure::persistence` (models, migrations, repository) |
| Network drives | `engine::network_drive` and [network-drive.md](network-drive.md) |
| A page of the window | `screens/`, with its messages handled in `app/handlers/` |
| Android calls into Syncative | `inbound::android`; calls from Syncative to Android: `infrastructure::android` and `Bridge.java` |
