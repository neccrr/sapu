# Sapu on macOS

Everything Mac-specific in one place: installing, first launch, permissions, building
`Sapu.app` yourself, where Sapu keeps its files, and how releases are made. For what Sapu
does and how to use it, see the [main README](../README.md).

Sapu needs **macOS 11 (Big Sur) or later** and runs natively on Apple Silicon and Intel.

## Install the app

1. Download `Sapu-<version>-macos-universal.zip` from
   [Releases](https://github.com/neccrr/sapu/releases).
2. Unzip it and drag **Sapu.app** into **Applications**.
3. Open it once with **right-click → Open → Open** (see below).

### First launch: "Sapu can't be opened"

A release that's signed with an Apple Developer ID and notarized opens with a normal
double-click. Until releases are signed (see [Signing and notarization](#signing-and-notarization)),
Gatekeeper blocks a plain double-click the first time. Any of these gets you past it, and
macOS remembers your choice:

- In Finder, **right-click Sapu.app → Open**, then **Open** again in the dialog.
- Or, after a blocked double-click, go to **System Settings → Privacy & Security** and click
  **Open Anyway** next to the message about Sapu.
- Or remove the download quarantine in Terminal:

  ```sh
  xattr -dr com.apple.quarantine /Applications/Sapu.app
  ```

### Full Disk Access

macOS hides some folders (Mail, Messages, Safari, the Trash and others) from apps until you
allow it. Without access Sapu still works, but those folders are skipped in a scan and Undo
can't look inside the Trash to put things back.

1. Open **System Settings → Privacy & Security → Full Disk Access**.
2. Click **+**, choose **Sapu** in Applications (or turn its switch on if it's listed).
3. Quit and reopen Sapu.

If you run Sapu from Terminal (`cargo run`, `Sapu.command` or the `sapu` command), give
Full Disk Access to **Terminal** (or iTerm, Ghostty…) instead, since macOS checks the app
that started it.

You can tell whether it worked: the log says `Full Disk Access: yes` or `no` at startup
(**Logs**, ⌘L), and a scan that couldn't read some folders says so in the status bar.

The app is ad-hoc signed when it's bundled, which is what lets macOS remember this
permission for it. If you rebuild Sapu.app yourself, macOS may treat it as a new app and ask
again; remove the old entry with **−** and add the new one.

### Updating and uninstalling

To update, replace Sapu.app with the new one. Your protected list, history (Undo) and Sapu AI
live outside the app, so they carry over.

To remove Sapu completely, delete Sapu.app and these three folders:

```text
~/Library/Application Support/sapu
~/Library/Caches/sapu
~/Library/Logs/sapu
```

## Where Sapu keeps its files

Everything follows the usual macOS places and stays on this Mac. Nothing is stored inside
Sapu.app and nothing is sent anywhere.

```text
~/Library/Application Support/sapu/   things you'd miss: keep
  ai/model.bin            Sapu AI (retrained on every scan)
  ai/feedback.tsv         your Keep / trash choices, which teach the AI
  safety/protected.txt    your "never touch" list
  safety/journal.jsonl    everything moved to the Trash (powers Undo)
~/Library/Caches/sapu/                safe to delete anytime
  scans/<id>.bin          last scan of each folder, for an instant start
~/Library/Logs/sapu/                  sapu-gui.log, sapu.log (rotated at 10 MB)
```

**Safety → Data on this Mac** shows the sizes and opens each folder; `sapu data` prints the
same, and `sapu data --clear-cache` empties the cache. The logs open nicely in
**Console.app**.

Items Sapu cleans go to your normal Trash (`~/.Trash`, or the `.Trashes` folder of another
volume). Sapu never empties the Trash, so Undo works until you do.

## iCloud Drive

Sapu reads iCloud Drive and every app's iCloud folder from `~/Library/Mobile Documents`. It
tells downloaded files from cloud-only ones by comparing a file's size with the space it
really takes on disk, so no network access is needed.

- **Remove download** frees the space on this Mac and keeps the file in iCloud. It uses
  Apple's own `brctl evict`, falling back to `fileproviderctl evict`. It's the same as
  Finder's right-click → Remove Download.
- **Download** fetches it again (`brctl download`, or `fileproviderctl materialize`).
- **Delete…** removes the item from iCloud on all your devices, so it asks for an explicit
  "I understand" for each item. Whole app containers in iCloud can't be deleted, and
  automatic cleaning never touches iCloud.

If macOS refuses an action, Sapu says so and points you to the Finder menu item that does
the same thing.

## Build Sapu.app yourself

You need the Xcode Command Line Tools (`xcode-select --install`) and
[Rust](https://rustup.rs) 1.95 or newer.

```sh
git clone https://github.com/neccrr/sapu
cd sapu
./scripts/bundle-macos.sh
```

That builds a release binary and puts **`dist/Sapu.app`** together:

- `Contents/MacOS/Sapu`: the `sapu-gui` binary,
- `Contents/Info.plist`: bundle ID `io.github.neccrr.sapu`, version from `Cargo.toml`,
  minimum macOS 11, Retina on,
- an ad-hoc code signature (`codesign --sign -`), so it runs locally and keeps its
  Full Disk Access. Set `SIGN_IDENTITY` to sign with a Developer ID instead (see
  [Signing and notarization](#signing-and-notarization)).

Drag it into Applications and grant Full Disk Access as above. A self-built app has no
quarantine flag, so it opens without the right-click step.

Set `SKIP_BUILD=1` to bundle a `target/release/sapu-gui` you've already built (the release
workflow uses this for the universal binary):

```sh
SKIP_BUILD=1 ./scripts/bundle-macos.sh
```

### A universal (Apple Silicon + Intel) app

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
for t in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release -p sapu-gui --target "$t"
done
mkdir -p target/release
lipo -create -output target/release/sapu-gui \
  target/aarch64-apple-darwin/release/sapu-gui \
  target/x86_64-apple-darwin/release/sapu-gui
SKIP_BUILD=1 ./scripts/bundle-macos.sh
```

### Just run it

- `cargo run --release` builds and opens the window app (it's the workspace's default).
- Double-click **`Sapu.command`** in Finder to do the same without typing. The first build
  takes a few minutes; later launches are quick.
- `cargo install --path crates/sapu-cli` installs the `sapu` terminal command.

## Options for testing

Set these in the environment before starting Sapu, for example
`SAPU_THEME=dark cargo run --release`. For the installed app, run its binary directly,
because `open` doesn't pass your shell's variables on:

```sh
SAPU_GLASS=1 /Applications/Sapu.app/Contents/MacOS/Sapu
```

| Variable | Effect |
|---|---|
| `SAPU_THEME=light\|dark` | Force a theme (default: follow macOS) |
| `SAPU_GLASS=1` | Experimental: translucent window with the macOS blur behind the content |
| `SAPU_LOG=debug` | More detail in the log file |
| `SAPU_BRCTL=<path>` | Use another program instead of `brctl` for iCloud actions (for testing) |

## Releases

Pushing a tag like `v0.10.0` runs [`.github/workflows/release.yml`](../.github/workflows/release.yml)
on a macOS runner. It:

1. builds `sapu-gui` and `sapu` for `aarch64-apple-darwin` and `x86_64-apple-darwin`,
2. joins each pair into a universal binary with `lipo`,
3. bundles Sapu.app with `SKIP_BUILD=1 ./scripts/bundle-macos.sh`,
4. attaches `Sapu-vX.Y.Z-macos-universal.zip` (made with `ditto`, which keeps the bundle
   intact) and `sapu-cli-vX.Y.Z-macos-universal.tar.gz` to a GitHub release with generated
   notes.

When the signing secrets below are set, the workflow also signs Sapu.app and `sapu` with
your Developer ID and the hardened runtime, notarizes both with Apple, and staples the ticket
to Sapu.app. Without them it ad-hoc signs, as before.

Before tagging, bump `version` in the root `Cargo.toml` and add a `CHANGELOG.md` entry.

### Signing and notarization

You need a paid [Apple Developer](https://developer.apple.com/programs/) membership. Then:

1. In Xcode (**Settings → Accounts → Manage Certificates → + → Developer ID Application**),
   or on the Apple Developer site, create a **Developer ID Application** certificate.
2. In **Keychain Access**, export it with its private key as a `.p12` file, with a password.
3. At [account.apple.com](https://account.apple.com), under **Sign-In and Security → App-Specific
   Passwords**, make a password for notarization.
4. In the repository on GitHub, go to **Settings → Secrets and variables → Actions** and add:

| Secret | Value |
|---|---|
| `MACOS_CERT_P12` | the `.p12`, base64: `base64 -i cert.p12 \| pbcopy` |
| `MACOS_CERT_PASSWORD` | the `.p12` password |
| `APPLE_ID` | your Apple ID email |
| `APPLE_TEAM_ID` | your 10-character team ID (Apple Developer → Membership) |
| `APPLE_APP_PASSWORD` | the app-specific password |

The next tag builds a signed, notarized release. With only the two certificate secrets set,
it signs without notarizing.

To sign a local build with a Developer ID in your keychain:

```sh
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./scripts/bundle-macos.sh
```

`security find-identity -v -p codesigning` lists the identities you have.

## Not done yet

- **A native SwiftUI shell** around `sapu-core` (through UniFFI) may live in this folder one
  day. Today the window app is [`crates/sapu-gui`](../crates/sapu-gui) (egui).
