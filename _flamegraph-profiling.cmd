@echo off
cd /d "%~dp0"

cargo flamegraph --profile profiling

pause