@echo off
rem Runs once as Administrator after Windows is installed (dockurr/windows copies /oem to C:\OEM and runs this).
rem Installs Visual Studio Build Tools and Rust, then Perl and tauri-cli through setup.cmd, then starts
rem watch.cmd at every login to wait for build requests. The NSIS packager is downloaded by tauri itself.
set ROOT=C:\bobcat
set LOG=%ROOT%\install.log
mkdir %ROOT% 2>nul
mkdir %ROOT%\dl 2>nul
copy /Y %~dp0watch.cmd %ROOT%\watch.cmd >nul
copy /Y %~dp0stub.cmd %ROOT%\build.cmd >nul

echo Installing Visual Studio Build Tools >> %LOG%
curl.exe -L -o %ROOT%\dl\vs_buildtools.exe https://aka.ms/vs/17/release/vs_BuildTools.exe >> %LOG% 2>&1
%ROOT%\dl\vs_buildtools.exe --quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended >> %LOG% 2>&1

echo Installing Rust >> %LOG%
set RUSTUP_HOME=%ROOT%\rustup
set CARGO_HOME=%ROOT%\cargo
curl.exe -L -o %ROOT%\dl\rustup-init.exe https://win.rustup.rs/x86_64 >> %LOG% 2>&1
%ROOT%\dl\rustup-init.exe -y --profile minimal --default-toolchain stable >> %LOG% 2>&1

rem Perl and tauri-cli: in their own script, which can be run again (this script once stopped after the Rust step)
call %~dp0setup.cmd

echo Registering the build watcher >> %LOG%
rem Not a SYSTEM scheduled task: only the logged-in user can reach the host share, so watch.cmd runs from the Startup folder
copy /Y %ROOT%\watch.cmd "C:\ProgramData\Microsoft\Windows\Start Menu\Programs\StartUp\bobcat-watch.cmd" >> %LOG% 2>&1
start "bobcat-watch" /MIN cmd /c C:\bobcat\watch.cmd

mkdir \\host.lan\Data\out 2>nul
copy /Y %LOG% \\host.lan\Data\out\install.log >nul 2>&1
echo ready> \\host.lan\Data\out\ready
