$ErrorActionPreference = "Stop"
$workerDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Push-Location $workerDir
try {
    npm run check:deploy
    npm run deploy
} finally {
    Pop-Location
}
