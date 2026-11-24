import SwiftUI

// Liquid Glass (macOS 26 and later), with a translucent material on older
// macOS. Glass is for the controls layer only (bars, banners, action buttons),
// never for the file lists themselves, so content stays easy to read.
//
// `#if compiler(>=6.2)` keeps the shell building with older Xcode versions,
// whose SDKs don't have the glass APIs yet.

extension View {
    /// A floating glass panel, such as the status bar or a banner.
    @ViewBuilder
    func glassPanel(cornerRadius: CGFloat = 14) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        #if compiler(>=6.2)
            if #available(macOS 26.0, *) {
                self.glassEffect(.regular, in: shape)
            } else {
                self.background(.regularMaterial, in: shape)
            }
        #else
            self.background(.regularMaterial, in: shape)
        #endif
    }

    /// A glass button. `prominent` is for the one main action on a screen.
    @ViewBuilder
    func glassButton(prominent: Bool = false) -> some View {
        #if compiler(>=6.2)
            if #available(macOS 26.0, *) {
                if prominent {
                    self.buttonStyle(.glassProminent)
                } else {
                    self.buttonStyle(.glass)
                }
            } else {
                fallbackButton(prominent: prominent)
            }
        #else
            fallbackButton(prominent: prominent)
        #endif
    }

    @ViewBuilder
    private func fallbackButton(prominent: Bool) -> some View {
        if prominent {
            self.buttonStyle(.borderedProminent)
        } else {
            self.buttonStyle(.bordered)
        }
    }
}

/// Groups glass controls so they blend into each other on macOS 26.
struct GlassGroup<Content: View>: View {
    var spacing: CGFloat = 8
    @ViewBuilder var content: () -> Content

    var body: some View {
        #if compiler(>=6.2)
            if #available(macOS 26.0, *) {
                GlassEffectContainer(spacing: spacing) { content() }
            } else {
                content()
            }
        #else
            content()
        #endif
    }
}
