$ErrorActionPreference='Continue'
$ProgressPreference='SilentlyContinue'
[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false)
$sha='344203bd5cf43ba082c59df118c333c93128b3b040f484e33a13a8c6cd5ccb90'
$short=$sha.Substring(0,12)
$u=[Environment]::GetFolderPath('UserProfile')
$archive=Join-Path $u ".cache\paseo-gpui-current-$short.tar.gz"
$src=Join-Path $u ".cache\paseo-gpui-src-$short"
$tc=Join-Path $u '.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin'
$cargo=Join-Path $tc 'cargo.exe'
$rustc=Join-Path $tc 'rustc.exe'
$log=Join-Path $u ".cache\paseo-gpui-check-$short.log"
$exitFile=Join-Path $u ".cache\paseo-gpui-check-$short.exit"
function Log([string]$m){ $m | Tee-Object -FilePath $log -Append }
$result=1
try {
  $actual=(Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
  Log "archive=$archive"
  Log "expected_sha256=$sha"
  Log "actual_sha256=$actual"
  if($actual -ne $sha){ throw "Source hash mismatch" }
  if(Test-Path $src){ throw "Source directory already exists" }
  New-Item -ItemType Directory -Path $src | Out-Null
  & tar.exe -xzf $archive -C $src
  if($LASTEXITCODE -ne 0){ throw "Archive extraction failed: $LASTEXITCODE" }
  Log "extracted=$src"
  $env:PATH="$tc;$env:PATH"
  $env:CARGO_BUILD_JOBS='2'
  $env:CARGO_TERM_COLOR='never'
  $env:CARGO_TARGET_DIR=Join-Path $u '.cache/paseo-gpui-target'
  $env:CARGO_NET_GIT_FETCH_WITH_CLI='true'
  Set-Location $src
  Log ("cargo=" + (& $cargo --version 2>&1))
  Log ("rustc=" + (& $rustc --version 2>&1))
  Log "STEP=cargo_clean"
  & $cargo clean -p paseo-desktop 2>&1 | Tee-Object -FilePath $log -Append
  if($LASTEXITCODE -ne 0){ throw "Native package clean failed: $LASTEXITCODE" }
  Log "STEP=cargo_check"
  & $cargo check --locked -p paseo-desktop 2>&1 | Tee-Object -FilePath $log -Append
  if($LASTEXITCODE -ne 0){ throw "Cargo check failed: $LASTEXITCODE" }
  Log "TEST_FILTER=workspace_tab_layout::tests"
  & $cargo test --locked -p paseo-desktop --no-default-features --lib workspace_tab_layout::tests -- --test-threads=1 2>&1 | Tee-Object -FilePath $log -Append
  if($LASTEXITCODE -ne 0){ throw "Focused workspace tab test failed: $LASTEXITCODE" }
  Log "STEP=cargo_native_build"
  & $cargo build --locked -p paseo-desktop --bin paseo-desktop 2>&1 | Tee-Object -FilePath $log -Append
  if($LASTEXITCODE -ne 0){ throw "Native application build failed: $LASTEXITCODE" }
  $result=0
} catch {
  Log ("ERROR: " + $_.Exception.ToString())
  $result=1
}
Log "RESULT=$result"
$result | Set-Content -Encoding ascii $exitFile
exit $result
