import SwiftUI
import UIKit

/// The Desk: a charcoal page, cards on it with a hairline edge, and brushed silver for what you press, as
/// Veyron's mark is silver on charcoal. A Mac's screen is the only colourful thing here.
enum Desk {
    static let page = Color(red: 20 / 255, green: 20 / 255, blue: 21 / 255)
    static let card = Color(red: 28 / 255, green: 28 / 255, blue: 30 / 255)
    static let raised = Color(red: 41 / 255, green: 41 / 255, blue: 44 / 255)
    static let text = Color(red: 238 / 255, green: 238 / 255, blue: 239 / 255)
    static let dim = Color(red: 152 / 255, green: 152 / 255, blue: 157 / 255)
    static let faint = Color(red: 106 / 255, green: 106 / 255, blue: 112 / 255)
    static let accent = Color(red: 207 / 255, green: 207 / 255, blue: 210 / 255)
    static let accentLight = Color(red: 228 / 255, green: 228 / 255, blue: 230 / 255)
    /// What is written on silver.
    static let onAccent = Color(red: 22 / 255, green: 22 / 255, blue: 23 / 255)
    /// A switch that is on: darker than the silver, so its white knob shows.
    static let switchOn = Color(red: 132 / 255, green: 132 / 255, blue: 138 / 255)
    /// The mark's own shading, for the one action a screen is for.
    static let silver = LinearGradient(colors: [Color(red: 236 / 255, green: 236 / 255, blue: 237 / 255), Color(red: 168 / 255, green: 168 / 255, blue: 172 / 255)],
                                       startPoint: .topLeading, endPoint: .bottomTrailing)
    static let good = Color(red: 57 / 255, green: 217 / 255, blue: 138 / 255)
    static let warn = Color(red: 1, green: 188 / 255, blue: 46 / 255)
    static let bad = Color(red: 1, green: 122 / 255, blue: 112 / 255)
    static let hairline = Color.white.opacity(0.1)
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
