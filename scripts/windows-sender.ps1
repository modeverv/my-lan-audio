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
    [int]$InputBufferSizeFrames = 0,
    [double]$MetricsIntervalSec = 1.0,
    [double]$DurationSec = 0,
    [string]$OutputFile = "",
    [string]$LogFile = "",
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
    if ($InputBufferSizeFrames -gt 0) {
        $cargoArgs += @("--input-buffer-size-frames", ([string]$InputBufferSizeFrames))
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

if ($LogFile) {
    $logDir = Split-Path -Parent $LogFile
    if ($logDir) {
        New-Item -ItemType Directory -Path $logDir -Force | Out-Null
    }
    $resolvedLogFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($LogFile)
    "sender log: $resolvedLogFile" | Tee-Object -FilePath $resolvedLogFile -Append
    "sender command: mise exec -- cargo $($cargoArgs -join ' ')" | Tee-Object -FilePath $resolvedLogFile -Append
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & mise exec -- cargo @cargoArgs 2>&1 |
            ForEach-Object { $_.ToString() } |
            Tee-Object -FilePath $resolvedLogFile -Append
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
} else {
    & mise exec -- cargo @cargoArgs
    $exitCode = $LASTEXITCODE
}

exit $exitCode
