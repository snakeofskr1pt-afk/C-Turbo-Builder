@echo off
setlocal
rem ---------------------------------------------------------------------------
rem  Drag & drop launcher for cpp-turbo-builder.
rem
rem  Usage: drag a C++ project folder onto this file in Explorer.
rem
rem  The project is built with `turbo build --root <folder>`, so it picks up
rem  turbo.json from that folder (compiler, include dirs, chunk size, ...).
rem  Keep turbo.exe next to this script (as in the release zip), or run it from
rem  the repository root where target\release\turbo.exe exists.
rem ---------------------------------------------------------------------------

set "TURBO=%~dp0turbo.exe"
if not exist "%TURBO%" set "TURBO=%~dp0target\release\turbo.exe"
if not exist "%TURBO%" (
    echo turbo.exe was not found next to this script.
    echo Download the release zip or build the project first: cargo build --release
    pause
    exit /b 2
)

if "%~1"=="" (
    echo Drag a C++ project folder onto this file to build it.
    echo.
    echo The folder should contain your sources and optionally a turbo.json.
    pause
    exit /b 2
)

echo ============================================================
echo  Building: %~1
echo ============================================================
"%TURBO%" build --root "%~1" --pause
exit /b %ERRORLEVEL%
