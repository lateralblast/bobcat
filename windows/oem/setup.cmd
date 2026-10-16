@echo off
rem Installs what is still missing after Visual Studio Build Tools and Rust: Strawberry Perl (the vendored OpenSSL build
rem needs it) and tauri-cli. Safe to run again; install.bat calls it, and it can be run by hand in the VM.
set ROOT=C:\bobcat
set LOG=%ROOT%\install.log
set RUSTUP_HOME=%ROOT%\rustup
set CARGO_HOME=%ROOT%\cargo
set PATH=%CARGO_HOME%\bin;C:\Strawberry\perl\bin;%PATH%
mkdir %ROOT%\dl 2>nul

if exist C:\Strawberry\perl\bin\perl.exe goto perl_done
echo Installing Strawberry Perl >> %LOG%
curl.exe -L -o %ROOT%\dl\perl.msi https://github.com/StrawberryPerl/Perl-Dist-Strawberry/releases/download/SP_53822_64bit/strawberry-perl-5.38.2.2-64bit.msi >> %LOG% 2>&1
msiexec /i %ROOT%\dl\perl.msi /qn /norestart INSTALLDIR=C:\Strawberry >> %LOG% 2>&1
:perl_done

if exist %CARGO_HOME%\bin\cargo-tauri.exe goto tauri_done
echo Installing tauri-cli >> %LOG%
cargo install tauri-cli --version "^2" --locked >> %LOG% 2>&1
:tauri_done

copy /Y %LOG% \\host.lan\Data\out\install.log >nul 2>&1
