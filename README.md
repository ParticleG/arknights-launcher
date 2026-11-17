# Arknights Launcher

An unofficial Linux desktop launcher for the original Arknights CN Windows
client, built with Tauri 2, React, TypeScript, and Rust. This is not an Endfield
launcher, a native Linux port of the game, or an official Hypergryph product.

The project follows the separation used by
[AugustLigh/LLauncher](https://github.com/AugustLigh/LLauncher): a native UI,
release/download management, and a selected Proton runtime. It implements the
original Arknights CN distribution protocol instead of renaming Endfield's
configuration or copying its VFS-specific logic.

## Build and run

Install Bun, Rust, and the
[Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux),
including GTK 3 and WebKitGTK 4.1. On Arch Linux, install a DWProton runtime if
one is not already available:

```sh
paru -S dwproton-bin
```

From this project directory:

```sh
bun install --frozen-lockfile
bun run tauri dev
```

Build a release executable and Debian bundle:

```sh
bun run tauri build --bundles deb
```

The executable is normally written to
`src-tauri/target/release/arknights-launcher`; the Debian bundle is under
`src-tauri/target/release/bundle/deb/`. Building the app does not install a game,
create an account, or publish the project anywhere.

## Use an existing installation

The default paths reuse the Arknights installation already managed through the
official launcher:

```text
Proton compatdata:
~/.local/share/arknights-proton/compatdata

Game folder:
~/.local/share/arknights-proton/compatdata/pfx/drive_c/Program Files/Hypergryph Launcher/games/Arknights
```

Select another game folder, compatdata directory, or Proton executable in
**Paths & runtime**, then choose **Save paths**. The compatdata path is the
parent of `pfx`, not `pfx` itself. Path changes and file mutation are disabled
while a game session is running; close the game and refresh before changing
its installation.

An existing `Arknights.exe` is detected without downloading the game again.
The verified version remains **Unknown** until all required files have been
checked successfully. The launcher does not guess that an existing client is
the latest release, and verification is not an artificial prerequisite for
launching a locally installed game.

## File maintenance

- **Check updates** obtains the latest official release and its ZIP directory.
- **Verify files** reads local files and compares their size and CRC32 with the
  official ZIP entries. It does not alter game files and is available while a
  game is running.
- **Install game / Repair / update** downloads only missing or changed entries,
  validates their ZIP CRC32, and replaces each completed file. It does not fetch
  a full archive just to repair a small file.
- **Cancel** stops the active operation. Completed files are retained; unfinished
  file output must not replace an existing destination. A later repair checks
  the remaining differences again.

Repair restores files from the official distribution: modifications to files
that are part of that distribution are overwritten. Unrelated extra files are
preserved. Keep personal backups of mods or custom replacements yourself.
The launcher does not mutate game files while Arknights is using either the
selected prefix or the same game directory through another prefix. Directory
matching uses filesystem identity, including symlink aliases, rather than
comparing Wine environment variables alone.

The installer accesses the official split ZIP through HTTPS byte ranges. The
volumes form one concatenated seekable archive. Its central directory supplies
file paths, sizes, compressed locations, and CRC values, allowing per-file
verification and selective extraction. It does not need to parse or decrypt
Hypergryph's opaque `config.ini` or `game_files` data; those original archive
entries are handled like other publisher-owned files.

CRC32 detects accidental corruption; it is not a cryptographic authenticity
guarantee. Release metadata and archive data are obtained directly from the
publisher over HTTPS. The app does not mirror or redistribute them.

## Launching

**Launch** starts `Arknights.exe` through the selected DWProton installation
with the configured prefix and the CN launch arguments. Steam itself is not
required. Login remains inside the original game; this app never collects
account credentials or implements an alternative login flow.

If the configured game is already running, another copy is not started. The
launcher leaves that session untouched. Use **Check updates** to refresh the
status of a session started outside this app. API failures are shown in the UI
but do not hide local settings or prevent an otherwise installed client from
launching offline.

## User data

Settings use the XDG configuration directory, under `arknights-launcher`.
`XDG_CONFIG_HOME` and `XDG_DATA_HOME` follow the standard user-directory
conventions. The app keeps the verified version tied to the selected game
folder and clears that identification when the selected installation changes.

The game and Wine prefix remain user-managed data. Removing the launcher
application must not remove the prefix, downloaded game, saves, or account
configuration. Never run this app as root, change a running prefix to a different
Wine version, or delete its prefix merely to reinstall the launcher.

## Support and licensing

The application source is MIT-licensed. The upstream LLauncher MIT notice is
preserved in `LICENSES/LLauncher-MIT.txt` and included in desktop bundles. The
reference project and Hypergryph do not endorse this application.

Game files, names, trademarks, services, and their agreements belong to their
respective owners and are not covered by the application's MIT license. Review
the current [Hypergryph launcher agreement](https://user.hypergryph.com/protocol/plain/launcher/service)
and game/service terms before use. The official protocol is not a supported
third-party API and can change without notice. A third-party native launcher
and DWProton do not guarantee compatibility with anti-cheat or future game
updates. This project does not modify anti-cheat or bypass its checks.

## Local verification

The following checks were exercised locally with Arknights 79.0.0:

- `cargo test --release --lib --manifest-path src-tauri/Cargo.toml`: four
  deterministic tests passed, covering split offsets/seeks, unsafe archive
  paths/symlinks, CRC failure preserving an existing destination, and shared-game
  directory detection across different prefixes and working directories. The
  real-process regression also covers directory aliases and escaped path names.
- `bun run tauri build --bundles deb`: frontend type checking, production
  compilation, the native executable, and the Debian bundle succeeded.
- With the original game left running, a second real desktop instance selected
  different compatdata for the same installation. It still detected the active
  game and disabled repair, path changes, and duplicate launch.
- The real desktop interface loaded the official release metadata and
  recognized the existing installation and its running game session.
- Read-only verification checked 14,021 required files (24.3 GiB), reported
  zero missing/changed files, and recorded verified version 79.0.0.
- An actual UI verification operation was cancelled; the UI left its busy
  state and subsequently completed a repair.
- In a private Btrfs reflink copy, a missing 16-byte `game_files` entry was
  restored byte-for-byte; the other 14,020 archive files were unchanged and an
  unrelated sentinel survived.
- A separate four-file repair restored stored and deflated entries, including
  a compressed file spanning a 1 GiB ZIP-volume boundary. All four outputs
  matched the original files. These repair checks targeted only the private
  copy, which was removed afterwards.
- After the previous game session had exited, the native UI launched the real
  game through the existing DWProton 11.0-12 installation. The game reached a
  rendered START screen. Unity 2021.3.39f1 initialized Direct3D 11 on the AMD
  Radeon RX 9070 XT.

The runtime emitted missing legacy GStreamer dependency warnings, including
`libnettle.so.8`, `libvpx.so.9`, `libtheoradec.so.1`, and `libjpeg.so.62`.
They did not prevent the observed START screen, but this does not establish
video/cutscene playback, account login, battle functionality, or compatibility
with the currently published `dwproton-bin` release. Those runtime capabilities
are not claimed as verified.
