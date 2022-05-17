@echo off
cargo build --release

SET cwd=%cd%

xcopy %cwd%\assets\* %cwd%\target\release\assets /E/H/C/I/Y

cd %cwd%\target\release
start /WAIT /B ./dolphin_auto_updater.exe