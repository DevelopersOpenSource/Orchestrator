#!/usr/bin/env bash
# Empacota uma versão PORTÁTIL do Orchestrator para Linux: uma pasta com os
# binários (TUI + app desktop + a trava + a memória) e os scripts de
# dependência/execução, sem instalar nada e sem precisar de FUSE (ao
# contrário do AppImage — útil dentro de container/sandbox, ou quando o
# usuário só quer extrair e rodar).
#
# Uso: scripts/empacotar-portatil-linux.sh   (sai em dist/)
set -euo pipefail

raiz="$(cd "$(dirname "$0")/.." && pwd)"
cd "$raiz"
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

alvo="$(rustc -vV | awk '/^host:/ {print $2}')"
nome="orchestrator-portatil-linux-$alvo"
destino="dist/$nome"

echo "==> dependências da interface e a página da memória (embutida no memoryd)"
(cd web && npm ci --no-audit --no-fund)
(cd web && npm run build:memory)

echo "==> app desktop (via tauri build — NUNCA cargo build direto)"
# `cargo build --release -p orchestrator-desktop` sozinho produz um binário
# que tenta abrir o servidor de desenvolvimento (`devUrl` do tauri.conf.json)
# em vez da interface embutida — mesmo com `web/apps/desktop/dist/` fresco.
# Só o `tauri build` (via a CLI, que passa as variáveis de ambiente certas
# para o build.rs decidir modo produção) embute o app do jeito que roda sem
# precisar de nada escutando em localhost:5179. Visto na prática: o app
# abria e dava "Connection refused" na hora, mesmo com o dist/ atualizado.
(cd web/apps/desktop && npx tauri build --no-bundle)

echo "==> os outros binários (release)"
cargo build --release \
  -p orchestrator-cli -p orchestrator-mcp-server -p orchestrator-memoryd

echo "==> montando $destino"
rm -rf "$destino"
mkdir -p "$destino"
for bin in orchestrator orchestrator-desktop orchestrator-hook orchestrator-mcp orchestrator-memoryd; do
  install -m 0755 "target/release/$bin" "$destino/$bin"
done
install -m 0755 scripts/rodar.sh "$destino/rodar.sh"
install -m 0755 scripts/instalar-dependencias.sh "$destino/instalar-dependencias.sh"
install -m 0644 LICENSE "$destino/LICENSE"
cat > "$destino/LEIAME.txt" <<'EOF'
Orchestrator — versão portátil (Linux)

Não precisa instalar: é só extrair e rodar.

  ./rodar.sh          abre o app (com tela gráfica) ou a TUI (sem)
  ./rodar.sh --tui    força a TUI no terminal
  ./rodar.sh --app    força o app desktop
  ./orchestrator      a TUI direto, como digitar "claude" sozinho abre a sessão dele

Falta alguma biblioteca do sistema (WebKitGTK, etc.)? Rode:
  ./instalar-dependencias.sh --instalar

O código fonte e a documentação completa:
https://github.com/EchoGroupStudio/Orchestrator
EOF

echo "==> compactando"
mkdir -p dist
tar -C dist -czf "dist/$nome.tar.gz" "$nome"

echo
echo "Pronto: dist/$nome.tar.gz"
du -h "dist/$nome.tar.gz" | awk '{print "  " $1}'
