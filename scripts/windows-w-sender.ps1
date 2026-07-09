param(
    [string]$Target = "127.0.0.1:50000",
    [string]$Bind = "0.0.0.0:0",
    [string]$Device = "CABLE Output",
    [int]$MaxPacketFrames = 240,
    [double]$MetricsIntervalSec = 1.0,
    [double]$DurationSec = 0,
    [string]$LogFile = "",
    [switch]$ListDevices,
    [switch]$Require48kStereo,
    [switch]$Release
)

$ErrorActionPreference = "Stop"

$cargoArgs = @("run")
if ($Release) {
    $cargoArgs += "--release"
}

$cargoArgs += @("-p", "w-sender", "--")

if ($ListDevices) {
    $cargoArgs += "--list-devices"
} else {
    $cargoArgs += @(
        "--target", $Target,
        "--bind", $Bind,
        "--device", $Device,
        "--max-packet-frames", ([string]$MaxPacketFrames),
        "--metrics-interval-sec", ([string]$MetricsIntervalSec)
    )
    if ($Require48kStereo) {
        $cargoArgs += "--require-48k-stereo"
    }
    if ($DurationSec -gt 0) {
        $cargoArgs += @("--duration-sec", ([string]$DurationSec))
    }
}

if ($LogFile) {
    $logDir = Split-Path -Parent $LogFile
    if ($logDir) {
        New-Item -ItemType Directory -Path $logDir -Force | Out-Null
    }
    $resolvedLogFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($LogFile)
    "w-sender log: $resolvedLogFile" | Tee-Object -FilePath $resolvedLogFile -Append
    "w-sender command: mise exec -- cargo $($cargoArgs -join ' ')" | Tee-Object -FilePath $resolvedLogFile -Append
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
