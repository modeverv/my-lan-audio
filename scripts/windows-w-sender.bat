@echo off
setlocal EnableExtensions EnableDelayedExpansion

pushd "%~dp0\.." || exit /b 1

if "%LAN_AUDIO_LOG_DIR%"=="" set "LAN_AUDIO_LOG_DIR=logs"
if not exist "%LAN_AUDIO_LOG_DIR%" mkdir "%LAN_AUDIO_LOG_DIR%"
if "%LAN_AUDIO_LOG_FILE%"=="" (
    set "LAN_AUDIO_LOG_STAMP=!DATE!-!TIME!"
    set "LAN_AUDIO_LOG_STAMP=!LAN_AUDIO_LOG_STAMP:/=!"
    set "LAN_AUDIO_LOG_STAMP=!LAN_AUDIO_LOG_STAMP::=!"
    set "LAN_AUDIO_LOG_STAMP=!LAN_AUDIO_LOG_STAMP:.=!"
    set "LAN_AUDIO_LOG_STAMP=!LAN_AUDIO_LOG_STAMP: =0!"
    set "LAN_AUDIO_LOG_FILE=%CD%\%LAN_AUDIO_LOG_DIR%\windows-w-sender-!LAN_AUDIO_LOG_STAMP!.log"
)
echo w-sender log: %LAN_AUDIO_LOG_FILE%

if not "%~1"=="" goto run_args

if "%LAN_AUDIO_TARGET%"=="" set "LAN_AUDIO_TARGET=127.0.0.1:50000"
if "%LAN_AUDIO_BIND%"=="" set "LAN_AUDIO_BIND=0.0.0.0:0"
if "%LAN_AUDIO_DEVICE%"=="" set "LAN_AUDIO_DEVICE=CABLE Output"
if "%LAN_AUDIO_MAX_PACKET_FRAMES%"=="" set "LAN_AUDIO_MAX_PACKET_FRAMES=240"
if "%LAN_AUDIO_METRICS_INTERVAL_SEC%"=="" set "LAN_AUDIO_METRICS_INTERVAL_SEC=1"

echo sender target: %LAN_AUDIO_TARGET%
echo sender bind: %LAN_AUDIO_BIND%
echo capture device: %LAN_AUDIO_DEVICE%
echo max packet frames: %LAN_AUDIO_MAX_PACKET_FRAMES%
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-w-sender.ps1" ^
    -Target "%LAN_AUDIO_TARGET%" ^
    -Bind "%LAN_AUDIO_BIND%" ^
    -Device "%LAN_AUDIO_DEVICE%" ^
    -MaxPacketFrames "%LAN_AUDIO_MAX_PACKET_FRAMES%" ^
    -MetricsIntervalSec "%LAN_AUDIO_METRICS_INTERVAL_SEC%" ^
    -LogFile "%LAN_AUDIO_LOG_FILE%" ^
    -Release
goto after_run

:run_args
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-w-sender.ps1" -LogFile "%LAN_AUDIO_LOG_FILE%" %*

:after_run
set "EXIT_CODE=%ERRORLEVEL%"

popd

if not "%EXIT_CODE%"=="0" (
    echo.
    echo w-sender exited with code %EXIT_CODE%
    pause
)

exit /b %EXIT_CODE%
