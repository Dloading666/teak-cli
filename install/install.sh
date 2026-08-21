#!/bin/sh
# Teak CLI — build from source. This fork is not distributed via coffeecli.com.
#
# Upstream Coffee CLI (different product):
#   https://github.com/edison7009/Coffee-CLI
#   https://coffeecli.com

set -e
CYAN='\033[0;36m'
YELLOW='\033[1;33m'
RESET='\033[0m'

echo ""
echo "  ${CYAN}Teak CLI${RESET}"
echo "  Fork of Coffee CLI — https://github.com/edison7009/Coffee-CLI"
echo ""
echo "  ${YELLOW}There is no coffeecli.com installer for this tree.${RESET}"
echo "  Downloading from coffeecli.com would install upstream Coffee CLI."
echo ""
echo "  Build from source:"
echo ""
echo "    cd src-ui && npm ci && npm run build && cd .."
echo "    cargo build --release"
echo ""
echo "  Binary: target/release/teak-cli"
echo "  Config: ~/.teak-cli/"
echo ""
exit 0
