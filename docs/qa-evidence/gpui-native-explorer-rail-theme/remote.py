from pathlib import Path
import subprocess,json,sys,base64
q=Path('/home/ltlly/.cache/paseo-native-qa/rail-20261102');host=sys.argv[1];short=(q/'remote-id').read_text();suffix='sh'if host=='mac'else'ps1';local=q/('check-mac.sh'if host=='mac'else'check-win.ps1');remote='.cache/paseo-gpui-check-'+short+'.'+suffix
subprocess.run(['scp',str(q/'source.tar.gz'),host+':.cache/paseo-gpui-current-'+short+'.tar.gz'],check=True)
subprocess.run(['scp',str(local),host+':'+remote],check=True)
command='bash '+remote if host=='mac'else'powershell.exe -NoProfile -ExecutionPolicy Bypass -EncodedCommand '+base64.b64encode(("& ([Environment]::GetFolderPath('UserProfile')+'/"+remote+"')").encode('utf-16-le')).decode()
p=subprocess.Popen(['ssh',host,command],stdout=open(q/(host+'-build.log'),'w'),stderr=subprocess.STDOUT);(q/(host+'-ssh.json')).write_text(json.dumps({'pid':p.pid,'args':p.args}));raise SystemExit(p.wait())
