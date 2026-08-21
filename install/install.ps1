# Teak CLI — build from source. This fork is not distributed via coffeecli.com.
#
# Upstream Coffee CLI (different product):
#   https://github.com/edison7009/Coffee-CLI
#   https://coffeecli.com

Write-Host ""
Write-Host "  Teak CLI" -ForegroundColor Cyan
Write-Host "  Fork of Coffee CLI — https://github.com/edison7009/Coffee-CLI"
Write-Host ""
Write-Host "  There is no coffeecli.com installer for this tree." -ForegroundColor Yellow
Write-Host "  Downloading from coffeecli.com would install upstream Coffee CLI."
Write-Host ""
Write-Host "  Build from source:"
Write-Host ""
Write-Host "    cd src-ui; npm ci; npm run build; cd .."
Write-Host "    cargo build --release"
Write-Host ""
Write-Host "  Binary: target/release/teak-cli.exe"
Write-Host "  Config: ~/.teak-cli/"
Write-Host ""
