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
# Phase setup: A has its original terminal in main and a Pi-labelled Node PTY
# created by Explorer's + menu before the private client was rebuilt/reopened.
run('click',300,363);time.sleep(.7);ta=next(t for t in terms()if t['workspaceId']==a and t['name']=='Pi fixture');pi=ta['id'];initial=layouts()[a];assert initial['focusedPaneId']=='main';assert initial['root']['pane']['tabs']==[m['terminalId']];assert initial['explorer']['pane']['focusedTab']==pi;checks['explorerCreateRetainsMainAndOneOwner']=True;save()
# Hidden panels preserve selection and PTYs, and give input back to main.
run('click',1250,450);start=len(data());run('type','pibefore');assert received(start,tag='pi-qa')==b'pibefore';run('key','Control_L','grave');wait(lambda:not layouts()[a]['explorer']['visible']);run('type','afterhide');assert received(start,tag='main')==b'afterhide';assert len(terms())==2;snap('hidden.png');run('key','Control_L','grave');wait(lambda:layouts()[a]['explorer']['visible']);assert layouts()[a]['explorer']['pane']['focusedTab']==pi;checks['hideShowRetainsPTYAndSelectionAndReturnsInput']=True;save()
# Per-workspace navigation and compatible + catalogs.
run('click',300,455);run('key','Control_L','grave');wait(lambda:b in layouts()and layouts()[b]['explorer']['visible']);assert layouts()[b]['explorer']['pane']['focusedTab']=='files';run('click',1246,117);wait(lambda:layouts()[b]['explorer']['pane']['focusedTab']=='changes_tree');run('click',300,363);assert layouts()[a]['explorer']['pane']['focusedTab']==pi;run('click',300,455);assert layouts()[b]['explorer']['pane']['focusedTab']=='changes_tree';checks['workspaceDockSelectionIsIndependent']=True;save()
run('click',1314,117);text=snap('menu.png');assert 'Browser'not in text;assert 'Pi fixture'in text;run('click',1312,282);wait(lambda:any(t['workspaceId']==b and t['name']=='Grok fixture'for t in terms()));gb=next(t for t in terms()if t['workspaceId']==b and t['name']=='Grok fixture')['id'];wait(lambda:layouts()[b]['explorer']['pane']['focusedTab']==gb);start=len(data());run('type','grokb');assert received(start,terminal=gb)==b'grokb';checks['secondWorkspaceProfileOwnsItsRealPTY']=True;save()
# Delayed create result belongs to A and must not displace B's selection/input.
run('click',300,363);(q/'proxy-control.json').write_text(json.dumps({'delayCreateMs':1000}));run('click',1314,117);run('click',1312,282);run('click',300,455);time.sleep(1.2);ga=next(t for t in terms()if t['workspaceId']==a and t['name']=='Grok fixture')['id'];assert ga in layouts()[a]['explorer']['pane']['tabs'];assert layouts()[a]['explorer']['pane']['focusedTab']==pi;assert layouts()[b]['explorer']['pane']['focusedTab']==gb;start=len(data());run('type','bstay');assert received(start,terminal=gb)==b'bstay';checks['delayedCreateNeverStealsOtherWorkspaceFocus']=True;(q/'proxy-control.json').write_text(json.dumps({'delayCreateMs':0}));snap('workspace-b.png')
# Keyboard New Tab remains a main-workspace action; Explorer's + is explicit.
run('dockwheel',-20);run('click',1246,117);wait(lambda:layouts()[b]['explorer']['pane']['focusedTab']=='changes_tree');run('key','Control_L','t');wait(lambda:any(t.startswith('launcher:')for t in layouts()[b]['root']['pane']['tabs']));assert layouts()[b]['explorer']['pane']['focusedTab']=='changes_tree';checks['globalNewTabKeepsExplorerAndTargetsMain']=True;save()
run('click',300,363);run('drag',1122,500,962,500);wait(lambda:470<layouts()[a]['explorer']['width']<490);snap('wide-dock.png');checks['dockWidthIsIndependentOfSplitTree']=True;save()
(q/'terminal-ids.json').write_text(json.dumps({'a':a,'b':b,'main':m['terminalId'],'pi':pi,'grokA':ga,'grokB':gb},indent=2));save();print(checks)
