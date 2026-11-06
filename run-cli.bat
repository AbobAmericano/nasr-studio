@echo off
setlocal
cd /d "%~dp0"

set "PATH=C:\Program Files\LLVM\bin;C:\Users\user\AppData\Local\Microsoft\WinGet\Packages\Gyan.FFmpeg.Shared_Microsoft.Winget.Source_8wekyb3d8bbwe\ffmpeg-9.0.2-full_build-shared\bin;C:\Users\user\.cargo\bin;%PATH%"

if exist "target\release\obs-rs.exe" (
    echo Starting obs-rs CLI (Release Mode)...
    target\release\obs-rs.exe %*
) else if exist "target\debug\obs-rs.exe" (
    echo Starting obs-rs CLI (Debug Mode)...
    target\debug\obs-rs.exe %*
) else (
    echo Building and running obs-cli...
    cargo run --release -p obs-cli -- %*
)

pause
