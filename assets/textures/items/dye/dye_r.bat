@echo off
for %%F in (*) do if not exist "%%F\" ren "%%F" "dye_%%F"
