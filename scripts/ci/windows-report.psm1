#Requires -Version 7.0
<#
    ClipBridge Windows CI evidence parsers.

    These functions are intentionally pure (input text/directory -> structured evidence) so the
    failure contract can be tested offline without a Windows toolchain:
      - MSTest TRX: report present, discovered > 0, executed > 0, mandatory suites executed.
      - NuGet JSON: real package graph (direct + transitive) and High/Critical/unknown severity blocks.
#>

Set-StrictMode -Version Latest

function Get-JsonArray {
    param($Object, [string]$Name)
    if ($null -eq $Object) { return @() }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property -or $null -eq $property.Value) { return @() }
    return @($property.Value)
}

function Get-NuGetScanSummary {
    <#
      Parse a `dotnet package list --include-transitive --format json --output-version 1` document.
      Throws when the restore graph is absent/empty so an unscanned project can never look clean.
    #>
    param(
        [Parameter(Mandatory = $true)]$Json,
        [string]$Project = '(unknown)'
    )

    $projects = Get-JsonArray $Json 'projects'
    if ($projects.Count -eq 0) {
        throw "NuGet scan for '$Project' contains no projects."
    }

    $frameworkCount = 0
    $topLevelCount = 0
    $transitiveCount = 0
    $packages = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $transitivePackages = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)

    foreach ($projectJson in $projects) {
        foreach ($framework in (Get-JsonArray $projectJson 'frameworks')) {
            if ($null -eq $framework) { continue }
            $frameworkCount++
            foreach ($package in (Get-JsonArray $framework 'topLevelPackages')) {
                if ($null -eq $package) { continue }
                $topLevelCount++
                $null = $packages.Add("$($package.id)@$($package.resolvedVersion)")
            }
            foreach ($package in (Get-JsonArray $framework 'transitivePackages')) {
                if ($null -eq $package) { continue }
                $transitiveCount++
                $null = $packages.Add("$($package.id)@$($package.resolvedVersion)")
                $null = $transitivePackages.Add("$($package.id)@$($package.resolvedVersion)")
            }
        }
    }

    if ($frameworkCount -eq 0 -or ($topLevelCount + $transitiveCount) -eq 0) {
        throw "NuGet scan for '$Project' resolved no packages; refusing to treat an empty scan as clean."
    }

    return [pscustomobject]@{
        frameworkCount         = $frameworkCount
        topLevelPackageCount   = $topLevelCount
        transitivePackageCount = $transitiveCount
        packages               = @($packages)
        transitivePackages     = @($transitivePackages)
    }
}

function Get-NuGetVulnerabilitySummary {
    <#
      Parse a `dotnet package list --vulnerable --include-transitive --format json` document.
      High/Critical block. Unknown/empty severity also blocks (cannot prove it is low/medium).
    #>
    param(
        [Parameter(Mandatory = $true)]$Json,
        [string]$Project = '(unknown)'
    )

    $result = [ordered]@{
        vulnerablePackageCount = 0
        high                   = 0
        critical               = 0
        unknownSeverity        = 0
        violations             = [System.Collections.Generic.List[object]]::new()
    }

    foreach ($projectJson in (Get-JsonArray $Json 'projects')) {
        foreach ($framework in (Get-JsonArray $projectJson 'frameworks')) {
            if ($null -eq $framework) { continue }
            $packages = @(Get-JsonArray $framework 'topLevelPackages') + @(Get-JsonArray $framework 'transitivePackages')
            foreach ($package in $packages) {
                if ($null -eq $package) { continue }
                foreach ($finding in (Get-JsonArray $package 'vulnerabilities')) {
                    if ($null -eq $finding) { continue }
                    $result.vulnerablePackageCount++
                    $severity = ([string]$finding.severity).Trim()
                    $normalized = $severity.ToLowerInvariant()
                    if ($normalized -eq 'critical') {
                        $result.critical++
                        $result.violations.Add([pscustomobject]@{ project = $Project; package = [string]$package.id; severity = $severity })
                    } elseif ($normalized -eq 'high') {
                        $result.high++
                        $result.violations.Add([pscustomobject]@{ project = $Project; package = [string]$package.id; severity = $severity })
                    } elseif ($normalized -notin @('low', 'moderate', 'medium')) {
                        $result.unknownSeverity++
                        $result.violations.Add([pscustomobject]@{
                            project  = $Project
                            package  = [string]$package.id
                            severity = if ($severity) { $severity } else { '(unknown)' }
                        })
                    }
                }
            }
        }
    }

    return $result
}

function Get-TestEvidence {
    <#
      Parse TRX result(s). Missing report, zero discovery, zero execution, all-skipped and missing
      mandatory suites all produce problems instead of a false pass. Total test count is not pinned.
    #>
    param(
        [Parameter(Mandatory = $true)][string]$ResultsDirectory,
        [string[]]$RequiredSuites = @()
    )

    $trxFiles = @(Get-ChildItem -Path $ResultsDirectory -Filter '*.trx' -File -ErrorAction SilentlyContinue)
    $evidence = [ordered]@{
        resultsDirectory     = $ResultsDirectory
        trxFiles             = @($trxFiles | ForEach-Object { $_.FullName })
        discovered           = 0
        executed             = 0
        passed               = 0
        failed               = 0
        skipped              = 0
        inconclusive         = 0
        mandatorySuites      = @()
        allMandatoryExecuted = $false
        problems             = @()
    }

    if ($trxFiles.Count -eq 0) {
        $evidence.problems += 'No TRX result report was produced.'
        return $evidence
    }

    $definitions = @{}
    $results = [System.Collections.Generic.List[object]]::new()

    foreach ($trxFile in $trxFiles) {
        try {
            [xml]$xml = Get-Content -LiteralPath $trxFile.FullName -Raw
        } catch {
            $evidence.problems += "TRX '$($trxFile.Name)' is not valid XML: $($_.Exception.Message)"
            continue
        }
        $testRun = $xml.SelectSingleNode('/*[local-name()="TestRun"]')
        if ($null -eq $testRun) {
            $evidence.problems += "TRX '$($trxFile.Name)' has no TestRun element."
            continue
        }

        foreach ($unitTest in @($testRun.SelectNodes('.//*[local-name()="TestDefinitions"]/*[local-name()="UnitTest"]'))) {
            if ($null -eq $unitTest) { continue }
            $id = [string]$unitTest.GetAttribute('id')
            if (-not $id) { continue }
            $testMethod = $unitTest.SelectSingleNode('./*[local-name()="TestMethod"]')
            $class = if ($null -ne $testMethod) { [string]$testMethod.GetAttribute('className') } else { '' }
            $definitions[$id] = $class
        }

        foreach ($result in @($testRun.SelectNodes('.//*[local-name()="Results"]/*[local-name()="UnitTestResult"]'))) {
            if ($null -eq $result) { continue }
            $results.Add([pscustomobject]@{
                testId   = [string]$result.GetAttribute('testId')
                testName = [string]$result.GetAttribute('testName')
                outcome  = [string]$result.GetAttribute('outcome')
            })
        }
    }

    $evidence.discovered = $definitions.Count
    $perSuite = @{}
    foreach ($definition in $definitions.GetEnumerator()) {
        $className = [string]$definition.Value
        if (-not $className) { $className = '(unknown)' }
        if (-not $perSuite.ContainsKey($className)) {
            $perSuite[$className] = [ordered]@{ name = $className; discovered = 0; executed = 0; passed = 0; failed = 0; skipped = 0 }
        }
        $perSuite[$className].discovered++
    }

    foreach ($result in $results) {
        $className = if ($definitions.ContainsKey($result.testId)) { [string]$definitions[$result.testId] } else { '(unknown)' }
        if (-not $className) { $className = '(unknown)' }
        if (-not $perSuite.ContainsKey($className)) {
            $perSuite[$className] = [ordered]@{ name = $className; discovered = 0; executed = 0; passed = 0; failed = 0; skipped = 0 }
        }

        switch ($result.outcome) {
            'Passed' {
                $evidence.passed++
                $evidence.executed++
                $perSuite[$className].passed++
                $perSuite[$className].executed++
            }
            'NotExecuted' {
                $evidence.skipped++
                $perSuite[$className].skipped++
            }
            'Inconclusive' {
                $evidence.inconclusive++
            }
            default {
                $evidence.failed++
                $evidence.executed++
                $perSuite[$className].failed++
                $perSuite[$className].executed++
            }
        }
    }

    $suiteEvidence = [System.Collections.Generic.List[object]]::new()
    $missing = [System.Collections.Generic.List[string]]::new()
    foreach ($required in $RequiredSuites) {
        if (-not $perSuite.ContainsKey($required)) {
            $missing.Add($required)
            $suiteEvidence.Add([ordered]@{ name = $required; discovered = 0; executed = 0; passed = 0; failed = 0; skipped = 0 })
            continue
        }
        $suiteEvidence.Add($perSuite[$required])
        if ($perSuite[$required].executed -eq 0) {
            $evidence.problems += "Mandatory suite '$required' discovered tests but executed none (all skipped/not run)."
        }
    }
    if ($missing.Count -gt 0) {
        $evidence.problems += "Mandatory suite(s) missing from TRX: $($missing -join ', ')."
    }

    $evidence.mandatorySuites = @($suiteEvidence)
    $evidence.allMandatoryExecuted = ($missing.Count -eq 0 -and @($suiteEvidence | Where-Object { $_.executed -eq 0 }).Count -eq 0)

    if ($evidence.discovered -eq 0) {
        $evidence.problems += 'TRX reports zero discovered tests.'
    }
    if ($evidence.executed -eq 0) {
        $evidence.problems += 'TRX reports zero executed tests (all skipped/not run).'
    }
    if ($evidence.failed -gt 0) {
        $evidence.problems += "TRX reports $($evidence.failed) failed test(s)."
    }

    return $evidence
}

Export-ModuleMember -Function Get-JsonArray, Get-NuGetScanSummary, Get-NuGetVulnerabilitySummary, Get-TestEvidence
