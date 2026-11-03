import AppKit
final class Fixture: NSObject, NSApplicationDelegate {
 var window: NSWindow!
 var field: NSTextField!
 let output = ProcessInfo.processInfo.environment["VISOR_FIXTURE_DIR"] ?? "/tmp/visor-native-qualification"
 func save(_ name: String, _ data: [String:Any]) {
  try! JSONSerialization.data(withJSONObject:data, options:[.sortedKeys]).write(to: URL(fileURLWithPath:output+"/"+name), options:.atomic)
 }
 func applicationDidFinishLaunching(_ note: Notification) {
  window = NSWindow(contentRect:NSRect(x:0,y:0,width:540,height:220),styleMask:[.titled,.closable],backing:.buffered,defer:false)
  window.title="Visor disposable input test"
  let label=NSTextField(labelWithString:"Disposable qualification window — no user documents")
  label.frame=NSRect(x:20,y:165,width:500,height:24); window.contentView!.addSubview(label)
  field=NSTextField(frame:NSRect(x:20,y:100,width:500,height:32));window.contentView!.addSubview(field)
  let button=NSButton(title:"Record test input",target:self,action:#selector(record))
  button.frame=NSRect(x:20,y:40,width:180,height:32);window.contentView!.addSubview(button)
  window.center();window.makeKeyAndOrderFront(nil);window.makeFirstResponder(field)
  NSApp.activate(ignoringOtherApps:true)
  let rect=window.convertToScreen(button.convert(button.bounds,to:nil))
  let height=CGDisplayBounds(CGMainDisplayID()).height
  save("target.json",["pid":ProcessInfo.processInfo.processIdentifier,"button_x":Int(rect.midX),"button_y":Int(height-rect.midY)])
 }
 @objc func record(){save("input.json",["text":field.stringValue])}
}
let app=NSApplication.shared
let delegate=Fixture();app.delegate=delegate;app.setActivationPolicy(.regular);app.run()
