@echo off
setlocal

pushd "%~dp0\.." || exit /b 1

if "%~1"=="" (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-sender.ps1" ^
        -Target "192.168.11.65:50000" ^
        -FeedbackListen "0.0.0.0:50001" ^
        -Device "CABLE Output"
) else (
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-sender.ps1" %*
)
set "EXIT_CODE=%ERRORLEVEL%"

popd

if not "%EXIT_CODE%"=="0" (
    echo.
    echo sender exited with code %EXIT_CODE%
    pause
)

exit /b %EXIT_CODE%
