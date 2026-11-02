import os,json,subprocess,sys,hashlib
from pathlib import Path
q=Path(__file__).parent;m=json.loads((q/'fixture.json').read_text());name=sys.argv[1];env=os.environ.copy();env.pop('WAYLAND_DISPLAY',None);env.update(DISPLAY=':99',PASEO_ENDPOINT=f"ws://127.0.0.1:{m['port']}/ws",PASEO_HOME=m['home'],XDG_CONFIG_HOME=str(q/('config-'+name)))
# Explicit shared visual fixture; isolate application settings without changing GTK's font.
config=q/('config-'+name)
(config/'gtk-3.0').mkdir(parents=True,exist_ok=True)
(config/'gtk-3.0/settings.ini').write_text('[Settings]\ngtk-font-name=Noto Sans 10\n')
(config/'paseo-desktop').mkdir(parents=True,exist_ok=True)
(config/'paseo-desktop/settings.json').write_text('{"theme":"dark","language":"en"}')
p=subprocess.Popen([str(q/name)],env=env,stdout=open(q/(name+'.log'),'w'),stderr=subprocess.STDOUT,start_new_session=True);meta=json.loads((q/'processes.json').read_text());meta['native']={'pid':p.pid,'args':p.args,'sha256':hashlib.file_digest(open(q/name,'rb'),'sha256').hexdigest()};(q/'processes.json').write_text(json.dumps(meta,indent=2));(q/(name+'-identity.json')).write_text(json.dumps(meta['native'],indent=2));print(meta['native'])
