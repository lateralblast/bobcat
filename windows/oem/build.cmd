@echo off
rem (C:\bobcat\build.cmd in the VM is a stub that calls this file from the share, so edits here apply at once.)
rem Copies the repository out of the read-only share, builds the GUI installer (NSIS) and bobcat-cli, and copies
rem them back to out\bundle (nsis\ and cli\). Arguments are ignored (kept for the watcher's request file).
set SHARE=\\host.lan\Data
set LOG=%SHARE%\out\build.log
set WORK=C:\bobcat\work
set RUSTUP_HOME=C:\bobcat\rustup
set CARGO_HOME=C:\bobcat\cargo
rem tauri's Windows resource step needs RC.EXE (Windows SDK) on the PATH; the newest SDK's x64 tools folder is used
rem (calling vcvars64.bat from here ended the script early, and cargo finds the MSVC linker by itself)
for /d %%d in ("C:\Program Files (x86)\Windows Kits\10\bin\10.*") do if exist "%%d\x64\rc.exe" set "SDKBIN=%%d\x64"
set PATH=%SDKBIN%;C:\bobcat\cargo\bin;C:\Strawberry\perl\bin;C:\Strawberry\c\bin;%PATH%
del "%SHARE%\out\build.done" 2>nul
del "%LOG%" 2>nul
rmdir /S /Q "%SHARE%\out\bundle" 2>nul
echo Copying the repository >> "%LOG%"
rem rc.exe copied to a path without spaces or parentheses: the Rust build script could not start it from Program Files (x86)
if not exist C:\bobcat\rc mkdir C:\bobcat\rc
copy /Y "%SDKBIN%\rc.exe" C:\bobcat\rc\ >nul
copy /Y "%SDKBIN%\rcdll.dll" C:\bobcat\rc\ >nul
set "RC=C:\bobcat\rc\rc.exe"
echo RC=%RC% >> "%LOG%"
rem /XD keeps target\ between builds, so only the first one compiles everything
robocopy "%SHARE%\src" %WORK% /MIR /XD "%SHARE%\src\windows\out" "%SHARE%\src\dist" .git target node_modules gen /XF *.pyc /NFL /NDL /NJH /NJS /NP >> "%LOG%" 2>&1
set BUILDRC=0
cd /d %WORK%\gui\src-tauri
echo Building the GUI installer >> "%LOG%"
cargo tauri build --bundles nsis --features vendored < nul >> "%LOG%" 2>&1
if errorlevel 1 set BUILDRC=1
rem the installer is copied as soon as it exists, so a failing CLI build does not lose it
if "%BUILDRC%"=="0" xcopy /E /Y /I /Q %WORK%\gui\src-tauri\target\release\bundle\nsis "%SHARE%\out\bundle\nsis" >> "%LOG%" 2>&1
if "%BUILDRC%"=="0" (
  cd /d %WORK%
  echo Building bobcat-cli >> "%LOG%"
  cargo build --release --features vendored < nul >> "%LOG%" 2>&1
  if errorlevel 1 set BUILDRC=1
)
rem plain copy into a directory that exists: xcopy asked "file or directory?" here and waited forever
if "%BUILDRC%"=="0" mkdir "%SHARE%\out\bundle\cli" 2>nul
if "%BUILDRC%"=="0" copy /Y %WORK%\target\release\bobcat-cli.exe "%SHARE%\out\bundle\cli\" >> "%LOG%" 2>&1
>"%SHARE%\out\build.done" echo %BUILDRC%
