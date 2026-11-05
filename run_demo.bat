@echo off
setlocal
cd /d "%~dp0"

taskkill /F /IM app.exe >nul 2>&1

set GSTREAMER_1_0_ROOT_MSVC_X86_64=%LOCALAPPDATA%\Programs\gstreamer\1.0\msvc_x86_64\
set PATH=%LOCALAPPDATA%\Programs\gstreamer\1.0\msvc_x86_64\bin;%USERPROFILE%\.cargo\bin;%PATH%
set PKG_CONFIG_PATH=%LOCALAPPDATA%\Programs\gstreamer\1.0\msvc_x86_64\lib\pkgconfig
set RUST_BACKTRACE=1

if not exist "%~dp0src-tauri\target\debug\app.exe" (
    echo ERROR: app.exe not found!
    pause
    exit /b 1
)

start "" "%~dp0src-tauri\target\debug\app.exe"
