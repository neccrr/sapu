# Changelog

## Unreleased
- A macOS guide, [`macos/README.md`](macos/README.md): install and first launch, Full Disk
  Access, data locations, iCloud, building Sapu.app (including universal builds), test
  options and how releases are made.
- `Sapu.command` and `scripts/bundle-macos.sh` are now executable after a fresh clone.
- Optional Developer ID signing and notarization. The release workflow signs, notarizes
  and staples Sapu.app (and signs and notarizes `sapu`) once the signing secrets are set,
  so a signed release opens with a normal double-click. `SIGN_IDENTITY=…
  ./scripts/bundle-macos.sh` signs a local build. Without secrets nothing changes.
- **SwiftUI shell (preview)**, `SapuShell.app`: a native window over `sapu-core` through the
  new `sapu-ffi` crate (UniFFI). It covers Browse, Recommendations, Largest files, Safety
  (protected list, History, Undo, data) and Logs. It cleans with exactly the same rules as
  Sapu.app:
  - Move to Trash for recommendations and single items;
  - Remove Download, Download Now, and Delete from iCloud with per-item consent;
  - Protect and Mark as Keep.
  Releases include it (universal, and signed and notarized when the secrets are set). Build
  it yourself with `./macos/build-shell.sh`. CI builds it on every pull request.
- The release workflow no longer fails with "Permission denied" on the bundle step.
- CI uploads Sapu.app and SapuShell.app (Apple Silicon) as downloadable artifacts on every run.

## 0.10.0
- **Instant start:** the last scan is cached (`~/Library/Caches/sapu/scans/`) and shown
  immediately while a fresh scan runs in the background. Nothing can be cleaned until the
  fresh scan is done. ⌘R now refreshes in the background too.
- **Clear data layout** following macOS conventions: `Application Support/sapu/ai/` and
  `safety/`, `Caches/sapu/`, `Logs/sapu/` (rotated at 10 MB). Old files move automatically.
- "Data on this Mac" in the Safety tab, and `sapu data [--clear-cache]`.
- Bundle ID is now `io.github.neccrr.sapu`.
- **UI cleanup:**
  - one type scale, with no text below 11.5 pt;
  - the same Open Folder / Rescan / Logs buttons on every tab;
  - Browse has its own path row;
  - click the column headers to sort (this replaces the Sort menu);
  - "Keep" badges no longer clutter the file list;
  - recommendation cards drop the repeated verdict badge;
  - sizes and buttons line up in Recommendations and iCloud;
  - Delete… in iCloud is quieter;
  - a tidier Safety tab, with a table for "Data on this Mac" and clearer Undo;
  - status bar hints change with the tab;
  - emoji icons that rendered as odd glyphs are gone.

## 0.9.0
- **iCloud delete with consent.** Items in iCloud can now be deleted, but only after an
  explicit "I understand" in a dedicated dialog (or typing `delete` in the terminal).
  The core refuses iCloud deletes without consent, never deletes whole iCloud containers,
  and automatic cleaning never touches iCloud.
- GitHub-ready: README with screenshots, MIT license, CI (fmt, clippy, tests on macOS),
  release workflow that builds a universal Sapu.app, issue templates.

## 0.8.1
- Fix: window opened blank on macOS (the blur layer was drawn over the content).
  The blur is now placed behind the content and is opt-in (`SAPU_GLASS=1`).
- Fewer, better AI guesses: confidence ≥ 85%, at most 40 per list.

## 0.8.0
- Logging everywhere, and a 📜 Logs window (⌘L) with filters, search, copy.
- Crashes are logged with file and line.

## 0.7.0
- Claude-inspired look: warm palette, serif headings, glass cards, smooth motion,
  follows Light/Dark.
- iCloud tab: downloaded vs cloud-only, "Remove download" (keeps it in iCloud), "Download".

## 0.6.0
- Failsafes: write-ahead journal + Undo, re-check before deleting, your protected list,
  warnings for folders containing git repos or Photos libraries, background cleaning
  with Stop, extra confirmation for risky batches.

## 0.5.0
- Sapu AI: an on-device classifier (logistic regression over path features) trained
  from rules, a labelled seed set and your feedback.

## 0.4.0
- Judgement system (Safe / Likely / Review / Keep) and cleanup recommendations.

## 0.3.0
- Window app (egui) with a list and a squarified treemap.

## 0.2.0
- Parallel tree scanner, terminal explorer, structured logging.

## 0.1.0
- First skeleton: scanner, cleanup rules, CLI.
