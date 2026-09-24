#!/usr/bin/env bash
# Abre o Orchestrator — o lançador da versão portátil (pasta extraída sem
# instalar, ou os binários recém-compilados em target/release).
#
# Sem argumento: abre o app desktop se houver uma tela gráfica (Wayland ou
# X11) e o binário dele existir; senão cai para a TUI no terminal — o
# mesmo critério de "orchestrator" sozinho abrir a TUI, como o `claude`.
#
# Uso:
#   scripts/rodar.sh            # decide sozinho (app com tela, TUI sem)
#   scripts/rodar.sh --tui      # força a TUI
#   scripts/rodar.sh --app      # força o app desktop
#   scripts/rodar.sh --dev      # usa target/release em vez desta pasta
set -uo pipefail

aqui="$(cd "$(dirname "$0")" && pwd)"
modo="auto"
dev=false
for arg in "$@"; do
  case "$arg" in
    --tui) modo="tui" ;;
    --app) modo="app" ;;
    --dev) dev=true ;;
    -h|--help)
      sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "argumento desconhecido: $arg (use --tui, --app, --dev ou --help)" >&2
      exit 2
      ;;
  esac
done

# Onde estão os binários: esta pasta (versão portátil extraída), senão
# target/release (rodando direto do repositório).
if $dev; then
  pasta="$(cd "$aqui/.." && pwd)/target/release"
elif [ -x "$aqui/orchestrator" ] || [ -x "$aqui/orchestrator-desktop" ]; then
  pasta="$aqui"
else
  pasta="$(cd "$aqui/.." && pwd)/target/release"
fi

tui="$pasta/orchestrator"
app="$pasta/orchestrator-desktop"

# WebKitGTK no Wayland (Fedora/Nobara, AMD): a partir do 2.52 a janela abre
# toda preta se a composição acelerada ficar ligada. O binário já define isto
# por conta própria; aqui é reforço para quem roda pelo launcher. Não
# sobrescreve quem já definiu.
export WEBKIT_DISABLE_DMABUF_RENDERER="${WEBKIT_DISABLE_DMABUF_RENDERER:-1}"
export WEBKIT_DISABLE_COMPOSITING_MODE="${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"

tem_tela() {
  [ -n "${WAYLAND_DISPLAY:-}" ] || [ -n "${DISPLAY:-}" ]
}

if [ "$modo" = "auto" ]; then
  if tem_tela && [ -x "$app" ]; then
    modo="app"
  else
    modo="tui"
  fi
fi

case "$modo" in
  app)
    if [ ! -x "$app" ]; then
      echo "orchestrator-desktop não está em $pasta — tente --tui, ou --dev depois de compilar" >&2
      exit 1
    fi
    exec "$app"
    ;;
  tui)
    if [ ! -x "$tui" ]; then
      echo "orchestrator não está em $pasta — tente --dev depois de compilar (cargo build --release -p orchestrator-cli)" >&2
      exit 1
    fi
    exec "$tui"
    ;;
esac
