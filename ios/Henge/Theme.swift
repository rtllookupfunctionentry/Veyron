import SwiftUI
import UIKit

/// The Desk: a quiet page, cards on it with a hairline edge, and brushed silver for what you press, as
/// Veyron's mark is silver on charcoal. A Mac's screen is the only colourful thing here. Dark, or light
/// (Settings › Appearance), where the silver turns to charcoal on a pale page; a running Mac's session is
/// always dark.
enum Desk {
    static let page = tone(light: (244, 244, 245), dark: (20, 20, 21))
    static let card = tone(light: (255, 255, 255), dark: (28, 28, 30))
    static let raised = tone(light: (232, 232, 235), dark: (41, 41, 44))
    static let text = tone(light: (22, 22, 23), dark: (238, 238, 239))
    static let dim = tone(light: (99, 99, 104), dark: (152, 152, 157))
    static let faint = tone(light: (142, 142, 147), dark: (106, 106, 112))
    static let accent = tone(light: (44, 44, 46), dark: (207, 207, 210))
    /// For tints and text: lighter on the dark page, deeper on the light one.
    static let accentLight = tone(light: (28, 28, 30), dark: (228, 228, 230))
    /// What is written on the accent.
    static let onAccent = tone(light: (244, 244, 245), dark: (22, 22, 23))
    /// A switch that is on: apart from one that is off, and dark enough for its white knob to show.
    static let switchOn = tone(light: (72, 72, 76), dark: (132, 132, 138))
    /// The mark's own shading, for the one action a screen is for.
    static let silver = LinearGradient(colors: [tone(light: (72, 72, 76), dark: (236, 236, 237)), tone(light: (28, 28, 30), dark: (168, 168, 172))],
                                       startPoint: .topLeading, endPoint: .bottomTrailing)
    static let good = tone(light: (20, 145, 90), dark: (57, 217, 138))
    static let warn = tone(light: (184, 119, 0), dark: (255, 188, 46))
    static let bad = tone(light: (210, 56, 43), dark: (255, 122, 112))
    static let hairline = Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(white: 1, alpha: 0.1) : UIColor(white: 0, alpha: 0.1) })

    private static func tone(light: (Int, Int, Int), dark: (Int, Int, Int)) -> Color {
        func ui(_ c: (Int, Int, Int)) -> UIColor {
            UIColor(red: CGFloat(c.0) / 255, green: CGFloat(c.1) / 255, blue: CGFloat(c.2) / 255, alpha: 1)
        }
        let l = ui(light), d = ui(dark)
        return Color(UIColor { $0.userInterfaceStyle == .dark ? d : l })
    }
}

/// Settings › Appearance.
enum DeskAppearance: String, CaseIterable, Identifiable {
    case system, light, dark
    static let key = "appearance"
    var id: String { rawValue }
    var title: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }
    var style: UIUserInterfaceStyle {
        switch self {
        case .system: return .unspecified
        case .light: return .light
        case .dark: return .dark
        }
    }
}

/// Sets its own window's style directly, so System follows iOS's setting as it changes and a
/// running Mac's session (in the same window) can stay dark over a light page.
struct WindowStyle: UIViewRepresentable {
    let style: UIUserInterfaceStyle

    final class Probe: UIView {
        var style = UIUserInterfaceStyle.unspecified { didSet { apply() } }
        override func didMoveToWindow() { super.didMoveToWindow(); apply() }
        private func apply() {
            if let window, window.overrideUserInterfaceStyle != style { window.overrideUserInterfaceStyle = style }
        }
    }

    func makeUIView(context: Context) -> Probe {
        let probe = Probe()
        probe.isUserInteractionEnabled = false
        probe.style = style
        return probe
    }

    func updateUIView(_ probe: Probe, context: Context) { probe.style = style }
}

extension View {
    /// Liquid Glass where iOS has it, for controls floating over a Mac's screen.
    @ViewBuilder func deskGlass<S: Shape>(in shape: S) -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            glassEffect(.regular, in: shape)
        } else {
            background(.ultraThinMaterial, in: shape)
        }
        #else
        background(.ultraThinMaterial, in: shape)
        #endif
    }

    /// Keeps a page's content a readable width on a big screen.
    func deskColumn() -> some View {
        frame(maxWidth: 680).frame(maxWidth: .infinity)
    }

    /// A card on the page.
    func deskCard(radius: CGFloat = 22) -> some View {
        background(Desk.card, in: RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: radius, style: .continuous).stroke(Desk.hairline, lineWidth: 0.5))
    }
}

/// The one action a screen is for.
struct DeskButtonStyle: ButtonStyle {
    var enabled = true
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 19, weight: .bold))
            .foregroundStyle(enabled ? Desk.onAccent : Desk.dim)
            .frame(maxWidth: .infinity)
            .frame(height: 58)
            .background(enabled ? AnyShapeStyle(Desk.silver) : AnyShapeStyle(Desk.raised), in: Capsule())
            .opacity(configuration.isPressed ? 0.85 : 1)
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// Labelled the way the page's other headings are.
struct DeskHeading: View {
    let title: String
    init(_ title: String) { self.title = title }
    var body: some View {
        Text(title)
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(Desk.dim)
            .padding(.horizontal, 18)
            .padding(.top, 18)
            .padding(.bottom, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A row of a card.
struct DeskBar: View {
    let fraction: Double
    var height: CGFloat = 8
    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Desk.raised)
                Capsule().fill(Desk.accent).frame(width: max(height, geo.size.width * min(max(fraction, 0), 1)))
            }
        }
        .frame(height: height)
    }
}

/// The picture of a Mac as it was last seen, kept beside its disk.
enum MacPreview {
    static func file(for mac: VirtualMac) -> URL { mac.directory.appendingPathComponent("preview.jpg") }

    static func capture(width: Int, height: Int) -> CGImage? {
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        guard henge_vm_snapshot(&pixels, Int32(width), Int32(height)) == 0 else { return nil }
        guard let provider = CGDataProvider(data: Data(pixels) as CFData) else { return nil }
        let info = CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
        return CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
                       space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: info, provider: provider, decode: nil,
                       shouldInterpolate: true, intent: .defaultIntent)
    }

    /// Off the main thread.
    static func save(for mac: VirtualMac) {
        let url = file(for: mac), height = Int(640 / mac.aspect)
        Task.detached(priority: .utility) { write(to: url, height: height) }
    }

    /// Before the machine stops, while there is still a screen to read.
    static func saveNow(for mac: VirtualMac) { write(to: file(for: mac), height: Int(640 / mac.aspect)) }

    private static func write(to url: URL, height: Int) {
        guard let image = capture(width: 640, height: height), let data = UIImage(cgImage: image).jpegData(compressionQuality: 0.8) else { return }
        try? data.write(to: url, options: .atomic)
    }
}

enum ByteText {
    static func string(_ bytes: Int64) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        formatter.allowedUnits = [.useGB, .useMB]
        formatter.includesUnit = true
        return formatter.string(fromByteCount: bytes)
    }

    static func string(_ bytes: UInt64) -> String {
        string(Int64(clamping: bytes))
    }
}

/// The names the JIT setup screens (from Husk) use, in Veyron's colours.
enum Theme {
    static let accent = Desk.accent
    static let accentSoft = Desk.accent.opacity(0.18)
    static let backdrop = Desk.page
    static let text = Desk.text
    static let textDim = Desk.dim
}

extension View {
    /// high: a card raised above the others (a result, an outcome).
    func hengeCard(high: Bool = false) -> some View {
        background(high ? Desk.raised : Desk.card, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 22, style: .continuous).stroke(Desk.hairline, lineWidth: 1))
    }
}

/// The one action a screen is for (the JIT setup screens use it).
struct PrimaryButtonStyle: ButtonStyle {
    var enabled = true

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 16, weight: .semibold))
            .foregroundStyle(enabled ? Desk.onAccent : Desk.dim)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 15)
            .background(enabled ? AnyShapeStyle(Desk.silver) : AnyShapeStyle(Desk.raised), in: RoundedRectangle(cornerRadius: 15, style: .continuous))
            .opacity(configuration.isPressed ? 0.85 : 1)
            .scaleEffect(configuration.isPressed ? 0.985 : 1)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}

/// A card that is also a button: it moves a little under the finger.
struct CardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .opacity(configuration.isPressed ? 0.9 : 1)
            .animation(.easeOut(duration: 0.14), value: configuration.isPressed)
    }
}
