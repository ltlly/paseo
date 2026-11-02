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

ids=json.loads((q/'terminal-ids.json').read_text());checks=json.loads((q/'checks.json').read_text());frames=json.loads((q/'observations.json').read_text());run('click',300,363);time.sleep(.6);assert ids['grokA']in layouts()[a]['root']['pane']['tabs']
run('key','Control_L','grave');wait(lambda:not layouts()[a]['explorer']['visible']);before=layouts()[a];run('drag',690,153,1250,500);after=layouts()[a];assert after['root']['pane']['tabs']==before['root']['pane']['tabs'];assert not after['explorer']['visible'];checks['hiddenDockNeverParticipatesInDropHitTesting']=True;save();snap('hidden-drop-fixed.png');run('key','Control_L','grave');wait(lambda:layouts()[a]['explorer']['visible'])
Path(m['mode']).write_text('\x1b[?1000h\x1b[?1003h\x1b[?1006h');time.sleep(.8)
try:
 start=len(data());run('click',650,400);run('hover',660,410);run('click',670,420);positive=data()[start:];assert b'\x1b[<'in received(start);(q/'mouse-positive-after.json').write_text(json.dumps(positive,indent=2));checks['mouseReportingHasRealPTYPositiveControl']=True;save()
 run('hover',690,153);start=len(data());run('drag',690,153,1300,500);wait(lambda:ids['grokA']in layouts()[a]['explorer']['pane']['tabs']);assert received(start)==b'';checks['capturedDragDoesNotSendTerminalMouseReports']=True;save();snap('mouse-drag-fixed.png')
 run('hover',1252,117);start=len(data());before=layouts()[a];run('draghold',1252,117,700,350);run('wheel',3);run('key','Escape');run('release');assert layouts()[a]==before;assert received(start)==b'';checks['dragScrollAndEscapeStayOutOfPTY']=True;save();snap('drag-cancelled.png')
finally:
 Path(m['mode']).write_text('\x1b[?1000l\x1b[?1003l\x1b[?1006l');time.sleep(.4)
run('click',1157,117);wait(lambda:layouts()[a]['explorer']['pane']['focusedTab']==ids['pi']);(q/'native-layout-visual.json').write_text(json.dumps(layouts()[a],indent=2));snap('native-dock-final.png');save();print(checks)
