#!/usr/bin/env python3
"""Exercise real terminal/editor/browser input and capture in the private desktop."""
import http.server
import json
from pathlib import Path
import runpy
import subprocess
import sys
import threading
import time
import urllib.parse

UnixHTTP = runpy.run_path('/home/bot/qualification/mcp-discovery.py')['UnixHTTP']
root = Path('/home/bot/workspace/apps')
root.mkdir(exist_ok=True)
profiles = Path('/home/bot/.config/hypermesh/mcp-profiles.json')
backup = profiles.read_bytes() if profiles.exists() else None
processes=[]
sid=None

def request(method, path, body=None, expected=200, companion=False, binary=False):
    client=UnixHTTP('/run/visor/hypermesh/'+('companion.sock' if companion else 'control.sock'))
    try:
        client.request(method,path,None if body is None else json.dumps(body),{'Content-Type':'application/json'})
        result=client.getresponse(); data=result.read()
        assert result.status==expected, f'{method} {path}: {result.status}'
        return data if binary else json.loads(data) if data else None
    finally: client.close()

def until(predicate, message):
    deadline=time.monotonic()+25
    while time.monotonic()<deadline:
        value=predicate()
        if value: return value
        time.sleep(.05)
    raise AssertionError(message)

def launch(argv):
    stream=(root/'applications.log').open('ab')
    process=subprocess.Popen(argv,stdout=stream,stderr=stream)
    stream.close(); processes.append(process)
    return process

def focus(pattern):
    def find():
        result=subprocess.run(['xdotool','search','--onlyvisible','--name',pattern],capture_output=True,text=True)
        return result.stdout.splitlines()
    window=until(find,'window not mapped')[-1]
    subprocess.run(['xdotool','windowactivate','--sync',window],check=True)

def type_text(text): request('POST',f'/session/{sid}/type',{'text':text},204)

posted=threading.Event()
class Browser(http.server.BaseHTTPRequestHandler):
    def log_message(self,*_): pass
    def do_GET(self):
        data=b'<title>Visor browser acceptance</title><h1>Private bot desktop</h1><form method="post"><input autofocus name="q"><button>Send</button></form>'
        self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def do_POST(self):
        data=self.rfile.read(int(self.headers['Content-Length']))
        if urllib.parse.parse_qs(data.decode()).get('q')==['browser-fixture']: posted.set()
        self.send_response(200);self.end_headers();self.wfile.write(b'Accepted')
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Browser)
worker=threading.Thread(target=server.serve_forever);worker.start()
try:
    profiles.write_text(json.dumps({'active':'default','profiles':{'default':{'servers':[]}}}))
    session=request('POST','/session',{'purpose':'container app qualification','recipes':[],'agent':{'name':'qualification','instructions':''},'skills':[]},201)
    sid=session['id']
    request('POST','/companion/automation',{'action':'start'},204,True)
    marker=root/'terminal.txt'
    launch(['xterm','-T','Visor terminal acceptance','-e','python3','-c',f"from pathlib import Path; value=input(); Path({str(marker)!r}).write_text(value); input()"])
    focus('^Visor terminal acceptance$')
    type_text('terminal-fixture\n')
    until(lambda: marker.exists() and marker.read_text()=='terminal-fixture','terminal input did not arrive')
    print('PASS terminal typed-through-visor',flush=True)
    editor=root/'editor-fixture.txt';editor.write_text('')
    launch(['mousepad','--disable-server',str(editor)])
    focus('editor-fixture.txt')
    type_text('editor-fixture')
    request('POST',f'/session/{sid}/type',{'keys':[{'key':'Control','action':'down'},{'key':'s'},{'key':'Control','action':'up'}]},204)
    until(lambda: editor.read_text()=='editor-fixture','editor save did not arrive')
    print('PASS editor typed-and-saved-through-visor',flush=True)
    browser_profile=root/'firefox-profile';browser_profile.mkdir(exist_ok=True)
    (browser_profile/'user.js').write_text('user_pref("browser.shell.checkDefaultBrowser", false);\nuser_pref("browser.startup.homepage_override.mstone", "ignore");\nuser_pref("browser.aboutwelcome.enabled", false);\n')
    launch(['firefox-esr','--no-remote','--profile',str(browser_profile),'--new-window',f'http://127.0.0.1:{server.server_port}/'])
    focus('Visor browser acceptance')
    type_text('browser-fixture\n')
    assert posted.wait(10),'browser form did not receive typed input'
    print('PASS browser typed-and-submitted-through-visor',flush=True)
    image=request('POST',f'/session/{sid}/view',expected=200,binary=True)
    assert image.startswith(b'\x89PNG\r\n\x1a\n') and len(image)>1000
    (root/'view.png').write_bytes(image)
    print('PASS screenshot real-private-desktop',flush=True)
finally:
    try:
        if sid:
            request('POST','/companion/automation',{'action':'stop'},204,True)
            request('DELETE',f'/session/{sid}',expected=204)
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=3)
                except subprocess.TimeoutExpired: process.kill();process.wait()
        server.shutdown();server.server_close();worker.join()
        if backup is None: profiles.unlink(missing_ok=True)
        else: profiles.write_bytes(backup)
