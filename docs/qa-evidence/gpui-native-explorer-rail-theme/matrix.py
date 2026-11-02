from pathlib import Path
import os,json,subprocess,time,sys,hashlib
q=Path(__file__).parent;m=json.loads((q/'fixture.json').read_text());name=sys.argv[1];theme=sys.argv[2]
# Reset only this private client's saved layout between matched process captures.
p=Path(m['home'])/'native-workspace-layouts.json';saved=json.loads(p.read_text());saved['layouts'][0]['layout']=json.loads((q/'layout-seed.json').read_text());p.write_text(json.dumps(saved))
env=os.environ.copy();env['QA_THEME']=theme;subprocess.run(['python3',str(q/'launch.py'),name],env=env,check=True);meta=json.loads((q/'processes.json').read_text())['native'];time.sleep(2)
def run(*a):subprocess.run(['python3',str(q/'input.py'),*map(str,a)],check=True);time.sleep(.18)
def snap(case):
 subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-draw_mouse','0','-video_size','1280x800','-f','x11grab','-i',':99+162,100','-frames:v','1','-y',str(q/f'{name}-{theme}-{case}.png')],check=True)
run('click',300,363);time.sleep(.5);run('hover',1250,800);time.sleep(.5);snap('fit-idle');run('hover',1000,117);snap('fit-tab-hover');run('hover',1312,117);snap('fit-plus-hover');run('click',1312,117);run('hover',1250,800);snap('fit-menu');run('key','Escape');run('drag',962,260,1122,260);run('hover',1250,800);snap('overflow-idle');run('hover',1319,117);snap('overflow-plus-hover');run('click',1319,117);run('hover',1250,800);snap('overflow-menu');run('key','Escape')
layout=json.loads(p.read_text())['layouts'][0]['layout'];assert abs(layout['explorer']['width']-320)<1,layout
run('close')
for _ in range(50):
 try:
  if Path(f"/proc/{meta['pid']}/stat").read_text().split()[2]=='Z':break
 except FileNotFoundError:break
 time.sleep(.1)
else:raise AssertionError('Client did not close')
ids=json.loads((q/'terminal-ids.json').read_text());terms=json.loads(subprocess.check_output(['node','--import','tsx',str(q/'client.mts'),'list'],env={**os.environ,'PASEO_NATIVE_QA':str(q)},text=True))['terminals'];assert {t['id']for t in terms}=={ids['main'],ids['pi'],ids['grokA']};(q/f'{name}-{theme}-result.json').write_text(json.dumps({'binary':meta,'theme':theme,'sameTerminalIdsAfterClientClose':True,'layout':layout},indent=2));print(name,theme,'passed')
