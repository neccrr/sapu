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

/// Right-click actions. All of them only look; none change anything on disk.
struct EntryMenu: View {
    let path: String

    var body: some View {
        Button("Reveal in Finder") { Finder.reveal(path) }
        Button("Copy Path") { Finder.copy(path) }
    }
}

// MARK: - Browse

struct BrowseView: View {
    @EnvironmentObject var model: AppModel
    /// Folders opened from the root, outermost first.
    @State private var stack: [Entry] = []
    @State private var rows: [Entry] = []
    @State private var selection: String?

    private var current: Entry? { stack.last }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 4) {
                Button { goBack() } label: { Image(systemName: "chevron.left") }
                    .disabled(stack.isEmpty)
                    .keyboardShortcut(.delete, modifiers: [])
                    .help("Back")
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
                if let id = ids.first { EntryMenu(path: id) }
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
        .onAppear(perform: reload)
        .onChange(of: model.summary?.rootPath) { _ in
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

extension Entry: Identifiable {
    public var id: String { path }
}

struct Breadcrumbs: View {
    let root: String
    let stack: [Entry]
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

    private let groups: [VerdictKind] = [.safe, .likely, .review]

    var body: some View {
        if model.suggestions.isEmpty {
            VStack(spacing: 8) {
                Text("Nothing to clean here").font(.system(.title2, design: .serif))
                Text("Sapu didn't find caches, leftovers or old installers worth removing in this folder.")
                    .foregroundStyle(.secondary)
            }
        } else {
            List {
                ForEach(groups, id: \.self) { verdict in
                    let items = model.suggestions.filter { $0.opinion.verdict == verdict }
                    if !items.isEmpty {
                        SwiftUI.Section {
                            ForEach(items, id: \.path) { item in
                                SuggestionRow(item: item)
                                    .contextMenu { EntryMenu(path: item.path) }
                            }
                        } header: {
                            HStack {
                                Text(verdictLabel(verdict: verdict))
                                Spacer()
                                Text(Format.size(items.reduce(0) { $0 + $1.size })).monospacedDigit()
                            }
                            .font(.headline)
                        }
                    }
                }
            }
        }
    }
}

struct SuggestionRow: View {
    let item: Suggestion

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
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
    @State private var rows: [Entry] = []

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
            if let id = ids.first { EntryMenu(path: id) }
        } primaryAction: { ids in
            if let id = ids.first { Finder.reveal(id) }
        }
        .onAppear { rows = model.largestFiles() }
        .onChange(of: model.summary?.rootPath) { _ in rows = model.largestFiles() }
    }
}

// MARK: - About

struct AboutView: View {
    var body: some View {
        Form {
            SwiftUI.Section("Where Sapu keeps its data") {
                ForEach(dataLocations(), id: \.path) { loc in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(loc.name).font(.body.weight(.medium))
                            Text(loc.note).font(.callout).foregroundStyle(.secondary)
                            Text(loc.path).font(.caption.monospaced()).foregroundStyle(.tertiary)
                        }
                        Spacer()
                        Button("Show in Finder") { Finder.open(loc.path) }
                    }
                }
            }
            SwiftUI.Section("About") {
                LabeledContent("Version", value: version())
                LabeledContent("Engine", value: "sapu-core, shared with Sapu and the sapu command")
                Text("This SwiftUI window is a preview. It scans and explains, but never moves or deletes anything.")
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }
}
