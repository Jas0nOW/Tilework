# Restarts the installed tiling manager (stops a running copy first).
Get-Process tilework -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep 1
Start-Process "$env:LOCALAPPDATA\Tilework\tilework.exe"
'started (log: %LOCALAPPDATA%\Tilework\tilework.log)'
