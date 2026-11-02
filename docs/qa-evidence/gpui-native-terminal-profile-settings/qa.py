from pathlib import Path
import json,subprocess,time
from PIL import Image
q=Path(__file__).parent;m=json.loads((q/'fixture.json').read_text());checks={};observations={}
def run(*args):subprocess.run(['python3',str(q/'input.py'),*map(str,args)],check=True);time.sleep(.3)
def config():return json.loads(subprocess.run(['node','--import','tsx',str(q/'client.mts'),'config'],capture_output=True,text=True,check=True).stdout)
def patch(profiles):
 file=q/'patch.json';file.write_text(json.dumps({'terminalProfiles':profiles}));subprocess.run(['node','--import','tsx',str(q/'client.mts'),'patch',str(file)],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True);time.sleep(.5)
def wait(predicate):
 for _ in range(40):
  if predicate():return
  time.sleep(.15)
 raise AssertionError('State did not settle')
def snap(name):
 subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-draw_mouse','0','-video_size','1280x800','-f','x11grab','-i',':99+162,100','-frames:v','1','-y',str(q/name)],check=True)
 im=Image.open(q/name);im=im.crop((400,168,880,632))if name in ['edit-before.png','validation.png','conflict.png','removed.png']else im;im.resize((im.width*2,im.height*2)).save(q/(name+'.ocr.png'));text=subprocess.run(['tesseract',str(q/(name+'.ocr.png')),'stdout','--tessdata-dir',str(q),'--psm','6'],capture_output=True,text=True,check=True).stdout;observations[name]=text;return text
(q/'remove-input.py').write_text((q/'input.py').read_text().replace("b'Paseo Native'","b'Remove profile?'"))
def dialog(*args):subprocess.run(['python3',str(q/'remove-input.py'),*map(str,args)],check=True);time.sleep(.3)
def persist():(q/'checks.json').write_text(json.dumps(checks,indent=2));(q/'observations.json').write_text(json.dumps(observations,indent=2))
# Wait for inventory/restore before pressing the empty workspace's profile pencil.
time.sleep(.8);run('key','Escape');run('click',300,455);time.sleep(.5);run('click',1144,556);time.sleep(.7);snap('page.png');run('click',1233,501);assert 'Grokfixture'in ''.join(snap('edit-before.png').split())
run('key','Control_L','a');run('type','Renamed');run('key','Return');run('key','Return');run('key','Return');time.sleep(.6)
c=config();g=next(p for p in c['profiles']if p['id']=='grok-qa');assert g['name']=='Renamed';assert g['args'][-1]=='a b';assert g['opaque']=={'keep':17};checks['renamePreservesOpaqueFieldsAndArgumentBoundaries']=True;snap('renamed.png')
# Return from save restores the page's keyboard focus; reopen and validate cancellation.
run('key','Escape');time.sleep(.3);run('click',1144,556);time.sleep(.4);run('click',1233,501);run('key','Control_L','a');run('key','BackSpace');run('click',910,677);assert 'Name is required'in snap('validation.png');assert next(p for p in config()['profiles']if p['id']=='grok-qa')['name']=='Renamed';run('key','Escape');checks['validationAndCancelDoNotWrite']=True;checks['saveRestoresPageKeyboardFocus']=True
# Add through the three field Enter sequence.
run('click',1282,360);run('type','QA profile');run('key','Return');run('type','echo');run('key','Return');run('type','-n hello');run('key','Return');time.sleep(.5);c=config();added=next(p for p in c['profiles']if p['name']=='QA profile');assert added['args']==['-n','hello'];assert not c['hooks'];checks['addAndEnterFocusProgression']=True
run('click',1142,650);time.sleep(.5);c=config();assert c['profiles'][2]['id']==added['id'];assert c['profiles'][1]['opaque']=={'keep':17};checks['reorderPreservesAllOtherProfileData']=True
run('click',1265,575);dialog('key','Escape');assert len(config()['profiles'])==4;run('click',1265,575);dialog('key','Return');time.sleep(.5);assert all(p['id']!=added['id']for p in config()['profiles']);checks['removeRequiresExplicitConfirmation']=True
# A live unrelated update must not reconstruct an open editor or drop its draft.
run('click',1233,501);run('key','Control_L','a');run('type','Mine');c=config();c['profiles'][0]['name']='External Pi';patch(c['profiles']);run('key','Return');run('key','Return');run('key','Return');time.sleep(.5);c=config();assert c['profiles'][0]['name']=='External Pi';assert c['profiles'][1]['name']=='Mine';assert c['profiles'][1]['args'][-1]=='a b';checks['liveUnrelatedChangesMergeWithoutResettingDraft']=True
run('click',1233,501);run('key','Control_L','a');run('type','Mine2');c=config();c['profiles'][1]['name']='Theirs';patch(c['profiles']);run('key','Return');run('key','Return');run('key','Return');assert 'changed on the host'in snap('conflict.png');assert config()['profiles'][1]['name']=='Theirs';run('key','Escape');checks['observedSameFieldConflictDoesNotOverwriteHost']=True
run('click',1233,501);run('key','Control_L','a');run('type','Mine3');c=config();patch([p for p in c['profiles']if p['id']!='grok-qa']);run('key','Return');run('key','Return');run('key','Return');assert 'removed on the host'in snap('removed.png');assert all(p['id']!='grok-qa'for p in config()['profiles']);run('key','Escape');checks['deletedProfileIsNotResurrected']=True
patch(json.loads((q/'seeded-profiles.json').read_text()));snap('restored.png');persist();print(json.dumps(checks))
