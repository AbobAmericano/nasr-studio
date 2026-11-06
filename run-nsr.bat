@echo off
setlocal
cd /d "%~dp0"
set "PATH=C:\Program Files\LLVM\bin;C:\Users\user\AppData\Local\Microsoft\WinGet\Packages\Gyan.FFmpeg.Shared_Microsoft.Winget.Source_8wekyb3d8bbwe\ffmpeg-9.0.2-full_build-shared\bin;C:\Users\user\.cargo\bin;%PATH%"

if exist "target\release\nasr-studio.exe" (
    echo Starting nasr studio...
    target\release\nasr-studio.exe
) else if exist "target\release\nsr-studio.exe" (
    echo Starting nsr studio...
    target\release\nsr-studio.exe
) else (
    echo Building and launching nasr studio...
    cargo run --release -p nasr-studio
)
