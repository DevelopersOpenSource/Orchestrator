# Empacotamento e Distribuição

> **Nota:** este documento é do desenho inicial do projeto (fase 1–2) e não foi atualizado — várias partes já mudaram (TUI, app desktop, `/provedor`, o revisor de decisões). Veja o [README](../README.md) para o estado atual e o `TRAVAMENTOS.md` para decisões e armadilhas registradas ao longo do caminho. O resto deste arquivo é o plano ORIGINAL (histórico); como o empacotamento é feito DE VERDADE hoje está aqui embaixo:

## Como é feito hoje (Linux)

Cinco binários: `orchestrator` (TUI — sozinho, sem subcomando, já abre a
interface, como `claude`), `orchestrator-desktop` (o app), `orchestrator-hook`
e `orchestrator-mcp` (a trava, instalados por projeto) e `orchestrator-memoryd`
(memória semântica, embute a página HTML dela).

- **`scripts/instalar-dependencias.sh`** — verifica, instala (`--instalar`) ou
  atualiza (`--atualizar`) o Rust, o Node.js e as bibliotecas do WebKitGTK/Tauri
  que o app desktop precisa; `--provedores` também instala as CLIs de IA
  opcionais (Codex, Kimi, Antigravity, OpenCode).
- **`scripts/empacotar-linux.sh`** — AppImage, `.deb` e `.rpm` via `tauri build`
  (é o que roda no CI, `.github/workflows/desktop.yml`).
- **`scripts/empacotar-portatil-linux.sh`** — uma pasta/tarball com os cinco
  binários prontos + `rodar.sh` + `instalar-dependencias.sh`, sem instalar nada
  e sem precisar de FUSE (o AppImage precisa) — útil em container/sandbox.
- **`scripts/rodar.sh`** — o lançador da versão portátil: abre o app com tela
  gráfica disponível, a TUI sem, ou o que `--tui`/`--app` mandar.

**Windows**: ainda não — bloqueios concretos (socket Unix, `killpg` POSIX,
ChromaDB via podman) documentados no job `windows` de
`.github/workflows/desktop.yml`. Fica para uma próxima etapa, para não sair
uma versão quebrada.

--------------------------------------------------------------------------------

## Plano original (histórico, fase 1–2)

Binários alvo: `orchestrator` (CLI), `orchestrator-service` (serviço de fundo), `orchestrator-hook`, e futuramente `orchestrator-mcp`.

## Linux — .deb (Ubuntu/Debian)

```sh
cargo install cargo-deb
cargo deb -p cli        # gera target/debian/*.deb
```

Metadados no `Cargo.toml` do crate:

```toml
[package.metadata.deb]
maintainer = "sodre"
depends = "$auto"
assets = [
    ["target/release/orchestrator",  "usr/bin/", "755"],
    ["target/release/orchestrator-service", "usr/bin/", "755"],
    ["target/release/orchestrator-hook", "usr/bin/", "755"],
    ["../../packaging/systemd/orchestrator-service.service", "usr/lib/systemd/user/", "644"],
]
```

## Linux — .rpm (Fedora/Nobara)

```sh
cargo install cargo-generate-rpm
cargo build --release
cargo generate-rpm -p crates/cli   # gera target/generate-rpm/*.rpm
```

```toml
[package.metadata.generate-rpm]
assets = [
    { source = "target/release/orchestrator",  dest = "/usr/bin/orchestrator",  mode = "755" },
    { source = "target/release/orchestrator-service", dest = "/usr/bin/orchestrator-service", mode = "755" },
    { source = "target/release/orchestrator-hook", dest = "/usr/bin/orchestrator-hook", mode = "755" },
    { source = "packaging/systemd/orchestrator-service.service", dest = "/usr/lib/systemd/user/orchestrator-service.service", mode = "644" },
]
```

Assine os pacotes com GPG (`rpm --addsign` / `debsigs`) e publique checksums.

## Windows — MSI (cargo-wix) + serviço

```powershell
cargo install cargo-wix
cargo wix -p cli          # gera target/wix/*.msi (requer WiX Toolset)
```

O serviço roda via crate `windows-service` (registro no instalador MSI ou `orchestrator-service --install-service`). Assine o MSI com Authenticode (`signtool sign`).

Notificações desktop usam toast nativo; o socket Unix é substituído por named pipe.

## Cross-compile

Com [`cross`](https://github.com/cross-rs/cross) (usa containers, resolve o SQLite bundled sem toolchain do host):

```sh
cargo install cross
cross build --release --target x86_64-unknown-linux-gnu
cross build --release --target aarch64-unknown-linux-gnu
cross build --release --target x86_64-pc-windows-gnu
```

Observação: `rusqlite` com feature `bundled` compila o SQLite junto (só precisa de um C compiler do target); `fastembed` baixa o modelo ONNX em runtime, então o binário não embute o modelo.

## systemd --user

Unit real em `packaging/systemd/orchestrator-service.service`. Instalação manual:

```sh
mkdir -p ~/.config/systemd/user
cp packaging/systemd/orchestrator-service.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now orchestrator-service
journalctl --user -u orchestrator-service -f     # logs
```

Os pacotes .deb/.rpm instalam o unit em `/usr/lib/systemd/user/`; o usuário só roda `systemctl --user enable --now orchestrator-service`. Para o serviço subir sem login ativo: `loginctl enable-linger $USER`.
