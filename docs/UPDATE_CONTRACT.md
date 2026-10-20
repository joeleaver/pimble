# Updates: a home-folder install that keeps itself current

Status: built 2026-10-20 on branch `updates` (off `brand`). For early testing; Joe
expects to revisit it if Pimble's `.deb` is ever hosted in a package repository.

## Decisions (Joe, 2026-10-20)

1. **The home-folder route.** The desktop app installs into the person's own
   folders and replaces itself there. No root, no password, no package manager.
2. **"Restart to update".** The app downloads and checks an update in the
   background and then says so in the status bar. It never restarts on its own.
3. **Signed packages.** Every package a release attaches carries a detached
   Ed25519 signature, made in CI with a key only GitHub's secrets hold. The app
   carries the public key and installs nothing that does not verify.

## The install

`pimble --install`, run from an unpacked release, copies `pimble` (and
`pimble-mcp` beside it) into the install folder and writes the marker
`pimble-install.json` there. Only a binary that runs from a folder holding the
marker updates itself, so a development build, a `cargo run` or a `.deb` never
does.

| | Linux | Windows |
| --- | --- | --- |
| Install folder | `~/.local/share/pimble/app/` | `%LOCALAPPDATA%\Programs\Pimble\` |
| How | `pimble --install` from the unpacked tar.gz | `pimble-<v>-windows-x86_64-setup.exe` (per user, no administrator), or `pimble.exe --install` from the zip |
| On the PATH | `~/.local/bin/pimble`, `~/.local/bin/pimble-mcp` (symlinks) | not yet |
| Menu entry | `~/.local/share/applications/pimble.desktop` | Start menu `Pimble` (the installer), optional desktop shortcut |
| Uninstall | delete the folder, the links and the menu entry | Apps & features (the installer's uninstaller; notes and stores are elsewhere and stay) |
| Icon | `~/.local/share/icons/hicolor/{32..256}/apps/pimble.png`, `scalable/apps/pimble.svg` | the installer's and the Start menu's, from `assets/brand/pimble.ico` |

The menu entry's `Exec` names the install folder's binary, and its name matches
the binary's file name, which is the Wayland `app_id` the window carries.

## The Windows installer

`tools/windows/pimble.iss` (Inno Setup 6, built and checked in the Release
workflow's Windows job). It installs `pimble.exe`, `pimble-mcp.exe` and the marker
into the updater's folder, so an installed copy keeps itself current like a Linux
one; the next Setup is an upgrade of the same app (a fixed `AppId`). Its
uninstaller also removes what the updater leaves there (`update\`, `*.old`).

## The feed

`GET https://pimble.app/api/v1/releases` (the accounts service's cached view of
the latest GitHub release, the same one the download page reads). An asset gains
`signature_url`: its `<name>.sig` from the same release. `.sig` files are no longer
listed as assets of their own. The app falls back to `<url>.sig` when a feed from
before that change does not name one. `PIMBLE_UPDATE_FEED` points the app at
another feed (tests).

## The check

In a managed install the app checks 30 seconds after it starts and then every 24
hours. A newer version (semver, the tag without its `v`) for this OS (`linux`
asset ending `.tar.gz`, `windows` asset ending `.zip`) is downloaded, its signature
verified, and unpacked with the system `tar` (Windows 10 and later ship one that
reads zip) into `<install>/update/`, with `update/ready.json` naming the version.
Nothing outside `update/` changes. A failed check logs a line and tries again at
the next one; it says nothing on screen. A staged update found at start is offered
at once.

## Restart to update

The status bar shows "Restart to update to <version>". A click moves the staged
`pimble` and `pimble-mcp` over the installed ones (a rename on Linux, where the
running process keeps its open file; on Windows the running file is renamed
aside to `*.old` first and removed at the next start), rewrites the marker,
starts the new binary with the same arguments and `PIMBLE_WAIT_FOR_PID` naming
this process, and closes the window. The new process waits up to 20 seconds for
the old one to exit before it starts its embedded server, so the two never
contend for the port.

## Signing

CI's release job signs every package with `openssl pkeyutl -sign -rawin` (pure
Ed25519 over the file) using the PEM in the `PIMBLE_UPDATE_SIGNING_KEY` secret, and
attaches `<package>.sig` (64 raw bytes) beside it. A release without the secret
fails rather than ship unsigned. The public key is
`crates/pimble-app/update-key.pub` (the raw 32 bytes, base64), compiled in.
`tools/update-key.sh` makes a key pair, stores the private half as the secret,
and writes the public half there. Run it once; the private key never needs to
leave GitHub.

## Installing each release on Joe's machine

`tools/install-release.sh [vX.Y.Z]` downloads the release's Linux package and
signature with `gh`, verifies the signature against the compiled-in public key,
unpacks it into a temporary folder, and runs `pimble --install` from there. What
is tested is what shipped.

## Not done

- Windows: nothing on the PATH. The installer is checked on CI's Windows runner
  (install, the files and marker, the Start-menu entry, uninstall), but the update
  itself has not run on Windows yet. Apps & features keeps showing the version
  the installer put there after the app updates itself. Setup is not
  Authenticode-signed, so SmartScreen warns on first run until a code-signing
  certificate is bought and used.
- No "Check for Updates" menu item, no release notes shown, no channel choice.
- Old versions are not kept for a rollback.
