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
                .frame(minWidth: 820, minHeight: 520)
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
                    .disabled(model.scanning)
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
    case about = "Data and about"

    var id: String { rawValue }

    var symbol: String {
        switch self {
        case .browse: return "folder"
        case .recommendations: return "sparkles"
        case .largest: return "doc.text.magnifyingglass"
        case .about: return "info.circle"
        }
    }
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
            VStack(spacing: 0) {
                if let error = model.errorMessage {
                    Banner(text: error, tint: .red) { model.errorMessage = nil }
                }
                PreviewNotice()
                Group {
                    if model.scanning {
                        ScanningView()
                    } else if model.result == nil {
                        StartView()
                    } else {
                        switch pane ?? .browse {
                        case .browse: BrowseView()
                        case .recommendations: RecommendationsView()
                        case .largest: LargestFilesView()
                        case .about: AboutView()
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                StatusBar()
            }
            .toolbar {
                ToolbarItemGroup {
                    Button { model.chooseFolder() } label: { Label("Open Folder", systemImage: "folder.badge.plus") }
                        .help("Choose a folder to scan")
                    Button { model.scan() } label: { Label("Rescan", systemImage: "arrow.clockwise") }
                        .help("Scan again")
                        .disabled(model.scanning)
                }
            }
        }
    }
}

/// Says plainly that this window can look but not clean.
struct PreviewNotice: View {
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "eye")
            Text("Preview: this window only looks. To clean, use Sapu, where everything goes to the Trash and can be undone.")
                .lineLimit(2)
            Spacer()
            Button("Open Sapu") { Finder.openSapu() }
        }
        .font(.callout)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.quaternary.opacity(0.5))
    }
}

struct Banner: View {
    let text: String
    let tint: Color
    let dismiss: () -> Void

    var body: some View {
        HStack {
            Image(systemName: "exclamationmark.triangle")
            Text(text)
            Spacer()
            Button("Dismiss", action: dismiss)
        }
        .font(.callout)
        .foregroundStyle(tint)
        .padding(10)
        .background(tint.opacity(0.08))
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
            HStack {
                Button("Scan") { model.scan() }
                    .keyboardShortcut(.defaultAction)
                Button("Choose Folder…") { model.chooseFolder() }
            }
            Text("For a complete scan, add this app under System Settings, Privacy & Security, Full Disk Access.")
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
        }
        .padding()
    }
}

struct StatusBar: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        HStack {
            if let s = model.summary {
                Text("\(Format.size(s.bytes)) in \(Format.count(s.files)) files, scanned in \(String(format: "%.1f", s.seconds)) s")
                if s.cancelled {
                    Text("Stopped early, so totals are partial.")
                }
                if s.errors > 0 {
                    Text("\(Format.count(s.errors)) items couldn't be read. Full Disk Access gives a complete scan.")
                        .foregroundStyle(.orange)
                }
            } else {
                Text("Sapu \(version())")
            }
            Spacer()
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(.bar)
    }
}
