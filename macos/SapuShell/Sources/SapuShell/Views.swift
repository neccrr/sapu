import SapuFFI
import SwiftUI

extension VerdictKind {
    var tint: Color {
        switch self {
        case .safe: return .green
        case .likely: return .teal
        case .review: return .orange
        case .keep: return .secondary
        }
    }

    var short: String {
        switch self {
        case .safe: return "Safe"
        case .likely: return "Likely"
        case .review: return "Review"
        case .keep: return "Keep"
        }
    }
}

extension FileEntry: Identifiable {
    public var id: String { path }
}

struct VerdictBadge: View {
    let opinion: Opinion

    var body: some View {
        Text(opinion.fromAi ? "\(opinion.verdict.short) (AI)" : opinion.verdict.short)
            .font(.caption.weight(.medium))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .foregroundStyle(opinion.verdict.tint)
            .background(opinion.verdict.tint.opacity(0.12), in: Capsule())
            .help(opinion.reason)
    }
}

struct SizeBar: View {
    let fraction: Double

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule().fill(Color.accentColor.opacity(0.7))
                    .frame(width: max(2, geo.size.width * min(1, max(0, fraction))))
            }
        }
        .frame(width: 90, height: 6)
    }
}

/// What the window is asking you to confirm.
enum Pending: Identifiable {
    case trash(FileEntry)
    case icloudDelete(FileEntry)

    var id: String {
        switch self {
        case let .trash(e): return "trash:" + e.path
        case let .icloudDelete(e): return "icloud:" + e.path
        }
    }
}

/// Right-click actions for one item.
struct EntryMenu: View {
    @EnvironmentObject var model: AppModel
    let entry: FileEntry
    @Binding var pending: Pending?

    var body: some View {
        Button("Reveal in Finder") { Finder.reveal(entry.path) }
        Button("Copy Path") { Finder.copy(entry.path) }
        Divider()
        if entry.inIcloud {
            Button("Remove Download") { model.removeDownload(entry) }
                .disabled(!model.canClean)
            Button("Download Now") { model.download(entry) }
            Button("Delete from iCloud…") { pending = .icloudDelete(entry) }
                .disabled(!model.canClean)
        } else {
            Button("Move to Trash…") { pending = .trash(entry) }
                .disabled(!model.canClean)
        }
        Divider()
        Button("Protect") { model.protect(entry.path) }
        if !entry.idxPath.isEmpty {
            Button("Mark as Keep") { model.markKeep(entry) }
        }
    }
}

/// Confirmation for a single item. iCloud deletes need their own "I understand".
struct ConfirmSheet: View {
    @EnvironmentObject var model: AppModel
    let pending: Pending
    let close: () -> Void
    @State private var understood = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            switch pending {
            case let .trash(entry):
                Text("Move \(entry.name) to the Trash?").font(.headline)
                Text("\(Format.size(entry.size)). It's re-checked first, and you can put it back with Undo in Safety until you empty the Trash.")
                    .foregroundStyle(.secondary)
                Text(entry.path).font(.caption.monospaced()).foregroundStyle(.tertiary)
                HStack {
                    Spacer()
                    Button("Cancel", role: .cancel, action: close).keyboardShortcut(.cancelAction)
                    Button("Move to Trash") {
                        model.moveToTrash(entry)
                        close()
                    }
                    .keyboardShortcut(.defaultAction)
                }
            case let .icloudDelete(entry):
                Text("Delete \(entry.name) from iCloud?").font(.headline)
                Text("This removes it from iCloud and from all your devices, not just this Mac. On this Mac it goes to the Trash, so Undo works here until you empty it. If you only want space back on this Mac, use Remove Download instead.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text(entry.path).font(.caption.monospaced()).foregroundStyle(.tertiary)
                Toggle("I understand this deletes it from all my devices.", isOn: $understood)
                HStack {
                    Button("Remove Download Instead") {
                        model.removeDownload(entry)
                        close()
                    }
                    Spacer()
                    Button("Cancel", role: .cancel, action: close).keyboardShortcut(.cancelAction)
                    Button("Delete from iCloud", role: .destructive) {
                        model.deleteFromICloud(entry, understood: understood)
                        close()
                    }
                    .disabled(!understood)
                }
            }
        }
        .padding(20)
        .frame(width: 460)
    }
}

// MARK: - Browse

struct BrowseView: View {
    @EnvironmentObject var model: AppModel
    /// Folders opened from the root, outermost first.
    @State private var stack: [FileEntry] = []
    @State private var rows: [FileEntry] = []
    @State private var selection: String?
    @State private var pending: Pending?

    private var current: FileEntry? { stack.last }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 4) {
                Button { goBack() } label: { Image(systemName: "chevron.left") }
                    .disabled(stack.isEmpty)
                    .help("Back")
                    .glassButton()
                Breadcrumbs(root: model.summary?.rootPath ?? model.rootPath, stack: stack) { depth in
                    stack = Array(stack.prefix(depth))
                    reload()
                }
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            Divider()

            let total = max(1, current?.size ?? model.summary?.bytes ?? 1)
            Table(rows, selection: $selection) {
                TableColumn("Name") { entry in
                    HStack(spacing: 6) {
                        Image(systemName: entry.isDir ? "folder" : "doc")
                            .foregroundStyle(.secondary)
                        Text(entry.name).lineLimit(1)
                        if entry.inIcloud {
                            Image(systemName: entry.size * 2 < entry.logical ? "icloud" : "icloud.and.arrow.down")
                                .foregroundStyle(.secondary)
                                .help(entry.size * 2 < entry.logical ? "Only in iCloud" : "Downloaded from iCloud")
                        }
                        if entry.incomplete {
                            Image(systemName: "exclamationmark.circle")
                                .foregroundStyle(.orange)
                                .help("Some items inside couldn't be read.")
                        }
                    }
                }
                TableColumn("Size") { entry in
                    HStack {
                        SizeBar(fraction: Double(entry.size) / Double(total))
                        Text(Format.size(entry.size)).monospacedDigit()
                    }
                }
                .width(min: 150, ideal: 170)
                TableColumn("Verdict") { entry in
                    if let opinion = entry.opinion, opinion.verdict != .keep {
                        VerdictBadge(opinion: opinion)
                    }
                }
                .width(min: 80, ideal: 100)
                TableColumn("Modified") { entry in
                    Text(Format.date(entry.modified)).foregroundStyle(.secondary)
                }
                .width(min: 90, ideal: 110)
            }
            .contextMenu(forSelectionType: String.self) { ids in
                if let id = ids.first, let entry = rows.first(where: { $0.path == id }) {
                    EntryMenu(entry: entry, pending: $pending)
                }
            } primaryAction: { ids in
                guard let id = ids.first, let entry = rows.first(where: { $0.path == id }) else { return }
                if entry.hasChildren {
                    stack.append(entry)
                    reload()
                } else {
                    Finder.reveal(entry.path)
                }
            }

            if let id = selection, let entry = rows.first(where: { $0.path == id }), let opinion = entry.opinion {
                Divider()
                HStack(alignment: .firstTextBaseline) {
                    VerdictBadge(opinion: opinion)
                    Text(opinion.reason)
                    Spacer()
                    Text(opinion.category).foregroundStyle(.secondary)
                }
                .font(.callout)
                .padding(10)
            }
        }
        .sheet(item: $pending) { p in
            ConfirmSheet(pending: p) { pending = nil }
        }
        .onAppear(perform: reload)
        .onChange(of: model.generation) { _ in
            // A new scan: keep the place if the folders still exist, else go back to the top.
            stack = []
            reload()
        }
    }

    private func goBack() {
        guard !stack.isEmpty else { return }
        stack.removeLast()
        reload()
    }

    private func reload() {
        rows = model.children(of: current)
        selection = nil
    }
}

struct Breadcrumbs: View {
    let root: String
    let stack: [FileEntry]
    let jump: (Int) -> Void

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 2) {
                Button((root as NSString).lastPathComponent) { jump(0) }
                    .buttonStyle(.borderless)
                ForEach(Array(stack.enumerated()), id: \.offset) { index, entry in
                    Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.tertiary)
                    Button(entry.name) { jump(index + 1) }
                        .buttonStyle(.borderless)
                }
            }
            .font(.callout)
        }
    }
}

// MARK: - Recommendations

struct RecommendationsView: View {
    @EnvironmentObject var model: AppModel
    @State private var confirming = false

    private let groups: [VerdictKind] = [.safe, .likely, .review]

    var body: some View {
        if model.suggestions.isEmpty {
            VStack(spacing: 8) {
                Text("Nothing to clean here").font(.system(.title2, design: .serif))
                Text("Sapu didn't find caches, leftovers or old installers worth removing in this folder.")
                    .foregroundStyle(.secondary)
            }
        } else {
            VStack(spacing: 0) {
                List {
                    ForEach(groups, id: \.self) { verdict in
                        let items = model.suggestions.filter { $0.opinion.verdict == verdict }
                        if !items.isEmpty {
                            SwiftUI.Section {
                                ForEach(items, id: \.path) { item in
                                    SuggestionRow(item: item, ticked: binding(for: item))
                                        .contextMenu {
                                            Button("Reveal in Finder") { Finder.reveal(item.path) }
                                            Button("Copy Path") { Finder.copy(item.path) }
                                        }
                                }
                            } header: {
                                HStack {
                                    Text(SapuFFI.verdictLabel(verdict: verdict))
                                    Spacer()
                                    Text(Format.size(items.reduce(0) { $0 + $1.size })).monospacedDigit()
                                }
                                .font(.headline)
                            }
                        }
                    }
                }
            }
            // The action bar floats over the list in glass.
            .safeAreaInset(edge: .bottom, spacing: 0) {
                let picked = model.tickedSuggestions
                HStack {
                    Text("AI guesses and Review items are never ticked for you. Each item is re-checked just before cleaning.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Move \(picked.count) to Trash (\(Format.size(picked.reduce(0) { $0 + $1.size })))") {
                        confirming = true
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(picked.isEmpty || !model.canClean)
                    .glassButton(prominent: true)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .glassPanel()
                .padding(.horizontal, 10)
                .padding(.bottom, 6)
            }
            .sheet(isPresented: $confirming) {
                BatchConfirmSheet { confirming = false }
            }
        }
    }

    private func binding(for item: Suggestion) -> Binding<Bool> {
        Binding(
            get: { model.ticked.contains(item.path) },
            set: { on in
                if on { model.ticked.insert(item.path) } else { model.ticked.remove(item.path) }
            }
        )
    }
}

struct BatchConfirmSheet: View {
    @EnvironmentObject var model: AppModel
    let close: () -> Void
    @State private var checked = false

    var body: some View {
        let picked = model.tickedSuggestions
        let why = model.confirmationNeeded()
        VStack(alignment: .leading, spacing: 12) {
            Text("Move \(picked.count) items to the Trash?").font(.headline)
            Text("This frees about \(Format.size(picked.reduce(0) { $0 + $1.size })). Anything that changed since the scan is skipped, and you can put it all back with Undo in Safety until you empty the Trash.")
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            if let why {
                Toggle(why, isOn: $checked)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel, action: close).keyboardShortcut(.cancelAction)
                Button("Move to Trash") {
                    model.cleanTicked(checked: checked)
                    close()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(why != nil && !checked)
            }
        }
        .padding(20)
        .frame(width: 460)
    }
}

struct SuggestionRow: View {
    let item: Suggestion
    @Binding var ticked: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            if item.automatic {
                Toggle("", isOn: $ticked).labelsHidden()
            } else {
                Image(systemName: "hand.raised").foregroundStyle(.secondary).help("Sapu can't do this one for you.")
            }
            VStack(alignment: .leading, spacing: 3) {
                Text((item.path as NSString).lastPathComponent).font(.body.weight(.medium))
                Text(item.opinion.reason).font(.callout).foregroundStyle(.secondary)
                Text(item.path)
                    .font(.caption.monospaced())
                    .foregroundStyle(.tertiary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 4) {
                Text(Format.size(item.size)).monospacedDigit()
                if item.opinion.action == .removeDownload {
                    Text("Remove download").font(.caption).foregroundStyle(.secondary)
                } else if item.opinion.action == .trashContents {
                    Text("Empties the folder").font(.caption).foregroundStyle(.secondary)
                }
                if item.opinion.fromAi {
                    Text("Sapu AI, \(Int(item.opinion.confidence * 100))% sure")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 3)
    }
}

// MARK: - Largest files

struct LargestFilesView: View {
    @EnvironmentObject var model: AppModel
    @State private var rows: [FileEntry] = []

    var body: some View {
        Table(rows) {
            TableColumn("Name") { entry in Text(entry.name).lineLimit(1) }
            TableColumn("Size") { entry in Text(Format.size(entry.size)).monospacedDigit() }
                .width(min: 90, ideal: 100)
            TableColumn("Where") { entry in
                Text((entry.path as NSString).deletingLastPathComponent)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .contextMenu(forSelectionType: String.self) { ids in
            if let id = ids.first {
                Button("Reveal in Finder") { Finder.reveal(id) }
                Button("Copy Path") { Finder.copy(id) }
            }
        } primaryAction: { ids in
            if let id = ids.first { Finder.reveal(id) }
        }
        .onAppear { rows = model.largestFiles() }
        .onChange(of: model.generation) { _ in rows = model.largestFiles() }
    }
}

// MARK: - Safety

struct SafetyView: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        Form {
            SwiftUI.Section("What Sapu never touches") {
                Text("macOS itself (/System, /usr, /Library…), your home folder itself, ~/Library itself, keychains and .ssh. Everything goes to the Trash, never deleted outright.")
                    .foregroundStyle(.secondary)
            }
            SwiftUI.Section("Your protected list") {
                if model.protected.isEmpty {
                    Text("Nothing yet. Right-click an item in Browse, then Protect.").foregroundStyle(.secondary)
                }
                ForEach(model.protected, id: \.self) { path in
                    HStack {
                        Text(path).font(.callout.monospaced()).lineLimit(1).truncationMode(.middle)
                        Spacer()
                        Button("Remove") { model.unprotect(path) }
                    }
                }
            }
            SwiftUI.Section("History") {
                if model.history.isEmpty {
                    Text("Nothing has been moved to the Trash yet.").foregroundStyle(.secondary)
                }
                ForEach(model.history, id: \.id) { batch in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(batch.label).lineLimit(1).truncationMode(.middle)
                            Text("\(Format.dateTime(batch.time)), \(batch.items) items, \(Format.size(batch.bytes))")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if batch.restorable > 0 {
                            Button("Undo") { model.undo(batch) }
                                .disabled(model.working || model.scanning)
                        } else {
                            Text("Not in the Trash any more").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            SwiftUI.Section("Data on this Mac") {
                ForEach(model.locations, id: \.path) { loc in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(loc.name).font(.body.weight(.medium))
                            Text(loc.note).font(.callout).foregroundStyle(.secondary)
                            Text(loc.path).font(.caption.monospaced()).foregroundStyle(.tertiary)
                        }
                        Spacer()
                        Text(Format.size(loc.bytes)).monospacedDigit().foregroundStyle(.secondary)
                        Button("Show") { Finder.open(loc.path) }
                    }
                }
                Button("Clear Scan Cache") { model.clearCache() }
            }
            SwiftUI.Section("About") {
                LabeledContent("Version", value: SapuFFI.version())
                LabeledContent("Engine", value: "sapu-core, shared with Sapu and the sapu command")
            }
        }
        .formStyle(.grouped)
        .onAppear { model.refreshSafety() }
    }
}

// MARK: - Logs

struct LogsView: View {
    @State private var lines: [String] = []
    @State private var filter = ""

    private var shown: [String] {
        filter.isEmpty ? lines : lines.filter { $0.localizedCaseInsensitiveContains(filter) }
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                TextField("Search", text: $filter).textFieldStyle(.roundedBorder).frame(maxWidth: 240)
                Spacer()
                Button("Refresh") { reload() }
                Button("Copy") { Finder.copy(shown.joined(separator: "\n")) }
                Button("Open in Console") { Finder.openInConsole(SapuFFI.logPath()) }
            }
            .padding(10)
            Divider()
            ScrollView {
                Text(shown.joined(separator: "\n"))
                    .font(.caption.monospaced())
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
        }
        .onAppear(perform: reload)
    }

    private func reload() {
        lines = SapuFFI.recentLog(lines: 500)
    }
}
