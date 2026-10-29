"""Real WebKit viewport; only visor injects input, JS reports positions."""
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import Gtk, WebKit2

window = Gtk.Window(title="Visor scroll browser")
window.set_default_size(500, 400)
window.move(600, 40)
window.connect("destroy", Gtk.main_quit)
manager = WebKit2.UserContentManager()
manager.connect("script-message-received::position",
                lambda manager, result: print(result.get_js_value().to_string(), flush=True))
assert manager.register_script_message_handler("position")
view = WebKit2.WebView(user_content_manager=manager)
view.get_settings().set_hardware_acceleration_policy(WebKit2.HardwareAccelerationPolicy.NEVER)
window.add(view)
view.load_html("""<!doctype html><meta charset=utf-8>
<style>html {scroll-behavior: auto} body {margin:0}
main {width:4000px;height:4000px;background:repeating-linear-gradient(45deg,#acf,#fff 80px)}
</style><main>Visor browser scroll acceptance</main>
<script>
const report = () => window.webkit.messageHandlers.position.postMessage(
    'POSITION ' + window.scrollX + ' ' + window.scrollY);
// Wait for wheel animation and coalesced events before asserting a position.
let settled;
addEventListener('scroll', () => {
    clearTimeout(settled);
    settled = setTimeout(report, 250);
});
addEventListener('load', () => requestAnimationFrame(() => requestAnimationFrame(report)));
addEventListener('mousemove', () => window.webkit.messageHandlers.position.postMessage('POINTER'), {once:true});
</script>""", "about:blank")
window.show_all()
Gtk.main()
