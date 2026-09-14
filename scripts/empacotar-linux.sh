#!/usr/bin/env bash
# Empacota o Orchestrator para Linux: AppImage, .deb e .rpm.
#
# O app desktop leva junto os binários que o núcleo procura "ao lado do
# executável": o hook de segurança, o servidor MCP, o serviço de memória e a
# CLI. A página da memória é gerada antes do memoryd, que a embute.
#
# Uso: scripts/empacotar-linux.sh   (os pacotes saem em target/release/bundle)
set -euo pipefail

raiz="$(cd "$(dirname "$0")/.." && pwd)"
cd "$raiz"
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

alvo="$(rustc -vV | awk '/^host:/ {print $2}')"
binarios=(orchestrator-hook orchestrator-mcp orchestrator-memoryd orchestrator)

echo "==> dependências da interface"
(cd web && npm ci --no-audit --no-fund)

echo "==> página da memória (embutida no memoryd)"
(cd web && npm run build:memory)

echo "==> binários de apoio (release)"
cargo build --release -p orchestrator-mcp-server -p orchestrator-memoryd -p orchestrator-cli

pasta="web/apps/desktop/src-tauri/binaries"
mkdir -p "$pasta"
externos=()
for nome in "${binarios[@]}"; do
  # O Tauri exige o sufixo do alvo no nome; no pacote ele sai sem sufixo.
  install -m 0755 "target/release/$nome" "$pasta/$nome-$alvo"
  externos+=("\"binaries/$nome\"")
done
config_extra="{\"bundle\":{\"externalBin\":[$(IFS=,; echo "${externos[*]}")]}}"

echo "==> app desktop e pacotes"
# NO_STRIP: o `strip` antigo embutido no linuxdeploy não reconhece a seção
# `.relr.dyn` das bibliotecas de distros novas (Fedora 44) e aborta o AppImage
# ("unknown type [0x13] section .relr.dyn"). Sem strip o AppImage sai maior,
# mas sai.
(cd web/apps/desktop && NO_STRIP=true npx tauri build --config "$config_extra")

echo
echo "Pacotes gerados:"
find target/release/bundle -maxdepth 2 -type f \( -name '*.AppImage' -o -name '*.deb' -o -name '*.rpm' \) -printf '  %p  (%s bytes)\n'
