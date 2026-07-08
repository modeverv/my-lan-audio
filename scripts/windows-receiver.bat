@echo off
setlocal

pushd "%~dp0\.." || exit /b 1

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-receiver.ps1" %*
set "EXIT_CODE=%ERRORLEVEL%"

popd

if not "%EXIT_CODE%"=="0" (
    echo.
    echo receiver exited with code %EXIT_CODE%
    pause
)

exit /b %EXIT_CODE%
