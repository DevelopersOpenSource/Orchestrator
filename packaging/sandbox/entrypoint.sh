#!/bin/sh
# Sobe a tela virtual e o navegador com o DevTools Protocol aberto, depois
# fica vivo esperando comandos (`podman exec`) do Orchestrator.
set -e

# Perfil do navegador FORA de /work: lá é a pasta do usuário, montada em
# overlay descartável, e o perfil não tem por que disputar espaço com ela.
PROFILE=/home/orch/chrome-profile
mkdir -p "$PROFILE"

Xvfb "$DISPLAY" -screen 0 "$SCREEN" -nolisten tcp &
# Espera a tela existir antes de abrir qualquer janela.
for _ in $(seq 1 50); do
    xdpyinfo -display "$DISPLAY" >/dev/null 2>&1 && break
    sleep 0.1
done

# --no-sandbox: o sandbox interno do Chrome precisa de privilégios que este
# container não tem (e não deve ter). O isolamento aqui é o container em si:
# sem capabilities e, por padrão, o que se escreve em /work morre com o
# container (overlay). Rede não é isolada — há saída para a internet e acesso
# a serviço do host que escute em todas as interfaces.
chromium \
    --no-sandbox \
    --disable-setuid-sandbox \
    --remote-debugging-port=9223 \
    --no-first-run \
    --no-default-browser-check \
    --disable-dev-shm-usage \
    --disable-gpu \
    --window-size=1280,800 \
    --user-data-dir=$PROFILE \
    about:blank &

# O Chrome só aceita DevTools vindo de localhost (proteção contra DNS
# rebinding) e ignora --remote-debugging-address. Como o orquestrador fala de
# fora do container, encaminhamos: 9222 (publicada) → 9223 (onde o Chrome
# escuta), e para o Chrome a conexão continua vindo de 127.0.0.1.
# Espera o DevTools ACEITAR conexão (o arquivo de porta nem sempre existe):
# sem isso o encaminhador aceitaria conexões e as resetaria na hora.
for _ in $(seq 1 300); do
    if socat -u /dev/null TCP:127.0.0.1:9223,connect-timeout=1 2>/dev/null; then
        break
    fi
    sleep 0.2
done
socat TCP-LISTEN:9222,fork,reuseaddr TCP:127.0.0.1:9223 &

# Mantém o container vivo; o Orchestrator age por CDP e `podman exec`.
tail -f /dev/null
