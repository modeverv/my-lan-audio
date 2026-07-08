param(
    [string]$Target = "127.0.0.1:50000",
    [string]$FeedbackListen = "",
    [string]$Device = "CABLE Output",
    [double]$PacketMs = 5.0,
    [int]$CaptureQueueCapacity = 32,
    [ValidateSet("latest", "fifo")]
    [string]$CaptureQueueMode = "latest",
    [ValidateSet("off", "on")]
    [string]$CapturePacketPacing = "off",
    [double]$MetricsIntervalSec = 1.0,
    [double]$DurationSec = 0,
    [string]$OutputFile = "",
    [switch]$ListDevices,
    [switch]$MeterOnly,
    [switch]$Release,
    [switch]$NoSenderSideAsrc
)

$ErrorActionPreference = "Stop"

$cargoArgs = @("run")
if ($Release) {
    $cargoArgs += "--release"
}

$cargoArgs += @("-p", "sender", "--")

if ($ListDevices) {
    $cargoArgs += "--list-devices"
} else {
    $cargoArgs += @(
        "--target", $Target,
        "--input", "capture",
        "--packet-ms", ([string]$PacketMs),
        "--capture-queue-capacity", ([string]$CaptureQueueCapacity),
        "--capture-queue-mode", $CaptureQueueMode,
        "--capture-packet-pacing", $CapturePacketPacing,
        "--metrics-interval-sec", ([string]$MetricsIntervalSec)
    )

    if ($Device) {
        $cargoArgs += @("--device", $Device)
    }
    if ($FeedbackListen) {
        $cargoArgs += @("--feedback-listen", $FeedbackListen)
        if (-not $NoSenderSideAsrc) {
            $cargoArgs += "--sender-side-asrc"
        }
    }
    if ($DurationSec -gt 0) {
        $cargoArgs += @("--duration-sec", ([string]$DurationSec))
    }
    if ($OutputFile) {
        $cargoArgs += @("--output-file", $OutputFile)
    }
    if ($MeterOnly) {
        $cargoArgs += "--meter-only"
    }
}

& mise exec -- cargo @cargoArgs
exit $LASTEXITCODE
