import AppKit
import WebKit

final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate {
    private let appName = "TV OS"
    private var window: NSWindow!
    private var webView: WKWebView!
    private var daemon: Process?
    private var daemonPort: UInt16 = 8484
    private var startupDeadline = Date()

    private var baseURL: URL {
        URL(string: "http://127.0.0.1:\(daemonPort)")!
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        buildMenu()
        buildWindow()
        startDaemon()
        waitForDaemon()
    }

    func applicationWillTerminate(_ notification: Notification) {
        stopDaemon()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        window?.makeKeyAndOrderFront(nil)
    }

    private func buildWindow() {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .default()
        config.preferences.javaScriptCanOpenWindowsAutomatically = true
        config.defaultWebpagePreferences.allowsContentJavaScript = true

        webView = WKWebView(frame: .zero, configuration: config)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = false

        let screenFrame = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
        let width = min(max(screenFrame.width * 0.82, 1120), 1720)
        let height = min(max(screenFrame.height * 0.82, 720), 1040)
        let frame = NSRect(
            x: screenFrame.midX - width / 2,
            y: screenFrame.midY - height / 2,
            width: width,
            height: height
        )

        window = NSWindow(
            contentRect: frame,
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = appName
        window.titlebarAppearsTransparent = true
        window.backgroundColor = .black
        window.isMovableByWindowBackground = true
        window.minSize = NSSize(width: 960, height: 600)
        window.delegate = self
        window.contentView = webView
        window.makeKeyAndOrderFront(nil)

        showStartupPage(message: "Starting TV OS")
    }

    func windowDidChangeOcclusionState(_ notification: Notification) {
        // A hidden/minimized WebKit view does not need to composite the TV UI.
        // Unhiding on visibility restores the existing page and focus state.
        webView?.isHidden = !window.occlusionState.contains(.visible)
    }

    func windowDidDeminiaturize(_ notification: Notification) {
        webView?.isHidden = false
    }

    private func buildMenu() {
        let mainMenu = NSMenu()

        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(NSMenuItem(title: "About \(appName)", action: #selector(showAbout), keyEquivalent: ""))
        appMenu.addItem(.separator())
        appMenu.addItem(NSMenuItem(title: "Quit \(appName)", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        appItem.submenu = appMenu
        mainMenu.addItem(appItem)

        let viewItem = NSMenuItem()
        let viewMenu = NSMenu(title: "View")
        viewMenu.addItem(NSMenuItem(title: "Reload", action: #selector(reload), keyEquivalent: "r"))
        viewMenu.addItem(NSMenuItem(title: "Actual Size", action: #selector(actualSize), keyEquivalent: "0"))
        viewMenu.addItem(NSMenuItem(title: "Enter Full Screen", action: #selector(toggleFullScreen), keyEquivalent: "f"))
        viewItem.submenu = viewMenu
        mainMenu.addItem(viewItem)

        NSApp.mainMenu = mainMenu
    }

    private func startDaemon() {
        guard let resources = Bundle.main.resourceURL else {
            showFatalPage("Could not find bundled resources.")
            return
        }
        let daemonURL = resources.appendingPathComponent("tvosd")
        guard FileManager.default.isExecutableFile(atPath: daemonURL.path) else {
            showFatalPage("The bundled tvosd helper is missing or not executable.")
            return
        }

        daemonPort = freeLoopbackPort() ?? 8484

        let process = Process()
        process.executableURL = daemonURL
        process.currentDirectoryURL = resources
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        var env = ProcessInfo.processInfo.environment
        let profileDirectory = tvosProfileDirectory()
        try? FileManager.default.createDirectory(at: profileDirectory, withIntermediateDirectories: true)
        importExistingProfileIfNeeded(to: profileDirectory)
        env["HOME"] = NSHomeDirectory()
        env["TVOS_LISTEN"] = "127.0.0.1:\(daemonPort)"
        env["TVOS_UI_DIR"] = resources.appendingPathComponent("ui").path
        env["TVOS_CONFIG_DIR"] = profileDirectory.path
        env["TVOS_PROFILE_DIR"] = profileDirectory.path
        env["TVOS_MPV"] = env["TVOS_MPV"] ?? defaultVideoPlayerPath()
        env["TVOS_MAC_APP"] = "1"
        env["TVOS_BACKGROUND_WARMUPS"] = env["TVOS_BACKGROUND_WARMUPS"] ?? "0"
        env["PATH"] = mergedPath(from: env["PATH"], profileDirectory: profileDirectory)
        process.environment = env

        do {
            try process.run()
            daemon = process
            startupDeadline = Date().addingTimeInterval(24)
        } catch {
            showFatalPage("Could not start tvosd: \(error.localizedDescription)")
        }
    }

    private func waitForDaemon() {
        guard daemon?.isRunning == true else {
            showFatalPage("tvosd exited before the app was ready.")
            return
        }

        var request = URLRequest(url: baseURL.appendingPathComponent("api/version"))
        request.timeoutInterval = 0.8

        URLSession.shared.dataTask(with: request) { [weak self] _, response, _ in
            guard let self else { return }
            let ok = (response as? HTTPURLResponse)?.statusCode == 200
            DispatchQueue.main.async {
                if ok {
                    self.webView.load(URLRequest(url: self.baseURL))
                } else if Date() < self.startupDeadline {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
                        self.waitForDaemon()
                    }
                } else {
                    self.showFatalPage("tvosd did not become ready in time.")
                }
            }
        }.resume()
    }

    private func stopDaemon() {
        guard let daemon else { return }
        if daemon.isRunning {
            daemon.terminate()
        }
        self.daemon = nil
    }

    private func showStartupPage(message: String) {
        webView.loadHTMLString(statusHTML(title: message, body: "Preparing your library and interface."), baseURL: nil)
    }

    private func showFatalPage(_ message: String) {
        webView?.loadHTMLString(statusHTML(title: "TV OS could not start", body: message), baseURL: nil)
    }

    private func statusHTML(title: String, body: String) -> String {
        let escapedTitle = title.htmlEscaped
        let escapedBody = body.htmlEscaped
        return """
        <!doctype html>
        <html>
        <head>
          <meta charset="utf-8">
          <meta name="viewport" content="width=device-width, initial-scale=1">
          <style>
            :root { color-scheme: dark; }
            html, body { height: 100%; margin: 0; background: #080a10; color: #f5f7fb; font-family: -apple-system, BlinkMacSystemFont, "Inter", "Helvetica Neue", sans-serif; }
            body { display: grid; place-items: center; }
            main { width: min(560px, calc(100vw - 48px)); text-align: center; }
            h1 { margin: 0 0 12px; font-size: 28px; font-weight: 700; letter-spacing: 0; }
            p { margin: 0; color: #a8b0c3; font-size: 15px; line-height: 1.5; }
            .pulse { width: 44px; height: 44px; margin: 0 auto 24px; border-radius: 14px; background: linear-gradient(135deg, #5a96ff, #3f6fe4); box-shadow: 0 0 40px rgba(90,150,255,.34); animation: pulse 1.4s ease-in-out infinite; }
            @keyframes pulse { 0%, 100% { transform: scale(.92); opacity: .76; } 50% { transform: scale(1); opacity: 1; } }
          </style>
        </head>
        <body><main><div class="pulse"></div><h1>\(escapedTitle)</h1><p>\(escapedBody)</p></main></body>
        </html>
        """
    }

    @objc private func reload() {
        webView.reloadFromOrigin()
    }

    @objc private func actualSize() {
        webView.pageZoom = 1.0
    }

    @objc private func toggleFullScreen() {
        window.toggleFullScreen(nil)
    }

    @objc private func showAbout() {
        NSApp.orderFrontStandardAboutPanel(options: [
            .applicationName: appName,
            .applicationVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
            .version: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        ])
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if navigationAction.targetFrame == nil, let url = navigationAction.request.url {
            NSWorkspace.shared.open(url)
        }
        return nil
    }
}

private func mergedPath(from existing: String?, profileDirectory: URL) -> String {
    let preferred = [
        profileDirectory.appendingPathComponent("node_modules/.bin").path,
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin"
    ]
    let current = existing?.split(separator: ":").map(String.init) ?? []
    let merged = preferred + current.filter { !preferred.contains($0) }
    return merged.joined(separator: ":")
}

private func tvosProfileDirectory() -> URL {
    let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
        ?? URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Application Support", isDirectory: true)
    return base.appendingPathComponent("TV OS", isDirectory: true)
}

private func importExistingProfileIfNeeded(to destination: URL) {
    let marker = destination.appendingPathComponent(".portable-profile-imported")
    let importedKeys = ["settings.json", "addons.json", "cloudstream.json", "events.jsonl", "resume.json"]
    guard let sourceConfig = portableConfigDirectory() else {
        return
    }

    let fileManager = FileManager.default
    try? fileManager.createDirectory(at: destination, withIntermediateDirectories: true)

    let items = importedKeys + ["positions"]
    var didImport = false
    for item in items {
        let source = sourceConfig.appendingPathComponent(item)
        let target = destination.appendingPathComponent(item)
        guard fileManager.fileExists(atPath: source.path) else {
            continue
        }
        let shouldReplaceSmallResume = item == "resume.json"
            && fileSize(source) > fileSize(target)
            && fileManager.fileExists(atPath: marker.path)
        guard !fileManager.fileExists(atPath: target.path) || shouldReplaceSmallResume else {
            continue
        }
        if shouldReplaceSmallResume {
            try? fileManager.removeItem(at: target)
        }
        do {
            try fileManager.copyItem(at: source, to: target)
            didImport = true
        } catch {
            NSLog("TV OS profile import skipped \(item): \(error.localizedDescription)")
        }
    }

    if didImport {
        let note = "Imported from \(sourceConfig.path)\n"
        try? note.write(to: marker, atomically: true, encoding: .utf8)
    }
}

private func fileSize(_ url: URL) -> UInt64 {
    let attrs = try? FileManager.default.attributesOfItem(atPath: url.path)
    return attrs?[.size] as? UInt64 ?? 0
}

private func portableConfigDirectory() -> URL? {
    let fileManager = FileManager.default
    var candidates: [URL] = []

    if let explicit = ProcessInfo.processInfo.environment["TVOS_IMPORT_PROFILE_DIR"], !explicit.isEmpty {
        candidates.append(URL(fileURLWithPath: explicit).appendingPathComponent("config", isDirectory: true))
        candidates.append(URL(fileURLWithPath: explicit, isDirectory: true))
    }

    if let bundleURL = Bundle.main.bundleURL as URL? {
        var cursor = bundleURL
        for _ in 0..<8 {
            candidates.append(cursor.appendingPathComponent(".tvos/profile/config", isDirectory: true))
            cursor.deleteLastPathComponent()
        }
    }

    candidates.append(URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".config/tvos", isDirectory: true))

    return candidates.first { candidate in
        let settings = candidate.appendingPathComponent("settings.json").path
        let addons = candidate.appendingPathComponent("addons.json").path
        let events = candidate.appendingPathComponent("events.jsonl").path
        return fileManager.fileExists(atPath: settings)
            || fileManager.fileExists(atPath: addons)
            || fileManager.fileExists(atPath: events)
    }
}

private func defaultVideoPlayerPath() -> String {
    for candidate in ["/opt/homebrew/bin/mpv", "/usr/local/bin/mpv", "/usr/bin/mpv"] {
        if FileManager.default.isExecutableFile(atPath: candidate) {
            return candidate
        }
    }
    return "mpv"
}

private func freeLoopbackPort() -> UInt16? {
    let fd = socket(AF_INET, SOCK_STREAM, 0)
    guard fd >= 0 else { return nil }
    defer { close(fd) }

    var reuse: Int32 = 1
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

    var addr = sockaddr_in()
    addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    addr.sin_family = sa_family_t(AF_INET)
    addr.sin_port = in_port_t(0).bigEndian
    addr.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))

    let bound = withUnsafePointer(to: &addr) { ptr -> Int32 in
        ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
        }
    }
    guard bound == 0 else { return nil }

    var len = socklen_t(MemoryLayout<sockaddr_in>.size)
    let named = withUnsafeMutablePointer(to: &addr) { ptr -> Int32 in
        ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            getsockname(fd, $0, &len)
        }
    }
    guard named == 0 else { return nil }
    return UInt16(bigEndian: addr.sin_port)
}

private extension String {
    var htmlEscaped: String {
        var text = self
        text = text.replacingOccurrences(of: "&", with: "&amp;")
        text = text.replacingOccurrences(of: "<", with: "&lt;")
        text = text.replacingOccurrences(of: ">", with: "&gt;")
        text = text.replacingOccurrences(of: "\"", with: "&quot;")
        text = text.replacingOccurrences(of: "'", with: "&#39;")
        return text
    }
}
