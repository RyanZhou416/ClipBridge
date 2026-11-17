[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'rust-report.psm1') -Force

$tests = 0
function Assert-Rejected([string]$Name, [string]$Text) {
    $rejected = $false
    try { $null = Get-CargoTestSummary -Text $Text } catch { $rejected = $true }
    if (-not $rejected) { throw "Unexpected acceptance: $Name" }
    $script:tests++
}
Assert-Rejected 'missing report' ''
Assert-Rejected 'only build output' 'Finished test profile; Running unittests'
Assert-Rejected 'zero tests' 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'
Assert-Rejected 'all ignored' 'test result: ok. 0 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 0.00s'
Assert-Rejected 'all filtered' 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.00s'
Assert-Rejected 'failed status' 'test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'
Assert-Rejected 'inconsistent failed count' 'test result: ok. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s'

$text = @'
running 3 tests
test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests example
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
'@
$summary = Get-CargoTestSummary -Text $text
if ($summary.executed -ne 2 -or $summary.discovered -ne 3 -or $summary.suites -ne 2 -or $summary.ignored -ne 1) {
    throw 'Valid workspace summary was counted incorrectly.'
}
$tests++
Assert-Rejected 'later failed suite' ($text + "`n" + 'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s')
Write-Host "PASS: $tests cargo report contract tests"
