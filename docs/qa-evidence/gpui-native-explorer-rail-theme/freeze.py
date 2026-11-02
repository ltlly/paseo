from pathlib import Path
import json,tarfile,gzip,hashlib
root=Path.cwd();q=Path(__file__).parent;old=json.loads((q/'source-identity.json').read_text());paths=old['paths']
with (q/'source.tar.gz').open('wb')as out,gzip.GzipFile(filename='',fileobj=out,mode='wb',mtime=0)as zipped,tarfile.open(fileobj=zipped,mode='w')as tar:
 for name in paths:
  info=tar.gettarinfo(str(root/name),arcname=name);info.uid=info.gid=0;info.uname=info.gname='';info.mtime=0
  with(root/name).open('rb')as f:tar.addfile(info,f)
sha=hashlib.sha256((q/'source.tar.gz').read_bytes()).hexdigest();(q/'source-identity.json').write_text(json.dumps({'sha256':sha,'files':len(paths),'paths':paths,'fileSha256':{s:hashlib.sha256((root/s).read_bytes()).hexdigest()for s in paths}},indent=2));(q/'remote-id').write_text(sha[:12])
for name in ['check-mac.sh','check-win.ps1']:
 p=q/name;p.write_text(p.read_text().replace(old['sha256'],sha))
print(sha)
