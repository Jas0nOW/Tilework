# Restarts the installed tiling manager (stops a running copy first).
Get-Process cosmictile -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 1
Start-Process "$env:LOCALAPPDATA\CosmicTile\cosmictile.exe"
'started (log: %LOCALAPPDATA%\CosmicTile\cosmictile.log)'
