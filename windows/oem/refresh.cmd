@echo off
rem Installs the current oem scripts from the share into C:\bobcat and restarts the watcher (for when they change after install).
set SRC=\\host.lan\Data\out\oem
copy /Y %SRC%\watch.cmd C:\bobcat\watch.cmd
rem build.cmd in C:\bobcat is a stub (stub.cmd) that calls the current one from the share
copy /Y %SRC%\stub.cmd C:\bobcat\build.cmd
copy /Y %SRC%\watch.cmd "C:\ProgramData\Microsoft\Windows\Start Menu\Programs\StartUp\bobcat-watch.cmd"
taskkill /F /FI "WINDOWTITLE eq bobcat-watch*"
taskkill /F /FI "WINDOWTITLE eq C:\bobcat\watch.cmd*"
start "bobcat-watch" /MIN cmd /c C:\bobcat\watch.cmd
