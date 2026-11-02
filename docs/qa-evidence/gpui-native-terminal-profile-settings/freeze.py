from pathlib import Path
import json,tarfile,gzip,hashlib
root=Path('/home/ltlly/Code/paseo');q=Path(__file__).parent
old=json.loads((q/'source-core-tests-identity.json').read_text());paths=[p for p in old['paths'] if(root/p).is_file()]
with (q/'source.tar.gz').open('wb')as out,gzip.GzipFile(filename='',fileobj=out,mode='wb',mtime=0)as zipped,tarfile.open(fileobj=zipped,mode='w')as tar:
 for path in paths:
  info=tar.gettarinfo(str(root/path),arcname=path);info.uid=info.gid=0;info.uname=info.gname='';info.mtime=0
  with (root/path).open('rb')as f:tar.addfile(info,f)
sha=hashlib.file_digest((q/'source.tar.gz').open('rb'),'sha256').hexdigest();(q/'source-identity.json').write_text(json.dumps({'sha256':sha,'files':len(paths),'paths':paths,'fileSha256':{p:hashlib.sha256((root/p).read_bytes()).hexdigest()for p in paths}},indent=2))

for name in ['check-mac.sh','check-win.ps1']:
 s=(q/name).read_text().replace(old['sha256'],sha)
 if name.endswith('.sh'):
  a=s.index('say "TEST_FILTER=');b=s.index('say "STEP=cargo_native_build"',a)
 else:
  a=s.index('  $tests=@');b=s.index('  Log "STEP=cargo_native_build"',a)
 s=s[:a]+s[b:];(q/name).write_text(s)
(q/'remote-id').write_text(sha[:12])
core=['packages/desktop/native/src/terminal_profile_settings.rs','packages/desktop/native/src/testdata/terminal-profile-settings.json']
(q/'tested-core-match.json').write_text(json.dumps({p:hashlib.sha256((root/p).read_bytes()).hexdigest()==old['fileSha256'][p]for p in core},indent=2))
print(sha)
