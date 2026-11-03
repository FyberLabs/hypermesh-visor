import AppKit
import CoreGraphics
import ScreenCaptureKit

let injectionTag: Int64 = 0x48595045524d4553
let source = CGEventSource(stateID: .privateState)!
var heldKeys = Set<CGKeyCode>()
var heldButtons = Set<UInt32>()

func post(_ event: CGEvent?) throws {
    guard CGPreflightPostEventAccess(), let event else { throw NativeError.denied }
    event.setIntegerValueField(.eventSourceUserData, value: injectionTag)
    event.post(tap: .cghidEventTap)
}
enum NativeError: Error { case denied, invalid, unavailable }
func integer(_ value: Any?) throws -> Int {
    guard let number = value as? NSNumber else { throw NativeError.invalid }
    return number.intValue
}
func pointer(_ step: [String: Any]) throws {
    let point = CGEvent(source: nil)?.location ?? .zero
    switch step["kind"] as? String {
    case "move":
        let x = try integer(step["x"]), y = try integer(step["y"])
        let bounds = CGDisplayBounds(CGMainDisplayID())
        guard x >= 0, y >= 0, Double(x) < bounds.width, Double(y) < bounds.height else { throw NativeError.invalid }
        let type: CGEventType = heldButtons.contains(0) ? .leftMouseDragged : heldButtons.contains(1) ? .rightMouseDragged : .mouseMoved
        try post(CGEvent(mouseEventSource: source, mouseType: type,
                         mouseCursorPosition: CGPoint(x: bounds.minX + Double(x), y: bounds.minY + Double(y)), mouseButton: .left))
    case "button":
        let button = try integer(step["button"])
        guard (0...2).contains(button), let down = step["down"] as? Bool else { throw NativeError.invalid }
        let types: [(CGEventType, CGEventType)] = [(.leftMouseDown,.leftMouseUp),(.rightMouseDown,.rightMouseUp),(.otherMouseDown,.otherMouseUp)]
        try post(CGEvent(mouseEventSource: source, mouseType: down ? types[button].0 : types[button].1,
                         mouseCursorPosition: point, mouseButton: CGMouseButton(rawValue: UInt32(button))!))
        if down { heldButtons.insert(UInt32(button)) } else { heldButtons.remove(UInt32(button)) }
    case "scroll":
        let direction = step["direction"] as? String
        guard ["up","down","left","right"].contains(direction ?? "") else { throw NativeError.invalid }
        let dy: Int32 = direction == "up" ? 1 : direction == "down" ? -1 : 0
        let dx: Int32 = direction == "left" ? 1 : direction == "right" ? -1 : 0
        try post(CGEvent(scrollWheelEvent2Source: source, units: .line, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0))
    default: throw NativeError.invalid
    }
}
let namedKeys: [UInt32: CGKeyCode] = [0xff0d:36,0xff09:48,0xff08:51,0xff1b:53,0xff51:123,0xff52:126,0xff53:124,0xff54:125,
    0xffff:117,0xff50:115,0xff57:119,0xff55:116,0xff56:121,0xffe1:56,0xffe3:59,0xffe9:58,0xffeb:55,
    0xffbe:122,0xffbf:120,0xffc0:99,0xffc1:118,0xffc2:96,0xffc3:97,0xffc4:98,0xffc5:100,0xffc6:101,0xffc7:109,0xffc8:103,0xffc9:111]
let letterCodes: [Character: CGKeyCode] = ["a":0,"s":1,"d":2,"f":3,"h":4,"g":5,"z":6,"x":7,"c":8,"v":9,"b":11,"q":12,"w":13,"e":14,"r":15,"y":16,"t":17,"o":31,"u":32,"i":34,"p":35,"l":37,"j":38,"k":40,"n":45,"m":46]
func key(_ stroke: [String: Any]) throws {
    let raw = try integer(stroke["keysym"])
    guard raw >= 0, raw <= 0x10ffffff, let down = stroke["down"] as? Bool else { throw NativeError.invalid }
    let keysym = UInt32(raw)
    var code: CGKeyCode
    var text: String? = nil
    if let named = namedKeys[keysym] { code = named }
    else {
        let scalarValue = keysym >= 0x01000000 ? keysym & 0x00ffffff : keysym
        guard let scalar = UnicodeScalar(scalarValue), (keysym < 0xff00 || keysym >= 0x01000000) else { throw NativeError.invalid }
        text = String(scalar)
        code = text!.lowercased().first.flatMap { letterCodes[$0] } ?? 0
    }
    guard let event = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down) else { throw NativeError.unavailable }
    var flags: CGEventFlags = []
    let next = down ? heldKeys.union([code]) : heldKeys.subtracting([code])
    if next.contains(56) { flags.insert(.maskShift) }
    if next.contains(59) { flags.insert(.maskControl) }
    if next.contains(58) { flags.insert(.maskAlternate) }
    if next.contains(55) { flags.insert(.maskCommand) }
    event.flags = flags
    if let text {
        let utf16 = Array(text.utf16)
        event.keyboardSetUnicodeString(stringLength: utf16.count, unicodeString: utf16)
    }
    try post(event)
    heldKeys = next
}
func releaseHeld() {
    for code in heldKeys { try? post(CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false)) }
    for button in heldButtons {
        let types: [CGEventType] = [.leftMouseUp,.rightMouseUp,.otherMouseUp]
        try? post(CGEvent(mouseEventSource: source, mouseType: types[Int(button)], mouseCursorPosition: CGEvent(source:nil)?.location ?? .zero, mouseButton: CGMouseButton(rawValue: button)!))
    }
    heldKeys.removeAll(); heldButtons.removeAll()
}
func handle(_ request: [String: Any]) async throws -> [String: Any] {
    switch request["action"] as? String {
    case "info":
        let bounds = CGDisplayBounds(CGMainDisplayID())
        let point = CGEvent(source:nil)?.location ?? .zero
        var result: [String: Any] = ["width":Int(bounds.width),"height":Int(bounds.height),
            "x":Int(point.x-bounds.minX),"y":Int(point.y-bounds.minY),
            "screen_recording":CGPreflightScreenCaptureAccess(),"accessibility":CGPreflightPostEventAccess(),"input_monitoring":CGPreflightListenEventAccess()]
        if let app = NSWorkspace.shared.frontmostApplication, !app.isTerminated {
            result["pid"] = app.processIdentifier
            if let id = app.bundleIdentifier { result["app_id"] = id }
        }
        return result
    case "view":
        guard CGPreflightScreenCaptureAccess() else { throw NativeError.denied }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first(where: {$0.displayID == CGMainDisplayID()}) else { throw NativeError.unavailable }
        let bounds = CGDisplayBounds(display.displayID)
        let configuration = SCStreamConfiguration()
        configuration.width = Int(bounds.width); configuration.height = Int(bounds.height)
        configuration.showsCursor = true
        let image = try await SCScreenshotManager.captureImage(contentFilter: SCContentFilter(display:display,excludingWindows:[]), configuration:configuration)
        guard let png = NSBitmapImageRep(cgImage:image).representation(using:.png,properties:[:]) else { throw NativeError.unavailable }
        return ["png":png.base64EncodedString()]
    case "pointer":
        guard CGPreflightPostEventAccess(), CGPreflightListenEventAccess(), let steps = request["steps"] as? [[String:Any]], steps.count <= 4096 else { throw NativeError.denied }
        for step in steps { try pointer(step) }
        return [:]
    case "keyboard":
        guard CGPreflightPostEventAccess(), CGPreflightListenEventAccess(), let strokes = request["strokes"] as? [[String:Any]], strokes.count <= 4096 else { throw NativeError.denied }
        for stroke in strokes { try key(stroke) }
        return [:]
    default: throw NativeError.invalid
    }
}
@MainActor final class SetupController: NSObject {
    var window: NSWindow!
    var status: NSTextField!
    func show() {
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 280),
                          styleMask: [.titled, .closable, .miniaturizable], backing: .buffered, defer: false)
        window.title = "Hypermesh Visor Development — Permissions"
        window.isReleasedWhenClosed = false
        let content = window.contentView!
        let intro = NSTextField(wrappingLabelWithString: "Development helper for native desktop testing. This is not the finished Visor app. Grant only the permissions you want to test.")
        intro.frame = NSRect(x: 24, y: 205, width: 552, height: 52)
        content.addSubview(intro)
        status = NSTextField(wrappingLabelWithString: "")
        status.frame = NSRect(x: 24, y: 133, width: 552, height: 60)
        content.addSubview(status)
        for (index, pair) in [("Screen Recording", #selector(screen)), ("Accessibility", #selector(accessibility)), ("Input Monitoring", #selector(input))].enumerated() {
            let button = NSButton(title: pair.0, target: self, action: pair.1)
            button.frame = NSRect(x: 24 + index * 185, y: 85, width: 175, height: 32)
            content.addSubview(button)
        }
        let refreshButton = NSButton(title: "Refresh Status", target: self, action: #selector(refresh))
        refreshButton.frame = NSRect(x: 24, y: 30, width: 150, height: 32)
        content.addSubview(refreshButton)
        let quitButton = NSButton(title: "Quit", target: NSApplication.shared, action: #selector(NSApplication.terminate(_:)))
        quitButton.frame = NSRect(x: 470, y: 30, width: 100, height: 32)
        content.addSubview(quitButton)
        refresh()
        window.center(); window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }
    @objc func refresh() {
        status.stringValue = "Screen Recording: \(CGPreflightScreenCaptureAccess() ? "allowed" : "not allowed")\nAccessibility: \(CGPreflightPostEventAccess() ? "allowed" : "not allowed")\nInput Monitoring: \(CGPreflightListenEventAccess() ? "allowed" : "not allowed")"
    }
    func settings(_ pane: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?" + pane) {
            NSWorkspace.shared.open(url)
        }
        refresh()
    }
    @objc func screen() { _ = CGRequestScreenCaptureAccess(); settings("Privacy_ScreenCapture") }
    @objc func accessibility() { _ = CGRequestPostEventAccess(); settings("Privacy_Accessibility") }
    @objc func input() { _ = CGRequestListenEventAccess(); settings("Privacy_ListenEvent") }
}
@MainActor func runSetup() {
    let app = NSApplication.shared
    app.setActivationPolicy(.regular)
    let controller = SetupController()
    controller.show()
    withExtendedLifetime(controller) { app.run() }
}
@main struct NativeDesktop {
    @MainActor static func main() async {
        if CommandLine.arguments.contains("--setup") || (Bundle.main.bundleIdentifier == "sh.hyperme.visor.development" && isatty(STDIN_FILENO) == 0 && CommandLine.arguments.count == 1 && ProcessInfo.processInfo.environment["HYPERMESH_NATIVE_BRIDGE"] != "1") {
            runSetup()
            return
        }
        if CommandLine.arguments.contains("--permissions") {
            _ = CGRequestScreenCaptureAccess(); _ = CGRequestPostEventAccess(); _ = CGRequestListenEventAccess()
            return
        }
        defer { releaseHeld() }
        while let line = readLine() {
            var reply: [String:Any]
            do {
                guard line.utf8.count <= 1024*1024, let data = line.data(using:.utf8), let request = try JSONSerialization.jsonObject(with:data) as? [String:Any] else { throw NativeError.invalid }
                reply = ["result":try await handle(request)]
            } catch NativeError.denied { releaseHeld(); reply = ["error":"Grant Screen Recording, Accessibility and Input Monitoring as required, then restart Visor."] }
            catch { releaseHeld(); reply = ["error":"Native desktop operation unavailable or invalid."] }
            let data = try! JSONSerialization.data(withJSONObject:reply,options:[.sortedKeys])
            FileHandle.standardOutput.write(data); FileHandle.standardOutput.write(Data([10]))
        }
    }
}
