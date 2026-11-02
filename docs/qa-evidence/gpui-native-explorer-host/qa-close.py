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

ids=json.loads((q/'terminal-ids.json').read_text());checks=json.loads((q/'checks.json').read_text());frames=json.loads((q/'observations.json').read_text());run('click',300,363);time.sleep(.6);run('click',1157,117);wait(lambda:layouts()[a]['explorer']['pane']['focusedTab']==ids['pi'])
def dialog(key):subprocess.run(['python3',str(q/'dialog-input.py'),'key',key],check=True);time.sleep(.4)
run('middle',1252,117);dialog('Escape');assert len(terms())==4;assert layouts()[a]['explorer']['pane']['focusedTab']==ids['pi'];start=len(data());run('type','cancelpi');assert received(start,tag='pi-qa')==b'cancelpi';checks['cancelCloseKeepsAllPTYsAndExplorerFocus']=True;save();run('middle',1252,117);dialog('Return');wait(lambda:all(t['id']!=ids['grokA']for t in terms()));remaining={t['id']for t in terms()};assert remaining=={ids['main'],ids['pi'],ids['grokB']};assert layouts()[a]['root']['pane']['tabs']==[ids['main']];checks['backgroundCloseKillsOnlyCapturedTerminalID']=True;save();snap('after-background-close.png')
run('click',300,455);run('dockwheel',20);run('click',1230,117);wait(lambda:layouts()[b]['explorer']['pane']['focusedTab']==ids['grokB']);run('middle',1230,117);dialog('Return');wait(lambda:all(t['id']!=ids['grokB']for t in terms()));wait(lambda:layouts()[b]['explorer']['pane']['focusedTab']=='changes_tree');assert {t['id']for t in terms()}=={ids['main'],ids['pi']};checks['selectedCloseRetainsDefaultNavigatorTabs']=True;save();snap('after-selected-close.png');print(checks)
