Set-StrictMode -Version Latest

function Get-CargoTestSummary {
    <# Validate stable cargo/libtest summaries without requiring nightly JSON output. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)

    $pattern = '(?m)^test result: (?<result>ok|FAILED)\. (?<passed>\d+) passed; (?<failed>\d+) failed; (?<ignored>\d+) ignored; (?<measured>\d+) measured; (?<filtered>\d+) filtered out;'
    $groups = [regex]::Matches($Text, $pattern)
    if ($groups.Count -eq 0) { throw 'Cargo test report contains no recognized test summaries.' }
    $passed = 0L
    $failed = 0L
    $ignored = 0L
    $filtered = 0L
    foreach ($group in $groups) {
        $passed += [long]$group.Groups['passed'].Value
        $failed += [long]$group.Groups['failed'].Value
        $ignored += [long]$group.Groups['ignored'].Value
        $filtered += [long]$group.Groups['filtered'].Value
        if ($group.Groups['result'].Value -ne 'ok') { throw 'Cargo test report contains a failed suite.' }
    }
    if ($failed -gt 0) { throw "Cargo test report contains $failed failed tests." }
    if ($passed -eq 0) { throw 'Cargo tests executed zero passing tests (empty, filtered, or entirely ignored suite).' }
    [pscustomobject]@{
        suites = $groups.Count
        discovered = $passed + $failed + $ignored + $filtered
        executed = $passed + $failed
        passed = $passed
        failed = $failed
        ignored = $ignored
        filtered = $filtered
    }
}

Export-ModuleMember -Function Get-CargoTestSummary
