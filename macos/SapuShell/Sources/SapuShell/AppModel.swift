import AppKit
import Foundation
import SapuFFI

/// Holds the current scan and talks to sapu-core.
@MainActor
final class AppModel: ObservableObject {
    @Published var rootPath: String = homeFolder()
    @Published private(set) var result: ScanResult?
    @Published private(set) var summary: ScanSummary?
    @Published private(set) var suggestions: [Suggestion] = []
    @Published private(set) var scanning = false
    @Published private(set) var progress: ScanProgress?
    @Published var errorMessage: String?
    @Published var useAI = true

    private let scanner = Scanner()
    private var pollTask: Task<Void, Never>?

    func scan() {
        guard !scanning else { return }
        scanning = true
        errorMessage = nil
        progress = nil
        let path = rootPath
        let useAI = useAI
        let scanner = scanner

        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 200_000_000)
                guard let self, self.scanning else { return }
                self.progress = scanner.progress()
            }
        }

        Task.detached(priority: .userInitiated) {
            do {
                let result = try scanner.scan(path: path, useAi: useAI)
                let summary = result.summary()
                let suggestions = result.suggestions()
                await MainActor.run {
                    self.result = result
                    self.summary = summary
                    self.suggestions = suggestions
                    self.finish()
                }
            } catch {
                let message = AppModel.describe(error)
                await MainActor.run {
                    self.errorMessage = message
                    self.finish()
                }
            }
        }
    }

    func cancel() {
        scanner.cancel()
    }

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Scan"
        panel.directoryURL = URL(fileURLWithPath: rootPath)
        if panel.runModal() == .OK, let url = panel.url {
            rootPath = url.path
            scan()
        }
    }

    func children(of entry: Entry?) -> [Entry] {
        guard let result else { return [] }
        do {
            return try result.children(idxPath: entry?.idxPath ?? [])
        } catch {
            errorMessage = Self.describe(error)
            return []
        }
    }

    func largestFiles() -> [Entry] {
        result?.largestFiles(count: 100) ?? []
    }

    private func finish() {
        scanning = false
        pollTask?.cancel()
        pollTask = nil
    }

    nonisolated static func describe(_ error: Error) -> String {
        if let error = error as? SapuError {
            switch error {
            case let .Scan(message), let .NotFound(message):
                return message
            }
        }
        return error.localizedDescription
    }
}

/// Small helpers shared by the views.
enum Finder {
    static func reveal(_ path: String) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)])
    }

    static func open(_ path: String) {
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    static func copy(_ path: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(path, forType: .string)
    }

    /// Opens Sapu.app (the egui app), where cleaning happens.
    static func openSapu() {
        guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "io.github.neccrr.sapu") else {
            NSWorkspace.shared.open(URL(string: "https://github.com/neccrr/sapu/releases")!)
            return
        }
        NSWorkspace.shared.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration())
    }
}

enum Format {
    static func size(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
    }

    static func count(_ n: UInt64) -> String {
        NumberFormatter.localizedString(from: NSNumber(value: n), number: .decimal)
    }

    static func date(_ seconds: UInt64) -> String {
        guard seconds > 0 else { return "" }
        let date = Date(timeIntervalSince1970: TimeInterval(seconds))
        return date.formatted(date: .abbreviated, time: .omitted)
    }
}
