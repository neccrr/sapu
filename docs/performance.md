# Performance

Measured by `scripts/perf-macos.sh` on the GitHub macOS runner. The
[Performance workflow](../.github/workflows/perf.yml) runs it on every pull request that touches the
engine or the macOS apps; each run's full report is in its job summary and in the `perf-report`
artifact. You can run the same script on your own Mac:

```sh
bash scripts/bundle-macos.sh && bash macos/build-shell.sh
bash scripts/perf-macos.sh            # writes perf-report.md
```

## Setup

- Machine: GitHub's macOS runner, Apple M1 (virtual), 3 cores, 7 GB RAM, macOS 26.7.
- Test tree: 120,000 generated files, 629 MB (dependency folders, documents and a few big files).
  It is generated the same way every run.
- Each app is launched with the tree as its folder and its own empty HOME, so there's no cached
  scan and nothing on the machine is read or changed.
- "Launch to scan done" is the time from starting the process to the app's log saying
  `scan done`. In both apps that line comes after judging, recommendations and training Sapu AI.

## Results (two runs, 24 Nov 2026)

| | Run A | Run B |
|---|---|---|
| Engine alone, `sapu scan` (best of 3) | 0.76 s | 1.36 s |
| Engine through sapu-ffi, as SapuShell calls it (best of 3) | 0.52 s | 0.81 s |
| Sapu.app (egui), launch to scan done | 3.95 s | 5.33 s |
| SapuShell, Liquid Glass, launch to scan done | 2.01 s * | 2.72 s |
| SapuShell, no glass (`SAPU_NO_GLASS=1`), launch to scan done | 1.84 s * | 1.67 s |
| Sapu.app memory after the scan | 125 MB | 126 MB |
| SapuShell memory after the scan (glass on or off) | 82 MB | 83 MB |
| Idle CPU after the scan, either app | 0.0 to 0.5 % | 0.3 to 0.5 % |

\* In run A SapuShell logged `scan done` before training Sapu AI, so those two times leave out a
step that Sapu.app includes. Run B times the same work in both apps.

Sizes: Sapu.app 12.0 MB, SapuShell.app 3.5 MB, the `sapu` command 2.6 MB.

## What this means

- **Scanning is the same engine.** Both apps call sapu-core, which walks 120,000 files in about
  a second on this runner. The runner is noisy: the same command varied by up to 2x between runs.
- **SapuShell gets to a finished scan sooner and uses about a third less memory** (about 83 MB
  against 126 MB). Our guess, not profiled: Sapu.app's extra memory is its own GPU renderer and
  font atlas, while the shell draws with AppKit and SwiftUI, which macOS shares between apps. We haven't
  profiled where Sapu.app's extra two to three seconds go.
- **Both apps sit idle at under 1 % CPU** once the scan is over.
- **Liquid Glass cost: not measurable here.** With glass on and off, memory was the same and idle
  CPU was within noise. The launch times differed by 0.2 s in one run and 1 s in the other, in
  the noise of this runner. A CI runner is a virtual Mac with no real display and a software GPU,
  so it can't show the cost of drawing glass on a real screen.

## Estimated, not measured

These are expectations, not numbers from the runner:

- On a real Apple Silicon Mac (for example an M1 or newer MacBook), scans should be faster than on
  the 3-core virtual runner, because the scan uses all cores and the disk is local.
- Glass is drawn by the GPU's window compositor. On real hardware it should add a little GPU work
  while glass pieces animate or the window scrolls under them, and close to nothing when the window
  is still. Turn on Reduce Transparency in System Settings, or start SapuShell with
  `SAPU_NO_GLASS=1`, to compare on your own Mac.
- Memory numbers should be close to what you see on your Mac, since they don't depend much on the
  hardware.
