import AppKit
import Foundation
import SapuFFI

/// A message shown in the status bar after an action.
struct Notice: Equatable {
    enum Kind { case ok, error }
    var kind: Kind
    var text: String
}

/// Holds the current scan and talks to sapu-core. Everything that touches
/// files runs off the main thread and goes through sapu-core's safety checks.
@MainActor
final class AppModel: ObservableObject {
    @Published var rootPath: String = SapuFFI.homeFolder()
    @Published private(set) var result: ScanResult?
    @Published private(set) var summary: ScanSummary?
    @Published private(set) var suggestions: [Suggestion] = []
    /// Paths ticked in Recommendations.
    @Published var ticked: Set<String> = []
    @Published private(set) var scanning = false
    @Published private(set) var working = false
    @Published private(set) var progress: ScanProgress?
    @Published var notice: Notice?
    @Published var useAI = true
    @Published private(set) var history: [HistoryBatch] = []
    @Published private(set) var protected: [String] = []
    @Published private(set) var locations: [DataLocation] = []
    @Published private(set) var fullDiskAccess = SapuFFI.hasFullDiskAccess()
    /// Bumped after every scan so views reload their rows.
    @Published private(set) var generation = 0

    private let scanner = DiskScanner()
    private var pollTask: Task<Void, Never>?

    init() {
        SapuFFI.startLogging()
        SapuFFI.logEvent(message: "window opened, Full Disk Access: \(fullDiskAccess ? "yes" : "no")")
        refreshSafety()
    }

    /// Cleaning is only allowed on a finished, fresh scan, and one thing at a time.
    var canClean: Bool { result != nil && !scanning && !working }

    // MARK: Scanning

    func scan() {
        guard !scanning, !working else { return }
        scanning = true
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
                    self.ticked = Set(suggestions.filter { $0.preselected }.map { $0.path })
                    self.generation += 1
                    self.finishScan()
                }
            } catch {
                let message = AppModel.describe(error)
                await MainActor.run {
                    self.notice = Notice(kind: .error, text: message)
                    self.finishScan()
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

    private func finishScan() {
        scanning = false
        pollTask?.cancel()
        pollTask = nil
        fullDiskAccess = SapuFFI.hasFullDiskAccess()
        refreshSafety()
    }

    func children(of entry: FileEntry?) -> [FileEntry] {
        guard let result else { return [] }
        do {
            return try result.children(idxPath: entry?.idxPath ?? [])
        } catch {
            notice = Notice(kind: .error, text: AppModel.describe(error))
            return []
        }
    }

    func largestFiles() -> [FileEntry] {
        result?.largestFiles(count: 100) ?? []
    }

    // MARK: Recommendations

    var tickedSuggestions: [Suggestion] {
        suggestions.filter { $0.automatic && ticked.contains($0.path) }
    }

    /// The "I've checked" sentence, if the ticked batch needs one.
    func confirmationNeeded() -> String? {
        result?.confirmationNeeded(paths: tickedSuggestions.map { $0.path })
    }

    func cleanTicked(checked: Bool) {
        guard canClean, let result else { return }
        let paths = tickedSuggestions.map { $0.path }
        guard !paths.isEmpty else { return }
        SapuFFI.logEvent(message: "clean confirmed: \(paths.count) items, checked: \(checked)")
        run("Cleaning…") {
            let report = try result.clean(paths: paths, checked: checked)
            return AppModel.describe(report, verb: "Cleaned")
        }
    }

    // MARK: Single items

    func moveToTrash(_ entry: FileEntry) {
        guard canClean, let result else { return }
        SapuFFI.logEvent(message: "move to Trash confirmed: \(entry.path)")
        run("Moving to the Trash…") {
            let report = try result.moveToTrash(idxPath: entry.idxPath)
            return AppModel.describe(report, verb: "Moved to the Trash:")
        }
    }

    func deleteFromICloud(_ entry: FileEntry, understood: Bool) {
        guard canClean, let result else { return }
        SapuFFI.logEvent(message: "iCloud delete confirmed: \(entry.path), consent: \(understood)")
        run("Deleting from iCloud…") {
            _ = try result.deleteFromIcloud(idxPath: entry.idxPath, iUnderstand: understood)
            return "Deleted \(entry.name) from iCloud. It's in the Trash on this Mac, so Undo still works here."
        }
    }

    func removeDownload(_ entry: FileEntry) {
        guard canClean, let result else { return }
        SapuFFI.logEvent(message: "remove download: \(entry.path)")
        run("Removing the download…") {
            try result.removeDownload(idxPath: entry.idxPath)
            return "Removed the download of \(entry.name). It's still in iCloud."
        }
    }

    func download(_ entry: FileEntry) {
        guard let result else { return }
        SapuFFI.logEvent(message: "download: \(entry.path)")
        run("Starting the download…", rescan: false) {
            try result.download(idxPath: entry.idxPath)
            return "Downloading \(entry.name) from iCloud."
        }
    }

    func markKeep(_ entry: FileEntry) {
        guard let result else { return }
        do {
            try result.markKeep(idxPath: entry.idxPath)
            notice = Notice(kind: .ok, text: "Marked \(entry.name) as Keep. Sapu AI learns this on the next scan.")
        } catch {
            notice = Notice(kind: .error, text: AppModel.describe(error))
        }
    }

    // MARK: Safety

    func protect(_ path: String) {
        do {
            try SapuFFI.protect(path: path)
            notice = Notice(kind: .ok, text: "Protected. Sapu will never touch \(path) or a folder containing it.")
        } catch {
            notice = Notice(kind: .error, text: AppModel.describe(error))
        }
        refreshSafety()
        if result != nil { scan() }
    }

    func unprotect(_ path: String) {
        do {
            _ = try SapuFFI.unprotect(path: path)
            notice = Notice(kind: .ok, text: "Removed \(path) from the protected list.")
        } catch {
            notice = Notice(kind: .error, text: AppModel.describe(error))
        }
        refreshSafety()
    }

    func undo(_ batch: HistoryBatch) {
        guard !working, !scanning else { return }
        SapuFFI.logEvent(message: "undo requested: batch \(batch.id)")
        run("Putting things back…") {
            let report = SapuFFI.undo(batchId: batch.id)
            var text = "Restored \(report.restored) items from the Trash."
            if !report.failed.isEmpty {
                text += " \(report.failed.count) couldn't be restored automatically; use Finder, Trash, Put Back."
            }
            return text
        }
    }

    func clearCache() {
        do {
            try SapuFFI.clearCache()
            notice = Notice(kind: .ok, text: "Scan cache cleared.")
        } catch {
            notice = Notice(kind: .error, text: AppModel.describe(error))
        }
        refreshSafety()
    }

    func refreshSafety() {
        history = SapuFFI.history()
        protected = SapuFFI.protectedPaths()
        locations = SapuFFI.dataLocations()
    }

    // MARK: Helpers

    /// Run a file action in the background, show the outcome, then rescan so
    /// the window never shows items that are already gone.
    private func run(_ what: String, rescan: Bool = true, _ action: @escaping @Sendable () throws -> String) {
        working = true
        notice = Notice(kind: .ok, text: what)
        Task.detached(priority: .userInitiated) {
            let outcome: Notice
            do {
                outcome = Notice(kind: .ok, text: try action())
            } catch {
                outcome = Notice(kind: .error, text: AppModel.describe(error))
            }
            await MainActor.run {
                self.notice = outcome
                self.working = false
                self.refreshSafety()
                if rescan && self.result != nil { self.scan() }
            }
        }
    }

    nonisolated static func describe(_ report: CleanReport, verb: String) -> String {
        var text = "\(verb) \(report.done) items, freed \(Format.size(report.freedBytes))."
        if let first = report.skipped.first {
            text += " Skipped \(report.skipped.count) (first: \(first.reason))."
        }
        return text + " Undo is in Safety."
    }

    nonisolated static func describe(_ error: Error) -> String {
        if let error = error as? SapuError {
            switch error {
            case let .Failed(message), let .NotFound(message), let .NeedsConfirmation(message):
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
        try? FileManager.default.createDirectory(atPath: path, withIntermediateDirectories: true)
        NSWorkspace.shared.open(URL(fileURLWithPath: path))
    }

    static func copy(_ path: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(path, forType: .string)
    }

    static func openFullDiskAccessSettings() {
        let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        if let url = URL(string: url) { NSWorkspace.shared.open(url) }
    }

    /// Opens a log file in Console.app.
    static func openInConsole(_ path: String) {
        let file = URL(fileURLWithPath: path)
        let console = URL(fileURLWithPath: "/System/Applications/Utilities/Console.app")
        NSWorkspace.shared.open([file], withApplicationAt: console, configuration: NSWorkspace.OpenConfiguration())
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

    static func dateTime(_ seconds: UInt64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(seconds))
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}
