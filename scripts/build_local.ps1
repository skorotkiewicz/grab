
param(
    [switch]$Clean,
    [switch]$Verbose
)

Write-Host "🚀 Rust File Downloader - Local Build Script" -ForegroundColor Green
Write-Host "Building for Windows + Linux platforms..." -ForegroundColor Cyan
Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Gray

if ($Clean) {
    Write-Host "🧹 Cleaning previous builds..." -ForegroundColor Yellow
    cargo clean
    Remove-Item -Path "releases" -Recurse -Force -ErrorAction SilentlyContinue
}


Write-Host "📦 Checking build tools..." -ForegroundColor Yellow
$crossInstalled = Get-Command cross -ErrorAction SilentlyContinue
if (-not $crossInstalled) {
    Write-Host "Installing cross compilation tool..." -ForegroundColor Yellow
    cargo install cross
    if ($LASTEXITCODE -ne 0) {
        Write-Host "❌ Failed to install cross tool" -ForegroundColor Red
        exit 1
    }
}


New-Item -ItemType Directory -Force -Path "releases" | Out-Null


$builds = [ordered]@{
    "x86_64-pc-windows-msvc" = @{
        name = "downloader-windows-x64.exe"
        tool = "cargo"
        description = "Windows 64-bit"
    }
    "x86_64-unknown-linux-gnu" = @{
        name = "downloader-linux-x64"
        tool = "cross"
        description = "Linux 64-bit"
    }
    "aarch64-unknown-linux-gnu" = @{
        name = "downloader-linux-arm64"
        tool = "cross"
        description = "Linux ARM64 (Raspberry Pi, AWS Graviton)"
    }
}

$successful = 0
$failed = 0

foreach ($target in $builds.Keys) {
    $build = $builds[$target]
    Write-Host "`n🔨 Building: $($build.description)" -ForegroundColor Cyan
    Write-Host "   Target: $target" -ForegroundColor Gray
    Write-Host "   Tool: $($build.tool)" -ForegroundColor Gray
    

    rustup target add $target | Out-Null
    
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    
    try {
        if ($build.tool -eq "cross") {
            if ($Verbose) {
                cross build --target $target --release
            } else {
                cross build --target $target --release 2>$null
            }
        } else {
            if ($Verbose) {
                cargo build --target $target --release
            } else {
                cargo build --target $target --release 2>$null
            }
        }
        
        if ($LASTEXITCODE -eq 0) {
            $sw.Stop()
            

            $extension = if ($target.Contains("windows")) { ".exe" } else { "" }
            $source = "target/$target/release/downloader$extension"
            $destination = "releases/$($build.name)"
            
            if (Test-Path $source) {
                Copy-Item $source $destination
                $size = [math]::Round((Get-Item $destination).Length / 1MB, 2)
                Write-Host "   ✅ Success! ($($sw.Elapsed.TotalSeconds.ToString('F1'))s, $size MB)" -ForegroundColor Green
                $successful++
            } else {
                Write-Host "   ❌ Binary not found at $source" -ForegroundColor Red
                $failed++
            }
        } else {
            $sw.Stop()
            Write-Host "   ❌ Build failed ($($sw.Elapsed.TotalSeconds.ToString('F1'))s)" -ForegroundColor Red
            $failed++
        }
    } catch {
        $sw.Stop()
        Write-Host "   ❌ Error: $($_.Exception.Message)" -ForegroundColor Red
        $failed++
    }
}

Write-Host "`n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Gray
Write-Host "📊 Build Summary:" -ForegroundColor Yellow
Write-Host "   ✅ Successful: $successful" -ForegroundColor Green
Write-Host "   ❌ Failed: $failed" -ForegroundColor Red

if ($successful -gt 0) {
    Write-Host "`n📁 Built binaries:" -ForegroundColor Yellow
    Get-ChildItem "releases" | Sort-Object Name | ForEach-Object {
        $size = [math]::Round($_.Length / 1MB, 2)
        Write-Host "   📦 $($_.Name) ($size MB)" -ForegroundColor Cyan
    }
    
    Write-Host "`n🎉 Local build complete! Check the 'releases' folder." -ForegroundColor Green
    Write-Host "💡 For macOS builds, push a git tag to trigger GitHub Actions." -ForegroundColor Blue
} else {
    Write-Host "❌ All builds failed. Check error messages above." -ForegroundColor Red
    exit 1
}

Write-Host "`n🧪 Quick test..." -ForegroundColor Yellow
if (Test-Path "releases/downloader-windows-x64.exe") {
    $version = & "releases/downloader-windows-x64.exe" --version 2>$null
    if ($LASTEXITCODE -eq 0) {
        Write-Host "   ✅ Windows binary works!" -ForegroundColor Green
    }
}