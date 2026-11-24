import AppKit
import SwiftUI

// Liquid Glass (macOS 26 and later).
//
// Where glass goes: only the controls layer that floats over content, such as
// the path bar, status bar, action bar, filter chips, banners and the scanning
// card. The file lists themselves stay plain so text is easy to read.
//
// What it uses:
// - `glassEffect` with tint and `interactive()` (chips and pills react to the pointer),
// - `GlassEffectContainer` so neighbouring pieces blend into each other,
// - `glassEffectID` so pieces morph when they appear, disappear or change,
// - `.glass` / `.glassProminent` button styles,
// - soft scroll edges under the floating bars.
//
// Fallbacks: on older macOS, with Reduce Transparency on, or with
// `SAPU_NO_GLASS=1` (used by scripts/perf-macos.sh to measure what glass
// costs), the same places use an ordinary material or a solid fill.
// `#if compiler(>=6.2)` keeps this building with Xcode versions before 26.

enum GlassSettings {
    /// `SAPU_NO_GLASS=1` turns glass off for this run.
    static let disabled = ProcessInfo.processInfo.environment["SAPU_NO_GLASS"] == "1"
}

/// How a glass piece should look.
struct GlassStyle {
    var tint: Color? = nil
    /// Reacts to the pointer (for things you click).
    var interactive = false
    /// The clearer variant, for small pieces over busy content.
    var clear = false
}

private struct GlassPanelModifier<S: InsettableShape>: ViewModifier {
    let shape: S
    let style: GlassStyle
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    @ViewBuilder
    func body(content: Content) -> some View {
        if reduceTransparency {
            content.background(Color(nsColor: .windowBackgroundColor), in: shape)
                .overlay(shape.strokeBorder(Color.secondary.opacity(0.3)))
        } else if GlassSettings.disabled {
            fallback(content)
        } else {
            #if compiler(>=6.2)
                if #available(macOS 26.0, *) {
                    content.glassEffect(glass, in: shape)
                } else {
                    fallback(content)
                }
            #else
                fallback(content)
            #endif
        }
    }

    private func fallback(_ content: Content) -> some View {
        content
            .background(.regularMaterial, in: shape)
            .background((style.tint ?? .clear).opacity(0.18), in: shape)
    }

    #if compiler(>=6.2)
        @available(macOS 26.0, *)
        private var glass: Glass {
            var g: Glass = style.clear ? .clear : .regular
            if let tint = style.tint { g = g.tint(tint) }
            if style.interactive { g = g.interactive() }
            return g
        }
    #endif
}

private struct GlassIDModifier: ViewModifier {
    let id: String
    let namespace: Namespace.ID

    @ViewBuilder
    func body(content: Content) -> some View {
        #if compiler(>=6.2)
            if #available(macOS 26.0, *), !GlassSettings.disabled {
                content.glassEffectID(id, in: namespace)
            } else {
                content
            }
        #else
            content
        #endif
    }
}

private struct GlassButtonModifier: ViewModifier {
    let prominent: Bool
    let tint: Color?

    @ViewBuilder
    func body(content: Content) -> some View {
        #if compiler(>=6.2)
            if #available(macOS 26.0, *), !GlassSettings.disabled {
                if prominent {
                    content.buttonStyle(.glassProminent).tint(tint)
                } else {
                    content.buttonStyle(.glass).tint(tint)
                }
            } else {
                fallback(content)
            }
        #else
            fallback(content)
        #endif
    }

    @ViewBuilder
    private func fallback(_ content: Content) -> some View {
        if prominent {
            content.buttonStyle(.borderedProminent).tint(tint)
        } else {
            content.buttonStyle(.bordered).tint(tint)
        }
    }
}

private struct SoftScrollEdgeModifier: ViewModifier {
    @ViewBuilder
    func body(content: Content) -> some View {
        #if compiler(>=6.2)
            if #available(macOS 26.0, *), !GlassSettings.disabled {
                content.scrollEdgeEffectStyle(.soft, for: .all)
            } else {
                content
            }
        #else
            content
        #endif
    }
}

extension View {
    /// A floating glass panel with rounded corners.
    func glassPanel(cornerRadius: CGFloat = 16, _ style: GlassStyle = GlassStyle()) -> some View {
        modifier(GlassPanelModifier(shape: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous), style: style))
    }

    /// A glass capsule, for pills and chips.
    func glassCapsule(_ style: GlassStyle = GlassStyle()) -> some View {
        modifier(GlassPanelModifier(shape: Capsule(), style: style))
    }

    /// Lets this glass piece morph into, out of, or between others with the same namespace.
    func glassID(_ id: String, in namespace: Namespace.ID) -> some View {
        modifier(GlassIDModifier(id: id, namespace: namespace))
    }

    /// A glass button. `prominent` is for the one main action on a screen.
    func glassButton(prominent: Bool = false, tint: Color? = nil) -> some View {
        modifier(GlassButtonModifier(prominent: prominent, tint: tint))
    }

    /// Content fades softly where it scrolls under floating glass bars.
    func softScrollEdges() -> some View {
        modifier(SoftScrollEdgeModifier())
    }
}

/// Groups glass pieces so they blend and morph together.
struct GlassGroup<Content: View>: View {
    var spacing: CGFloat = 10
    @ViewBuilder var content: () -> Content

    var body: some View {
        #if compiler(>=6.2)
            if #available(macOS 26.0, *), !GlassSettings.disabled {
                GlassEffectContainer(spacing: spacing) { content() }
            } else {
                content()
            }
        #else
            content()
        #endif
    }
}

extension Animation {
    /// The spring used when glass pieces appear, disappear or morph.
    static let glassMorph = Animation.spring(response: 0.38, dampingFraction: 0.82)
}
