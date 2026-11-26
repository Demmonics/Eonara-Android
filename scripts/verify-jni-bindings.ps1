[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ApkPath,

    [Parameter(Mandatory = $true)]
    [ValidateSet("arm64-v8a", "x86_64")]
    [string]$Abi
)

$ErrorActionPreference = "Stop"

function ConvertTo-JniMangledName([string]$Value) {
    $builder = [System.Text.StringBuilder]::new()
    foreach ($character in $Value.ToCharArray()) {
        switch ([int]$character) {
            47 { [void]$builder.Append("_") }
            59 { [void]$builder.Append("_2") }
            91 { [void]$builder.Append("_3") }
            95 { [void]$builder.Append("_1") }
            default {
                if ([int]$character -gt 127) {
                    [void]$builder.Append(("_0{0:x4}" -f [int]$character))
                }
                else {
                    [void]$builder.Append($character)
                }
            }
        }
    }
    $builder.ToString()
}

$sdk = $env:ANDROID_HOME
if ([string]::IsNullOrWhiteSpace($sdk)) {
    $sdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
}
$ndk = $env:ANDROID_NDK_HOME
if ([string]::IsNullOrWhiteSpace($ndk)) {
    $ndk = Join-Path $sdk "ndk\27.2.12479018"
}

$analyzer = Join-Path $sdk "cmdline-tools\latest\bin\apkanalyzer.bat"
$llvmNm = Join-Path $ndk "toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-nm.exe"
$apk = (Resolve-Path -LiteralPath $ApkPath).Path
$className = "com.lyricmotion.android.LyricMotionActivity"
$classPath = "com\lyricmotion\android\LyricMotionActivity.kt"
$repoRoot = Split-Path $PSScriptRoot -Parent
$kotlinRoot = Join-Path $repoRoot "android\app\src\main\java"
$sourcePath = Join-Path $kotlinRoot $classPath

foreach ($required in @($analyzer, $llvmNm, $sourcePath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required JNI audit input is missing: $required"
    }
    $nativeSourceFiles = @(
        Get-ChildItem -LiteralPath $kotlinRoot -Filter "*.kt" -Recurse |
            Where-Object {
                [regex]::IsMatch([System.IO.File]::ReadAllText($_.FullName), '\bexternal\s+fun\s+')
            }
    )
    if ($nativeSourceFiles.Count -ne 1 -or $nativeSourceFiles[0].FullName -ne (Resolve-Path $sourcePath).Path) {
        throw "JNI declarations exist outside the audited class; extend this DEX/export check before adding bindings"
    }
}

Add-Type -AssemblyName System.IO.Compression.FileSystem
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("lyricmotion-jni-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = [System.IO.Compression.ZipFile]::OpenRead($apk)
    try {
        $libraryEntry = $zip.GetEntry("lib/$Abi/liblyricmotion_android.so")
        if ($null -eq $libraryEntry) {
            throw "APK does not contain lib/$Abi/liblyricmotion_android.so"
        }
        $libraryPath = Join-Path $work "liblyricmotion_android.so"
        $source = $libraryEntry.Open()
        $destination = [System.IO.File]::Create($libraryPath)
        try {
            $source.CopyTo($destination)
        }
        finally {
            $source.Dispose()
            $destination.Dispose()
        }
    }
    finally {
        $zip.Dispose()
    }

    $dexOutput = & $analyzer dex code --class $className $apk
    if ($LASTEXITCODE -ne 0) {
        throw "apkanalyzer could not inspect compiled class $className"
    }
    $compiledMethods = @(
        foreach ($line in $dexOutput) {
            if ($line.ToString() -match '^\s*\.method\s+.*\bnative\s+([A-Za-z0-9_$]+)\(([^)]*)\)(\S+)') {
                [pscustomobject]@{
                    Name = $Matches[1]
                    Parameters = $Matches[2]
                }
            }
        }
    )
    $sourceText = [System.IO.File]::ReadAllText($sourcePath)
    $sourceMethods = @(
        [regex]::Matches($sourceText, '\bexternal\s+fun\s+([A-Za-z0-9_]+)\s*\(') |
            ForEach-Object { $_.Groups[1].Value }
    )
    if ($compiledMethods.Count -eq 0) {
        throw "No compiled native declarations found in $className"
    }
    $sourceMethodSignature = ($sourceMethods | Sort-Object) -join "`n"
    $compiledMethodSignature = ($compiledMethods.Name | Sort-Object) -join "`n"
    if ($sourceMethodSignature -cne $compiledMethodSignature) {
        throw "Kotlin source native declarations differ from the compiled DEX declarations"
    }

    $nmOutput = & $llvmNm -D --defined-only $libraryPath
    if ($LASTEXITCODE -ne 0) {
        throw "llvm-nm could not inspect the APK's ARM64/x86_64 library"
    }
    $exports = @(
        $nmOutput | ForEach-Object {
            $parts = $_.ToString().Trim() -split '\s+'
            if ($parts.Count -gt 0) { $parts[-1] }
        }
    )

    $classMangled = ConvertTo-JniMangledName($className.Replace(".", "/"))
    foreach ($group in ($compiledMethods | Group-Object Name)) {
        foreach ($method in $group.Group) {
            $methodMangled = ConvertTo-JniMangledName($method.Name)
            $expected = "Java_${classMangled}_${methodMangled}"
            if ($group.Count -gt 1) {
                $expected += "__" + (ConvertTo-JniMangledName($method.Parameters))
            }
            if ($expected -notin $exports) {
                throw "Compiled native declaration '$($method.Name)' has no matching exported JNI symbol '$expected'"
            }
            Write-Output "Verified $className.$($method.Name) -> $expected"
        }
    }
    Write-Output "Verified $($compiledMethods.Count) compiled JNI declarations against $Abi exports in $apk"
}
finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}
