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

time.sleep(.8);run('click',300,363);time.sleep(.8);baseline=Path(m['input']).read_bytes();before=config();run('click',628,153);run('click',680,414);run('click',1233,501);run('key','Tab');run('key','Control_L','a');run('type','echo-tab');run('key','Shift_L','Tab');run('key','Control_L','a');run('type','Focused');snap('tab-focus.png');run('key','Escape');assert config()==before;checks['tabAndShiftTabCancelDoNotSave']=True
run('click',1233,501);snap('native-edit-final.png');run('click',1200,800);snap('backdrop-cancel.png');assert config()==before;checks['backdropCancelsWithoutSaving']=True
assert Path(m['input']).read_bytes()==baseline;checks['settingsKeysNeverReachRetainedPTY']=True
patch([]);assert 'No profiles yet'in snap('empty.png');patch(before['profiles']);checks['liveEmptyAndRestoredCatalog']=True
run('key','Escape');run('type','focusproof');time.sleep(.5)
raw=Path(m['input']).read_bytes()[len(baseline):];import base64
actual=b''.join(base64.b64decode(json.loads(line)['base64'])for line in raw.splitlines());(q/'focus-return-bytes.json').write_text(json.dumps({'expected':'focusproof','actualHex':actual.hex()}));assert actual==b'focusproof';checks['closingSettingsRestoresRetainedPTYInput']=True;persist();print(checks)
