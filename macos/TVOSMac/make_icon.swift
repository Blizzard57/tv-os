import AppKit

let outputDirectory = CommandLine.arguments.dropFirst().first.map(URL.init(fileURLWithPath:))
guard let outputDirectory else {
    fputs("usage: make_icon.swift <iconset-directory> [icns-file]\n", stderr)
    exit(64)
}
let icnsFile = CommandLine.arguments.dropFirst(2).first.map(URL.init(fileURLWithPath:))

try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)

let icons: [(name: String, pixels: CGFloat)] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024)
]

var pngByPixelSize: [Int: Data] = [:]
for icon in icons {
    let image = drawIcon(size: icon.pixels)
    guard
        let tiff = image.tiffRepresentation,
        let bitmap = NSBitmapImageRep(data: tiff),
        let png = bitmap.representation(using: .png, properties: [:])
    else {
        fputs("could not render \(icon.name)\n", stderr)
        exit(1)
    }
    pngByPixelSize[Int(icon.pixels)] = png
    try png.write(to: outputDirectory.appendingPathComponent(icon.name))
}

if let icnsFile {
    try writeICNS(to: icnsFile, pngByPixelSize: pngByPixelSize)
}

private func writeICNS(to url: URL, pngByPixelSize: [Int: Data]) throws {
    let chunks: [(type: String, pixels: Int)] = [
        ("icp4", 16),
        ("icp5", 32),
        ("icp6", 64),
        ("ic07", 128),
        ("ic08", 256),
        ("ic09", 512),
        ("ic10", 1024),
        ("ic11", 32),
        ("ic12", 64),
        ("ic13", 256),
        ("ic14", 512)
    ]

    var body = Data()
    for chunk in chunks {
        guard let png = pngByPixelSize[chunk.pixels] else { continue }
        appendFourCC(chunk.type, to: &body)
        appendBE32(UInt32(png.count + 8), to: &body)
        body.append(png)
    }

    var file = Data()
    appendFourCC("icns", to: &file)
    appendBE32(UInt32(body.count + 8), to: &file)
    file.append(body)
    try file.write(to: url)
}

private func appendFourCC(_ value: String, to data: inout Data) {
    data.append(value.data(using: .ascii)!)
}

private func appendBE32(_ value: UInt32, to data: inout Data) {
    var bigEndian = value.bigEndian
    withUnsafeBytes(of: &bigEndian) { bytes in
        data.append(contentsOf: bytes)
    }
}

private func drawIcon(size: CGFloat) -> NSImage {
    let image = NSImage(size: NSSize(width: size, height: size))
    image.lockFocus()
    defer { image.unlockFocus() }

    let scale = size / 512.0
    NSGraphicsContext.current?.imageInterpolation = .high

    let transform = NSAffineTransform()
    transform.scale(by: scale)
    transform.concat()

    NSColor.clear.setFill()
    NSRect(x: 0, y: 0, width: 512, height: 512).fill()

    let shadow = NSShadow()
    shadow.shadowBlurRadius = 26
    shadow.shadowOffset = NSSize(width: 0, height: -10)
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
    NSGraphicsContext.saveGraphicsState()
    shadow.set()

    let screen = NSBezierPath(roundedRect: NSRect(x: 48, y: 144, width: 416, height: 288), xRadius: 34, yRadius: 34)
    NSGradient(colors: [
        NSColor(calibratedRed: 0.10, green: 0.13, blue: 0.20, alpha: 1),
        NSColor(calibratedRed: 0.04, green: 0.05, blue: 0.07, alpha: 1)
    ])?.draw(in: screen, angle: -42)
    NSGraphicsContext.restoreGraphicsState()

    screen.lineWidth = 10
    NSColor(calibratedRed: 0.31, green: 0.55, blue: 1.00, alpha: 1).setStroke()
    screen.stroke()

    let accent = NSColor(calibratedRed: 0.25, green: 0.44, blue: 0.89, alpha: 1)
    accent.setFill()

    let play = NSBezierPath()
    play.move(to: NSPoint(x: 214, y: 336))
    play.line(to: NSPoint(x: 214, y: 240))
    play.line(to: NSPoint(x: 300, y: 288))
    play.close()
    play.fill()

    NSBezierPath(rect: NSRect(x: 236, y: 104, width: 40, height: 40)).fill()
    NSBezierPath(roundedRect: NSRect(x: 168, y: 82, width: 176, height: 22), xRadius: 11, yRadius: 11).fill()

    return image
}
