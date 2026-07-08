param(
    [string]$Listen = "0.0.0.0:50000",
    [string]$FeedbackTarget = "",
    [ValidateSet("null", "audio", "wav")]
    [string]$Output = "audio",
    [string]$OutputDevice = "",
    [string]$OutputFile = "",
    [long]$FixedDelayFrames = 14400,
    [int]$FixedLatencyMs = 0,
    [int]$OutputRingMs = 60,
    [int]$OutputRingCapacityMs = 160,
    [int]$RenderChunkMs = 2,
    [int]$OutputBufferSizeFrames = 0,
    [double]$MetricsIntervalSec = 1.0,
    [double]$DurationSec = 0,
    [double]$Freq = 440.0,
    [switch]$ListDevices,
    [switch]$TestTone,
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$cargoArgs = @("run")
if ($Release) {
    $cargoArgs += "--release"
}

$cargoArgs += @("-p", "receiver", "--")

if ($ListDevices) {
    $cargoArgs += "--list-devices"
} else {
    $cargoArgs += @(
        "--listen", $Listen,
        "--output", $Output,
        "--output-ring-ms", ([string]$OutputRingMs),
        "--output-ring-capacity-ms", ([string]$OutputRingCapacityMs),
        "--render-chunk-ms", ([string]$RenderChunkMs),
        "--metrics-interval-sec", ([string]$MetricsIntervalSec)
    )

    if ($FeedbackTarget) {
        $cargoArgs += @("--feedback-target", $FeedbackTarget)
    }
    if ($FixedDelayFrames -gt 0) {
        $cargoArgs += @("--fixed-delay-frames", ([string]$FixedDelayFrames))
    } elseif ($FixedLatencyMs -gt 0) {
        $cargoArgs += @("--fixed-latency-ms", ([string]$FixedLatencyMs))
    }
    if ($OutputDevice) {
        $cargoArgs += @("--output-device", $OutputDevice)
    }
    if ($OutputFile) {
        $cargoArgs += @("--output-file", $OutputFile)
    }
    if ($OutputBufferSizeFrames -gt 0) {
        $cargoArgs += @("--output-buffer-size-frames", ([string]$OutputBufferSizeFrames))
    }
    if ($DurationSec -gt 0) {
        $cargoArgs += @("--duration-sec", ([string]$DurationSec))
    }
    if ($TestTone) {
        $cargoArgs += @("--test-tone", "--freq", ([string]$Freq))
    }
}

& mise exec -- cargo @cargoArgs
exit $LASTEXITCODE
