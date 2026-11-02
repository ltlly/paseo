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

run('click',300,363);time.sleep(.5);ids=json.loads((q/'terminal-ids.json').read_text());checks=json.loads((q/'checks.json').read_text());frames=json.loads((q/'observations.json').read_text());file=Path(m['cwd'])/'alpha.rs';file.write_text('seed\n');run('click',1002,117);wait(lambda:layouts()[a]['explorer']['pane']['focusedTab']=='files');run('key','Control_L','p');run('type','alpha');time.sleep(.5);snap('file-search.png');run('key','Return');wait(lambda:any(t.startswith('file:')for t in layouts()[a]['root']['pane']['tabs']));file_id=next(t for t in layouts()[a]['root']['pane']['tabs']if t.startswith('file:'));assert layouts()[a]['explorer']['pane']['focusedTab']=='files';checks['newFileFromExplorerUsesRememberedMainPane']=True;save();snap('file-main.png');stamp=file.stat().st_mtime_ns;run('key','Control_L','End');run('key','Control_L','Home');run('key','Control_L','a');run('key','Control_L','c');run('key','Right');run('key','Control_L','End');time.sleep(1.1);assert file.stat().st_mtime_ns==stamp;assert file.read_text()=='seed\n';checks['editorNavigationAndCopyDoNotQueueSaves']=True;save()
# No per-step compositor delay: move within the 800ms autosave window.
subprocess.run(['python3',str(q/'input.py'),'type','draft'],check=True);assert file.read_text()=='seed\n';subprocess.run(['python3',str(q/'input.py'),'fastdrag','660','153','1300','500'],check=True);wait(lambda:file_id in layouts()[a]['explorer']['pane']['tabs']);unsaved_after_move=file.read_text();assert unsaved_after_move=='seed\n';wait(lambda:file.read_text()=='seed\ndraft');assert file_id not in layouts()[a]['root']['pane']['tabs'];checks['movingDirtyFileRetainsUnwrittenBufferAndAutosave']=True;(q/'file-move-proof.json').write_text(json.dumps({'file':str(file),'unwrittenAtDrop':unsaved_after_move,'afterAutosave':file.read_text(),'tabId':file_id},indent=2));save();snap('file-explorer.png');run('dockwheel',20);run('hover',1279,117);snap('file-close-hover.png');print(checks)
