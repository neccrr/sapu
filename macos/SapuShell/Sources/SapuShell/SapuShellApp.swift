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
                if !model.fullDiskAccess {
                    FullDiskAccessNotice()
                }
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
            // Content scrolls under the floating glass status bar.
            .safeAreaInset(edge: .bottom, spacing: 0) { StatusBar() }
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
        HStack(spacing: 8) {
            Image(systemName: "lock")
            Text("Some folders can't be read without Full Disk Access, so scans and Undo may miss things.")
                .lineLimit(2)
            Spacer()
            Button("Open Settings") { Finder.openFullDiskAccessSettings() }
                .glassButton()
        }
        .font(.callout)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .glassPanel()
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

    var body: some View {
        VStack(spacing: 12) {
            ProgressView()
            if let p = model.progress {
                Text("\(Format.count(p.files)) files, \(Format.size(p.bytes))")
                    .font(.headline.monospacedDigit())
                Text(p.currentPath)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 520)
            } else {
                Text("Starting…")
            }
            Button("Stop") { model.cancel() }
                .glassButton()
        }
        .padding(24)
        .frame(minWidth: 360)
        .glassPanel(cornerRadius: 22)
        .padding()
    }
}

struct StatusBar: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        HStack(spacing: 10) {
            if model.working {
                ProgressView().controlSize(.small)
            }
            if let notice = model.notice {
                Text(notice.text)
                    .foregroundStyle(notice.kind == .error ? Color.red : Color.secondary)
                    .lineLimit(2)
                Button("Dismiss") { model.notice = nil }
                    .buttonStyle(.borderless)
                    .font(.caption)
            } else if let s = model.summary {
                Text("\(Format.size(s.bytes)) in \(Format.count(s.files)) files, scanned in \(String(format: "%.1f", s.seconds)) s")
                if s.cancelled {
                    Text("Stopped early, so totals are partial.")
                }
                if s.errors > 0 {
                    Text("\(Format.count(s.errors)) items couldn't be read.")
                        .foregroundStyle(.orange)
                }
            } else {
                Text("Sapu \(SapuFFI.version())")
            }
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .glassPanel(cornerRadius: 12)
        .padding(.horizontal, 10)
        .padding(.bottom, 8)
    }
}
