@echo off
cd /d "%~dp0"

git diff > changes.diff
