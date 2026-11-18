:: | Считает количество строк в указанной папке
:: | По умолчанию - src
@echo off
set "SRC=%~dp0src"

powershell -NoProfile -ExecutionPolicy Bypass -Command "$files = Get-ChildItem -LiteralPath $env:SRC -File -Recurse; $all = 0; $code = 0; $comments = 0; foreach ($file in $files) { foreach ($line in [System.IO.File]::ReadLines($file.FullName)) { $all++; $text = $line.Trim(); if ($text.Length -gt 0) { if ($text.StartsWith('/')) { $comments++ } else { $code++ } } } }; Write-Host (' Total ' + $all + ' lines'); Write-Host (' Code: ' + $code + ' lines'); Write-Host (' comments: ' + $comments + ' lines'); Write-Host (''); Write-Host ('')"
pause