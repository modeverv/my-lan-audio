@echo off
setlocal

pushd "%~dp0\.." || exit /b 1

if "%~1"=="" (
    if "%LAN_AUDIO_TARGET%"=="" set "LAN_AUDIO_TARGET=192.168.11.65:50000"
    if "%LAN_AUDIO_FEEDBACK_LISTEN%"=="" set "LAN_AUDIO_FEEDBACK_LISTEN=0.0.0.0:50001"
    if "%LAN_AUDIO_DEVICE%"=="" set "LAN_AUDIO_DEVICE=CABLE Output"
    echo sender target: %LAN_AUDIO_TARGET%
    echo feedback listen: %LAN_AUDIO_FEEDBACK_LISTEN%
    echo capture device: %LAN_AUDIO_DEVICE%
    powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-sender.ps1" ^
        -Target "%LAN_AUDIO_TARGET%" ^
        -FeedbackListen "%LAN_AUDIO_FEEDBACK_LISTEN%" ^
        -Device "%LAN_AUDIO_DEVICE%"
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
