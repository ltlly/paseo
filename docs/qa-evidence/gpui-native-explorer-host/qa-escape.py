from pathlib import Path
import json,subprocess,time,base64
from PIL import Image
q=Path(__file__).parent;m=json.loads((q/'fixture.json').read_text());extra=json.loads((q/'extra.json').read_text());a=m['workspaceId'];b=extra['secondWorkspace'];checks={};frames={}
def run(*args):
 subprocess.run(['python3',str(q/'input.py'),*map(str,args)],check=True);time.sleep(.35)
def layouts():return {i['workspaceId']:i['layout']for i in json.loads((Path(m['home'])/'native-workspace-layouts.json').read_text())['layouts']}
def terms():return json.loads(subprocess.check_output(['node','--import','tsx',str(q/'client.mts'),'list'],text=True))['terminals']
def wait(fn):
 for _ in range(40):
  if fn():return
  time.sleep(.15)
 raise AssertionError('State did not settle')
def snap(name):
 subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-draw_mouse','0','-video_size','1280x800','-f','x11grab','-i',':99+162,100','-frames:v','1','-y',str(q/name)],check=True)
 im=Image.open(q/name);im=im.crop((1072,35,1272,268))if name=='menu.png'else im;im.resize((im.width*2,im.height*2)).save(q/(name+'.ocr.png'));text=subprocess.check_output(['tesseract',str(q/(name+'.ocr.png')),'stdout','--tessdata-dir',str(q),'--psm','6'],stderr=subprocess.DEVNULL,text=True);frames[name]=text;return text
def data():return [json.loads(line)for line in Path(m['input']).read_text().splitlines()]
def received(start,terminal=None,tag=None):return b''.join(base64.b64decode(i['base64'])for i in data()[start:]if(terminal is None or i.get('terminalId')==terminal)and(tag is None or i.get('tag')==tag))
def save():
 (q/'checks.json').write_text(json.dumps(checks,indent=2));(q/'observations.json').write_text(json.dumps(frames,indent=2))

run('click',300,363);time.sleep(.5);ids=json.loads((q/'terminal-ids.json').read_text());checks=json.loads((q/'checks.json').read_text());frames=json.loads((q/'observations.json').read_text());run('drag',690,153,1300,500);wait(lambda:ids['grokA']in layouts()[a]['explorer']['pane']['tabs']);Path(m['mode']).write_text('\x1b[?1000h\x1b[?1003h\x1b[?1006h');time.sleep(.8)
try:
 run('hover',1252,117);before=layouts()[a];start=len(data());run('dragescape',1252,117,700,350);after=layouts()[a];proof={'sameLayout':after==before,'before':before,'after':after,'unexpectedInputHex':received(start).hex()};(q/'drag-escape-proof.json').write_text(json.dumps(proof,indent=2));assert after==before;assert received(start)==b'';checks['dragScrollAndEscapeStayOutOfPTY']=True;save();snap('drag-cancelled.png')
finally:Path(m['mode']).write_text('\x1b[?1000l\x1b[?1003l\x1b[?1006l');time.sleep(.4)
run('click',1157,117);wait(lambda:layouts()[a]['explorer']['pane']['focusedTab']==ids['pi']);(q/'native-layout-visual.json').write_text(json.dumps(layouts()[a],indent=2));snap('native-dock-final.png');save();print(checks)
