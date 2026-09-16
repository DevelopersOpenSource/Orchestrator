#!/usr/bin/env bash
# Instala, verifica ou atualiza as dependências do Orchestrator: o
# toolchain do Rust, o Node.js, as bibliotecas do sistema que o app desktop
# (Tauri/WebKitGTK) precisa para compilar e rodar, e as dependências do
# `web/` (npm). Sempre prefere a versão ATUAL de cada ferramenta — evita o
# erro clássico de compilar contra algo desatualizado.
#
# Uso:
#   scripts/instalar-dependencias.sh              # só verifica, não mexe em nada
#   scripts/instalar-dependencias.sh --instalar    # instala o que faltar
#   scripts/instalar-dependencias.sh --atualizar   # também atualiza o que já existe
#   scripts/instalar-dependencias.sh --provedores  # + as CLIs de IA opcionais
#                                                     (Codex, Kimi, Antigravity, OpenCode)
#
# Linux apenas (Windows é outra história — ver docs/PACKAGING.md). Pede
# `sudo` só para as bibliotecas de sistema, e só com --instalar/--atualizar.
set -uo pipefail

raiz="$(cd "$(dirname "$0")/.." && pwd)"
cd "$raiz"

acao="verificar"
provedores=false
for arg in "$@"; do
  case "$arg" in
    --instalar) acao="instalar" ;;
    --atualizar) acao="atualizar" ;;
    --provedores) provedores=true ;;
    -h|--help)
      sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "argumento desconhecido: $arg (use --instalar, --atualizar, --provedores ou --help)" >&2
      exit 2
      ;;
  esac
done

faltando=0
verde() { printf '\033[32m%s\033[0m\n' "$1"; }
amarelo() { printf '\033[33m%s\033[0m\n' "$1"; }
vermelho() { printf '\033[31m%s\033[0m\n' "$1"; faltando=$((faltando + 1)); }

if [ "$(uname -s)" != "Linux" ]; then
  vermelho "este script só cobre Linux por enquanto — a versão portátil para Windows fica para depois (ver docs/PACKAGING.md)."
  exit 1
fi

# --------------------------------------------------------------- gerenciador
# Detecta pelo ID/ID_LIKE do /etc/os-release, não só pelo binário que existe
# no PATH (algumas distros trazem `apt` de compatibilidade sem ser Debian).
gerenciador=""
if [ -r /etc/os-release ]; then
  . /etc/os-release
  familia="${ID:-} ${ID_LIKE:-}"
  case "$familia" in
    *fedora*|*rhel*) gerenciador="dnf" ;;
    *debian*|*ubuntu*) gerenciador="apt" ;;
    *arch*) gerenciador="pacman" ;;
    *suse*) gerenciador="zypper" ;;
  esac
fi
if [ -z "$gerenciador" ]; then
  for cand in dnf apt pacman zypper; do
    command -v "$cand" >/dev/null 2>&1 && gerenciador="$cand" && break
  done
fi
echo "==> distro: ${PRETTY_NAME:-desconhecida} (gerenciador: ${gerenciador:-nenhum reconhecido})"

# Bibliotecas de sistema que o app desktop (Tauri/WebKitGTK) precisa para
# compilar e para rodar — a mesma lista usada no CI (.github/workflows/desktop.yml).
pacotes_sistema() {
  case "$gerenciador" in
    dnf) echo "webkit2gtk4.1-devel libappindicator-gtk3-devel librsvg2-devel libxdo-devel openssl-devel patchelf file gcc" ;;
    apt) echo "libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev libxdo-dev libssl-dev patchelf file build-essential" ;;
    pacman) echo "webkit2gtk-4.1 libappindicator-gtk3 librsvg xdotool openssl patchelf file base-devel" ;;
    zypper) echo "webkit2gtk4.1-devel libappindicator3-devel librsvg-devel libxdo-devel libopenssl-devel patchelf file gcc" ;;
    *) echo "" ;;
  esac
}

instalar_pacotes_sistema() {
  local pacotes
  pacotes="$(pacotes_sistema)"
  if [ -z "$pacotes" ]; then
    amarelo "não reconheço o gerenciador de pacotes desta distro — instale manualmente as bibliotecas do WebKitGTK/Tauri (veja .github/workflows/desktop.yml)."
    return
  fi
  echo "==> bibliotecas de sistema (sudo $gerenciador): $pacotes"
  case "$gerenciador" in
    dnf) sudo dnf install -y $pacotes ;;
    apt) sudo apt-get update && sudo apt-get install -y $pacotes ;;
    pacman) sudo pacman -Sy --needed --noconfirm $pacotes ;;
    zypper) sudo zypper install -y $pacotes ;;
  esac
}

# ------------------------------------------------------------------- rust
verificar_rust() {
  # shellcheck disable=SC1091
  [ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
  if command -v rustc >/dev/null 2>&1; then
    verde "✔ Rust $(rustc --version | awk '{print $2}')"
    if [ "$acao" = "atualizar" ]; then
      echo "   atualizando (rustup update)…"
      rustup update
    fi
  elif [ "$acao" = "verificar" ]; then
    vermelho "✖ Rust não encontrado — rode com --instalar (usa o instalador oficial, rustup.rs)"
  else
    echo "==> instalando Rust (rustup, canal stable)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
    verde "✔ Rust $(rustc --version | awk '{print $2}') instalado"
  fi
}

# ------------------------------------------------------------------- node
# Vite 8 (usado pelo app desktop e pela página de memória) pede Node 20+.
NODE_MINIMO=20
verificar_node() {
  if command -v node >/dev/null 2>&1; then
    local versao major
    versao="$(node --version)"
    major="${versao#v}"; major="${major%%.*}"
    if [ "$major" -ge "$NODE_MINIMO" ]; then
      verde "✔ Node $versao"
    else
      vermelho "✖ Node $versao é mais antigo que o exigido (>= v$NODE_MINIMO)"
      if [ "$acao" != "verificar" ]; then
        instalar_node
      fi
    fi
  elif [ "$acao" = "verificar" ]; then
    vermelho "✖ Node.js não encontrado — rode com --instalar"
  else
    instalar_node
  fi
}

instalar_node() {
  echo "==> instalando Node.js $NODE_MINIMO+ (pela distro, quando ela tiver uma versão recente o bastante)"
  case "$gerenciador" in
    dnf) sudo dnf install -y "nodejs" ;;
    apt) sudo apt-get install -y nodejs npm ;;
    pacman) sudo pacman -Sy --needed --noconfirm nodejs npm ;;
    zypper) sudo zypper install -y nodejs npm ;;
    *)
      amarelo "sem gerenciador reconhecido — instale o Node.js $NODE_MINIMO+ manualmente (nodejs.org) ou com nvm/fnm."
      return
      ;;
  esac
  command -v node >/dev/null 2>&1 && verde "✔ Node $(node --version) instalado" || vermelho "✖ a instalação do Node não deixou o binário no PATH — abra um terminal novo e confira"
}

# --------------------------------------------------------- dependências do npm
# Só existe dentro do repositório (código fonte) — a versão portátil não leva
# o `web/`, só os binários já compilados, e não precisa disto.
verificar_npm_web() {
  if [ ! -f web/package.json ]; then
    echo "(sem web/package.json em $raiz — pulando: isto só se aplica rodando de dentro do repositório)"
    return
  fi
  if [ ! -d web/node_modules ]; then
    if [ "$acao" = "verificar" ]; then
      vermelho "✖ web/node_modules não existe — rode com --instalar"
      return
    fi
    echo "==> instalando dependências do web/ (npm ci)"
    (cd web && npm ci --no-audit --no-fund) && verde "✔ web/node_modules instalado"
    return
  fi
  verde "✔ web/node_modules existe"
  if [ "$acao" = "atualizar" ]; then
    echo "==> npm outdated (web/) — o que dá para atualizar"
    (cd web && npm outdated || true)
    echo "   rode \`npm update\` dentro de web/ para aplicar; o package-lock.json muda — confira os testes depois."
  fi
}

# ---------------------------------------------------- CLIs de provedor (opcional)
# As mesmas contas que o /provedor do chat sabe usar — instaladas fora do
# padrão (--provedores), porque nem todo mundo tem (ou quer) essas quatro
# contas. Os comandos batem com `providers::install_command` no engine.
instalar_provedores() {
  echo "==> CLIs de provedor (contas de IA), em ~/.local"
  local bin="$HOME/.local/bin"
  mkdir -p "$bin"
  if command -v npm >/dev/null 2>&1; then
    echo "-- Codex (ChatGPT) e OpenCode (npm --prefix ~/.local, sem sudo)"
    npm i -g --prefix "$HOME/.local" @openai/codex opencode-ai
  else
    amarelo "sem npm — pulando Codex/OpenCode"
  fi
  echo "-- Antigravity (Google)"
  curl -fsSL https://antigravity.google/cli/install.sh | bash
  echo "-- Kimi Code"
  curl -LsSf https://code.kimi.com/install.sh | bash
  echo
  echo "Login de cada um (faça você mesmo, é interativo):"
  echo "  codex login   ·   kimi login   ·   agy (primeiro uso já pede)   ·   opencode auth login"
}

echo "==> Rust"
verificar_rust
echo
echo "==> Node.js"
verificar_node
echo
if [ "$acao" != "verificar" ]; then
  instalar_pacotes_sistema
  echo
fi
echo "==> dependências do web/"
verificar_npm_web
echo

if $provedores; then
  if [ "$acao" = "verificar" ]; then
    amarelo "==> --provedores pede --instalar ou --atualizar junto (só verificar não instala nada)"
  else
    instalar_provedores
  fi
  echo
fi

if [ "$faltando" -gt 0 ]; then
  echo
  vermelho "faltam $faltando dependência(s) — rode de novo com --instalar."
  exit 1
fi
verde "tudo certo."
