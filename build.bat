@echo off
cargo build

SET cwd=%cd%

xcopy %cwd%\assets\* %cwd%\target\debug\assets /E/H/C/I/Y
xcopy %cwd%\stuff\* %cwd%\target\debug\stuff /E/H/C/I/Y

cd %cwd%\target\debug
start /WAIT /B ./dolphin_auto_updater.exe