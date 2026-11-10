@echo off
cd /d "%~dp0"

cargo clippy --all-targets

pause