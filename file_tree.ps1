$Exclude = @("target", ".git", "items")

function Show-Tree {
    param(
        [string]$Path,
        [string]$Prefix = ""
    )

    $items = @(Get-ChildItem -LiteralPath $Path -Force | Sort-Object `
        @{ Expression = { if ($_.PSIsContainer) { 0 } else { 1 } } },
        Name)

    for ($index = 0; $index -lt $items.Count; $index++) {
        $item = $items[$index]
        $last = $index -eq ($items.Count - 1)
        $branch = if ($last) { "└──" } else { "├──" }
        $excluded = $item.PSIsContainer -and $Exclude -contains $item.Name
        $name = if ($excluded) { "$($item.Name) ..." } else { $item.Name }

        Write-Output "$Prefix$branch$name"

        if ($item.PSIsContainer -and -not $excluded) {
            $nextPrefix = $Prefix + $(if ($last) { "   " } else { "│  " })
            Show-Tree -Path $item.FullName -Prefix $nextPrefix
        }
    }
}

Show-Tree -Path (Get-Location).Path
Start-Sleep -Seconds 60
