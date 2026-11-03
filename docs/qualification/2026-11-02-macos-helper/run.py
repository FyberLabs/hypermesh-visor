raise SystemExit("Historical unauthenticated bridge driver disabled; see README.md")
import json, pathlib, subprocess, time, struct, base64
root=pathlib.Path('/tmp/visor-native-qualification')
app=pathlib.Path.home()/'Applications/Hypermesh Visor Development.app'
def call(request):
 d=root/('call-'+str(time.time_ns()));d.mkdir(mode=0o700)
 (d/'request').write_text(json.dumps(request)+'\n')
 subprocess.run(['/usr/bin/open','-g','-n',str(app),'--stdin',str(d/'request'),'--stdout',str(d/'response'),'--stderr',str(d/'error'),'--args','--bridge'],check=True,timeout=20)
 end=time.monotonic()+20
 while time.monotonic()<end:
  if (d/'response').exists() and (d/'response').read_bytes().endswith(b'\n'):break
  time.sleep(.02)
 result=json.loads((d/'response').read_text())
 if 'error' in result: raise RuntimeError(result['error'])
 return result['result']
target=json.loads((root/'target.json').read_text())
info=call({'action':'info'})
assert all(info[k] for k in ['screen_recording','accessibility','input_monitoring']),info
assert info['pid']==target['pid'], 'test window lost focus; refusing input'
text='visor-native-\U0001f642'
strokes=[]
for ch in text:
 k=ord(ch) if ord(ch)<256 else 0x01000000+ord(ch)
 strokes.extend([{'keysym':k,'down':True},{'keysym':k,'down':False}])
call({'action':'keyboard','strokes':strokes})
assert call({'action':'info'})['pid']==target['pid'], 'test window lost focus; refusing click'
call({'action':'pointer','steps':[{'kind':'move','x':target['button_x'],'y':target['button_y']},{'kind':'button','button':0,'down':True},{'kind':'button','button':0,'down':False}]})
end=time.monotonic()+5
while not (root/'input.json').exists() and time.monotonic()<end:time.sleep(.05)
assert json.loads((root/'input.json').read_text())['text']==text
image=base64.b64decode(call({'action':'view'})['png'],validate=True)
assert image[:8]==b'\x89PNG\r\n\x1a\n'
width,height=struct.unpack('>II',image[16:24])
assert (width,height)==(info['width'],info['height'])
report={'permissions':{k:info[k] for k in ['screen_recording','accessibility','input_monitoring']},'checks':['foreground-owned-fixture','unicode-keyboard-roundtrip','pointer-move-and-click','native-screen-capture-png'],'image_dimensions':[width,height],'image_bytes':len(image),'context':'installed development app launched via LaunchServices; direct helper, not daemon or companion acceptance'}
(root/'report.json').write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
