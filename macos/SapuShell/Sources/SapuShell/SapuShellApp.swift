import AppKit
import SapuFFI
import SwiftUI

@main
struct SapuShellApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        WindowGroup("Sapu") {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 860, minHeight: 540)
                .onAppear {
                    // Swift apps started from the command line don't come to the front on their own.
                    NSApplication.shared.setActivationPolicy(.regular)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
        }
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Open Folder…") { model.chooseFolder() }
                    .keyboardShortcut("o")
            }
            CommandMenu("Scan") {
                Button("Rescan") { model.scan() }
                    .keyboardShortcut("r")
                    .disabled(model.scanning || model.working)
                Button("Stop Scan") { model.cancel() }
                    .keyboardShortcut(".")
                    .disabled(!model.scanning)
                Divider()
                Toggle("Use Sapu AI", isOn: $model.useAI)
            }
        }
    }
}

enum Pane: String, CaseIterable, Identifiable {
    case browse = "Browse"
    case recommendations = "Recommendations"
    case largest = "Largest files"
    case safety = "Safety"
    case logs = "Logs"

    var id: String { rawValue }

    var symbol: String {
        switch self {
        case .browse: return "folder"
        case .recommendations: return "sparkles"
        case .largest: return "doc.text.magnifyingglass"
        case .safety: return "checkmark.shield"
        case .logs: return "list.bullet.rectangle"
        }
    }

    /// Safety and Logs work without a scan.
    var needsScan: Bool { self != .safety && self != .logs }
}

struct ContentView: View {
    @EnvironmentObject var model: AppModel
    @State private var pane: Pane? = .browse

    var body: some View {
        NavigationSplitView {
            List(Pane.allCases, selection: $pane) { item in
                Label(item.rawValue, systemImage: item.symbol).tag(item)
            }
            .navigationSplitViewColumnWidth(min: 180, ideal: 200)
        } detail: {
            let current = pane ?? .browse
            VStack(spacing: 0) {
                Group {
                    if current.needsScan && model.scanning {
                        ScanningView()
                    } else if current.needsScan && model.result == nil {
                        StartView()
                    } else {
                        switch current {
                        case .browse: BrowseView()
                        case .recommendations: RecommendationsView()
                        case .largest: LargestFilesView()
                        case .safety: SafetyView()
                        case .logs: LogsView()
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            // Content scrolls under the floating glass banner and status bar.
            .safeAreaInset(edge: .top, spacing: 0) {
                if !model.fullDiskAccess { FullDiskAccessNotice() }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) { StatusBar() }
            .softScrollEdges()
            .task { model.scanFromLaunchArguments() }
            .toolbar {
                ToolbarItemGroup {
                    Button { model.chooseFolder() } label: { Label("Open Folder", systemImage: "folder.badge.plus") }
                        .help("Choose a folder to scan")
                        .disabled(model.scanning || model.working)
                    Button { model.scan() } label: { Label("Rescan", systemImage: "arrow.clockwise") }
                        .help("Scan again")
                        .disabled(model.scanning || model.working)
                }
            }
        }
    }
}

struct FullDiskAccessNotice: View {
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "lock.fill").foregroundStyle(.orange)
            Text("Some folders can't be read without Full Disk Access, so scans and Undo may miss things.")
                .lineLimit(2)
            Spacer()
            Button("Open Settings") { Finder.openFullDiskAccessSettings() }
                .glassButton(tint: .orange)
        }
        .font(.callout)
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
        .glassPanel(cornerRadius: 18, GlassStyle(tint: .orange.opacity(0.25)))
        .padding(.horizontal, 10)
        .padding(.top, 8)
    }
}

struct StartView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        VStack(spacing: 14) {
            Text("See what's filling your disk")
                .font(.system(.title, design: .serif))
            Text(model.rootPath)
                .font(.callout.monospaced())
                .foregroundStyle(.secondary)
            GlassGroup {
                HStack {
                    Button("Scan") { model.scan() }
                        .keyboardShortcut(.defaultAction)
                        .glassButton(prominent: true)
                    Button("Choose Folder…") { model.chooseFolder() }
                        .glassButton()
                }
            }
            .controlSize(.large)
            Text("Everything Sapu cleans goes to the Trash first, and you can undo it in Safety.")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
        }
        .padding()
    }
}

struct ScanningView: View {
    @EnvironmentObject var model: AppModel
    @Namespace private var glass

    var body: some View {
        GlassGroup(spacing: 14) {
            VStack(spacing: 14) {
                VStack(spacing: 10) {
                    ProgressView().controlSize(.large)
                    Text("Scanning \((model.rootPath as NSString).lastPathComponent)")
                        .font(.system(.title3, design: .serif))
                    if let p = model.progress {
                        Text(p.currentPath)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .frame(maxWidth: 460)
                    }
                }
                .padding(24)
                .frame(minWidth: 420)
                .glassPanel(cornerRadius: 26, GlassStyle(tint: .accentColor.opacity(0.12)))
                .glassID("card", in: glass)

                // Live counters, each its own pill; they morph in as numbers arrive.
                HStack(spacing: 8) {
                    if let p = model.progress {
                        Counter(value: Format.count(p.files), label: "files")
                            .glassID("files", in: glass)
                        Counter(value: Format.size(p.bytes), label: "found")
                            .glassID("bytes", in: glass)
                        if p.errors > 0 {
                            Counter(value: Format.count(p.errors), label: "unreadable", tint: .orange)
                                .glassID("errors", in: glass)
                        }
                    }
                    Button("Stop") { model.cancel() }
                        .glassButton(tint: .red)
                        .glassID("stop", in: glass)
                }
            }
        }
        .animation(.glassMorph, value: model.progress?.errors ?? 0 > 0)
        .animation(.glassMorph, value: model.progress == nil)
        .padding()
    }
}

/// A number with a word under it, in a small glass pill.
struct Counter: View {
    let value: String
    let label: String
    var tint: Color? = nil

    var body: some View {
        VStack(spacing: 1) {
            Text(value).font(.headline.monospacedDigit())
            Text(label).font(.caption2).foregroundStyle(.secondary)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 7)
        .glassCapsule(GlassStyle(tint: tint?.opacity(0.3)))
    }
}

/// Floating glass status pills. Each piece morphs in and out on its own:
/// the scan summary, a spinner while a file action runs, and the latest notice.
struct StatusBar: View {
    @EnvironmentObject var model: AppModel
    @Namespace private var glass

    var body: some View {
        GlassGroup(spacing: 8) {
            HStack(spacing: 8) {
                Group {
                    if let s = model.summary {
                        HStack(spacing: 6) {
                            Image(systemName: "internaldrive")
                            Text("\(Format.size(s.bytes)) in \(Format.count(s.files)) files, \(String(format: "%.1f", s.seconds)) s")
                            if s.cancelled { Text("(partial)") }
                            if s.errors > 0 {
                                Text("\(Format.count(s.errors)) unreadable").foregroundStyle(.orange)
                            }
                        }
                    } else {
                        Text("Sapu \(SapuFFI.version())")
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .glassCapsule()
                .glassID("summary", in: glass)

                if model.working {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Working…")
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .glassCapsule(GlassStyle(tint: .accentColor.opacity(0.2)))
                    .glassID("working", in: glass)
                }

                Spacer(minLength: 0)

                if let notice = model.notice, !model.working {
                    HStack(spacing: 8) {
                        Image(systemName: notice.kind == .error ? "exclamationmark.triangle.fill" : "checkmark.circle.fill")
                            .foregroundStyle(notice.kind == .error ? Color.red : Color.green)
                        Text(notice.text).lineLimit(2).foregroundStyle(.primary)
                        Button { model.notice = nil } label: { Image(systemName: "xmark") }
                            .buttonStyle(.borderless)
                            .help("Dismiss")
                    }
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .glassCapsule(GlassStyle(tint: (notice.kind == .error ? Color.red : Color.green).opacity(0.18),
                                             interactive: true))
                    .glassID("notice", in: glass)
                }
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .animation(.glassMorph, value: model.working)
        .animation(.glassMorph, value: model.notice)
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
        .task(id: model.notice) {
            // Good news fades away by itself; errors stay until dismissed.
            guard let notice = model.notice, notice.kind == .ok, !model.working else { return }
            try? await Task.sleep(nanoseconds: 6_000_000_000)
            if model.notice == notice { model.notice = nil }
        }
    }
}
