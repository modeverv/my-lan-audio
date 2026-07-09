@echo off
setlocal

pushd "%~dp0\.." || exit /b 1

if not "%~1"=="" goto run_args

if "%LAN_AUDIO_TARGET%"=="" set "LAN_AUDIO_TARGET=192.168.11.65:50000"
if "%LAN_AUDIO_FEEDBACK_LISTEN%"=="" set "LAN_AUDIO_FEEDBACK_LISTEN=0.0.0.0:50001"
if "%LAN_AUDIO_DEVICE%"=="" set "LAN_AUDIO_DEVICE=CABLE Output"
if "%LAN_AUDIO_PACKET_MS%"=="" set "LAN_AUDIO_PACKET_MS=10"
if "%LAN_AUDIO_CAPTURE_QUEUE_CAPACITY%"=="" set "LAN_AUDIO_CAPTURE_QUEUE_CAPACITY=16"
rem if "%LAN_AUDIO_CAPTURE_QUEUE_MODE%"=="" set "LAN_AUDIO_CAPTURE_QUEUE_MODE=latest"
if "%LAN_AUDIO_CAPTURE_QUEUE_MODE%"=="" set "LAN_AUDIO_CAPTURE_QUEUE_MODE=fifo"

if "%LAN_AUDIO_CAPTURE_PACKET_PACING%"=="" set "LAN_AUDIO_CAPTURE_PACKET_PACING=on"
if "%LAN_AUDIO_INPUT_BUFFER_SIZE_FRAMES%"=="" set "LAN_AUDIO_INPUT_BUFFER_SIZE_FRAMES=256"
echo sender target: %LAN_AUDIO_TARGET%
echo feedback listen: %LAN_AUDIO_FEEDBACK_LISTEN%
echo capture device: %LAN_AUDIO_DEVICE%
echo packet ms: %LAN_AUDIO_PACKET_MS%
echo capture queue capacity: %LAN_AUDIO_CAPTURE_QUEUE_CAPACITY%
echo capture queue mode: %LAN_AUDIO_CAPTURE_QUEUE_MODE%
echo capture packet pacing: %LAN_AUDIO_CAPTURE_PACKET_PACING%
echo input buffer size frames: %LAN_AUDIO_INPUT_BUFFER_SIZE_FRAMES%
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-sender.ps1" ^
    -Target "%LAN_AUDIO_TARGET%" ^
    -FeedbackListen "%LAN_AUDIO_FEEDBACK_LISTEN%" ^
    -Device "%LAN_AUDIO_DEVICE%" ^
    -PacketMs "%LAN_AUDIO_PACKET_MS%" ^
    -CaptureQueueCapacity "%LAN_AUDIO_CAPTURE_QUEUE_CAPACITY%" ^
    -CaptureQueueMode "%LAN_AUDIO_CAPTURE_QUEUE_MODE%" ^
    -CapturePacketPacing "%LAN_AUDIO_CAPTURE_PACKET_PACING%" ^
    -InputBufferSizeFrames "%LAN_AUDIO_INPUT_BUFFER_SIZE_FRAMES%" ^
    -Release
goto after_run

:run_args
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows-sender.ps1" %*

:after_run
set "EXIT_CODE=%ERRORLEVEL%"

popd

if not "%EXIT_CODE%"=="0" (
    echo.
    echo sender exited with code %EXIT_CODE%
    pause
)

exit /b %EXIT_CODE%
