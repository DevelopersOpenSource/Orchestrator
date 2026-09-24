//! Ciclo de vida da sandbox em container (podman).
//!
//! A sandbox existe para o orquestrador PODER TESTAR o que as CLIs
//! produziram — abrir a página, clicar, rodar o binário — sem acesso à
//! máquina do usuário nem à tela dele. Tudo roda dentro do container, num
//! display virtual; o que sai de lá é texto (e um PNG quando pedido).
//!
//! Usamos o `podman` pela linha de comando em vez de uma API: é o que já
//! está instalado, funciona rootless e deixa o usuário inspecionar/matar a
//! sandbox com os comandos que ele já conhece.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Nome da imagem construída a partir de `packaging/sandbox/Containerfile`.
pub const IMAGE: &str = "orchestrator-sandbox";

/// Porta do DevTools Protocol dentro do container.
const CDP_PORT: u16 = 9222;

/// Label onde guardamos a pasta do host montada em `/work`.
///
/// Precisa ser label porque com o overlay (`:O`) o `podman inspect .Mounts`
/// devolve o diretório de merge do storage, não o caminho original — quem
/// readota uma sandbox não teria como saber sobre qual pasta ela está.
const LABEL_WORKDIR: &str = "orch.workdir";

/// Label com o modo de montagem, para readotar sabendo se escrita persiste.
const LABEL_MOUNT: &str = "orch.mount";

/// Label que marca "este container é uma sandbox desta versão".
///
/// Sem ela não havia como distinguir "sandbox sem pasta montada" de
/// "container de outra versão cujos labels eu não sei ler" — e readotar o
/// segundo fazia a sandbox AFIRMAR a pasta e o modo que foram pedidos, que é
/// exatamente a mentira que as guardas existem para impedir.
const LABEL_MARCA: &str = "orch.sandbox";

/// Prazo de um comando dentro da sandbox, em segundos.
///
/// Sem prazo, um comando que não termina (o jeito mais fácil de cair nisso é
/// subir um servidor em primeiro plano) congelava o processo MCP INTEIRO: ele
/// atende uma requisição por vez, então nem o `ui_stop` — a única saída —
/// chegava a ser lido. O corte acontece DENTRO do container, com o `timeout`
/// do coreutils, porque matar o `podman exec` daqui deixaria o processo vivo
/// lá dentro.
const EXEC_TIMEOUT: u64 = 120;

/// Código com que o `timeout` do coreutils avisa que cortou.
const RC_TIMEOUT: i32 = 124;

/// Quantas sandboxes podem estar de pé ao mesmo tempo.
///
/// Cada uma carrega Chromium + Xvfb e 512 MB de `/dev/shm`. Como `ui_exec`
/// passou a criar sandbox sozinha, um nome digitado diferente virava um
/// container novo e ninguém segurava a conta.
pub const MAX_SANDBOXES: usize = 3;

/// Como a pasta do host aparece dentro da sandbox, em `/work`.
///
/// O padrão é [`Mount::Overlay`] por um motivo medido em teste: verificar de
/// verdade quase sempre grava algo (banco, log, arquivo de saída), e com
/// `:ro` o teste morria em "read-only file system" — o orquestrador gastava
/// passos copiando tudo para `/tmp` antes de rodar. Com o overlay do podman
/// ele escreve à vontade DENTRO da sandbox e nada disso chega ao host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mount {
    /// `:O` — escrita livre dentro da sandbox, descartada com o container.
    #[default]
    Overlay,
    /// `:ro` — nem dentro da sandbox se escreve.
    ReadOnly,
    /// `:rw` — o que a sandbox gravar FICA na pasta do host.
    ReadWrite,
}

impl Mount {
    /// Opções do `-v` para este modo.
    ///
    /// `:O` não pode vir acompanhado de `:Z` — o podman recusa com
    /// "can only specify 1 'z', 'Z', or 'O' option"; o overlay já entra
    /// rotulado para o SELinux sozinho.
    pub fn option(self) -> &'static str {
        match self {
            Mount::Overlay => "O",
            Mount::ReadOnly => "ro,Z",
            Mount::ReadWrite => "rw,Z",
        }
    }

    /// Como explicar este modo para quem lê a resposta da tool.
    pub fn descricao(self) -> &'static str {
        match self {
            Mount::Overlay => "escrita descartável (a pasta do host não muda)",
            Mount::ReadOnly => "somente leitura",
            Mount::ReadWrite => "escrita real na pasta do host",
        }
    }

    /// Volta de [`Mount::option`] para o modo (usado ao readotar).
    pub fn from_option(raw: &str) -> Option<Self> {
        match raw.trim() {
            "O" => Some(Mount::Overlay),
            "ro,Z" | "ro" => Some(Mount::ReadOnly),
            "rw,Z" | "rw" => Some(Mount::ReadWrite),
            _ => None,
        }
    }
}

/// Como uma sandbox foi pedida.
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// Nome único (vira o nome do container: `orch-sbx-<nome>`).
    pub name: String,
    /// Pasta do host montada em `/work` — o que a CLI produziu.
    pub workdir: Option<PathBuf>,
    /// Como `workdir` é montada. Padrão: overlay descartável.
    pub mount: Mount,
    /// Porta do host que expõe o CDP (0 = escolhe uma livre).
    pub port: u16,
    /// Deixa rodar containers DENTRO da sandbox (`docker`/`podman`, compose)
    /// — para testar projeto que sobe banco, fila etc. em container. Troca o
    /// "sem capability nenhuma" por `--privileged`, que no podman ROOTLESS
    /// continua limitado ao usuário do dono (não vira root da máquina).
    pub containers: bool,
}

impl SandboxSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            workdir: None,
            mount: Mount::default(),
            port: 0,
            containers: false,
        }
    }
}

/// Uma sandbox em execução.
#[derive(Debug, Clone)]
pub struct Sandbox {
    pub name: String,
    pub container: String,
    /// Endereço do DevTools no host (`http://127.0.0.1:<porta>`).
    pub cdp_url: String,
    pub port: u16,
    /// Pasta do host em `/work`, se houver.
    pub workdir: Option<PathBuf>,
    /// Modo de montagem em vigor nesta sandbox.
    pub mount: Mount,
    /// `true` quando esta sandbox já estava de pé e foi readotada.
    pub adopted: bool,
}

/// Nome do container de uma sandbox.
///
/// Prefixado pelo projeto ativo (`ORCHESTRATOR_PROJECT`, exportado pelo
/// Orchestrator para toda CLI e para o próprio chat do orquestrador): o
/// nome do container é GLOBAL na máquina — sem o prefixo, uma CLI do
/// projeto A e outra do projeto B que escolhem o mesmo nome de sandbox
/// (ex.: "teste") colidiriam no MESMO container. O nome que o chamador vê
/// (`Sandbox.name`/respostas de `ui_*`) continua o nome cru que ele pediu —
/// só o identificador no podman ganha o prefixo.
pub fn container_name(name: &str) -> String {
    match std::env::var("ORCHESTRATOR_PROJECT") {
        Ok(p) if !p.trim().is_empty() => {
            format!("orch-sbx-{}-{}", sanitize(&p), sanitize(name))
        }
        _ => format!("orch-sbx-{}", sanitize(name)),
    }
}

/// Reduz o nome a algo que o podman aceita como nome de container.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_lowercase();
    if trimmed.is_empty() {
        "sandbox".to_string()
    } else {
        trimmed.chars().take(40).collect()
    }
}

/// Argumentos do `podman run` para uma sandbox.
///
/// Separado da execução para poder ser verificado em teste: é aqui que mora
/// o isolamento (sem privilégio, sem capabilities, pasta do projeto intocada
/// por padrão). Rede NÃO é isolada: a sandbox tem saída para a internet e
/// alcança serviço do host que escute em todas as interfaces (medido — só o
/// que escuta em 127.0.0.1 fica fora de alcance).
pub fn run_args(spec: &SandboxSpec, port: u16) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        container_name(&spec.name),
        // Memória compartilhada do Chromium.
        "--shm-size".into(),
        "512m".into(),
        // Do host para dentro, só o CDP é publicado, e só no localhost.
        "-p".into(),
        format!("127.0.0.1:{port}:{CDP_PORT}"),
    ];
    if spec.containers {
        // Container dentro do container precisa de user namespace e /dev/fuse.
        args.push("--privileged".into());
        args.push("--device".into());
        args.push("/dev/fuse".into());
    } else {
        // A sandbox comum não precisa de privilégio nenhum.
        args.push("--security-opt".into());
        args.push("no-new-privileges".into());
        args.push("--cap-drop".into());
        args.push("ALL".into());
    }
    // A marca vai sempre, inclusive sem pasta montada: é ela que diz que a
    // ausência dos outros labels é informação, e não ignorância.
    args.push("--label".into());
    args.push(format!("{LABEL_MARCA}=1"));
    if let Some(dir) = &spec.workdir {
        // No podman rootless o usuário do container é mapeado para um subuid
        // do host; sem `keep-id` ele não atravessa uma pasta 700 do usuário.
        args.push("--userns".into());
        args.push("keep-id".into());
        // Registrado em label porque o `:O` esconde a pasta original no
        // `inspect` (ver LABEL_WORKDIR).
        args.push("--label".into());
        args.push(format!("{LABEL_WORKDIR}={}", dir.display()));
        args.push("--label".into());
        args.push(format!("{LABEL_MOUNT}={}", spec.mount.option()));
        args.push("-v".into());
        args.push(format!("{}:/work:{}", dir.display(), spec.mount.option()));
    }
    args.push(IMAGE.into());
    args
}

/// Roda `podman` com os argumentos dados e devolve o stdout.
fn podman(args: &[String]) -> Result<String> {
    let out = Command::new("podman")
        .args(args)
        .output()
        .context("não consegui executar `podman` (está instalado?)")?;
    if !out.status.success() {
        bail!(
            "podman {}: {}",
            args.first().cloned().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A imagem da sandbox já foi construída?
pub fn image_exists() -> bool {
    podman(&["image".into(), "exists".into(), IMAGE.into()]).is_ok()
}

/// Constrói a imagem a partir do `Containerfile` do projeto.
///
/// Demora vários minutos na primeira vez (baixa Debian + Chromium), então
/// quem chama deve avisar o usuário.
pub fn build_image(repo_root: &Path) -> Result<String> {
    let containerfile = repo_root.join("packaging/sandbox/Containerfile");
    if !containerfile.is_file() {
        bail!(
            "não achei {} — a imagem da sandbox é construída a partir dele",
            containerfile.display()
        );
    }
    podman(&[
        "build".into(),
        "-t".into(),
        IMAGE.into(),
        "-f".into(),
        containerfile.display().to_string(),
        repo_root.display().to_string(),
    ])
}

/// Existe um container com este nome — de pé ou morto.
pub fn exists(name: &str) -> bool {
    podman(&["container".into(), "exists".into(), container_name(name)]).is_ok()
}

/// A sandbox com este nome está DE PÉ?
///
/// `podman container exists` não serve aqui: ele também responde "sim" para
/// um container parado, e readotar um morto levaria a um beco sem saída
/// (sem porta publicada, sem navegador, e o `run` seguinte recusando o nome).
pub fn is_running(name: &str) -> bool {
    podman(&[
        "container".into(),
        "inspect".into(),
        "--format".into(),
        "{{.State.Running}}".into(),
        container_name(name),
    ])
    .map(|s| s.trim() == "true")
    .unwrap_or(false)
}

/// Porta do host que publica o DevTools de uma sandbox de pé.
pub fn published_port(name: &str) -> Result<u16> {
    let out = podman(&[
        "port".into(),
        container_name(name),
        format!("{CDP_PORT}/tcp"),
    ])?;
    parse_published_port(&out)
        .with_context(|| format!("não entendi a porta publicada de \"{name}\": {out:?}"))
}

/// Extrai a porta de uma linha do `podman port` (`127.0.0.1:41763`).
fn parse_published_port(out: &str) -> Option<u16> {
    out.lines()
        .filter_map(|l| l.trim().rsplit_once(':'))
        .find_map(|(_, p)| p.trim().parse().ok())
}

/// O que os labels dizem sobre uma sandbox de pé: `(pasta, modo, é nossa)`.
pub fn labels(name: &str) -> Result<(Option<PathBuf>, Option<Mount>, bool)> {
    let out = podman(&[
        "inspect".into(),
        "--format".into(),
        format!(
            "{{{{index .Config.Labels \"{LABEL_WORKDIR}\"}}}}\t\
             {{{{index .Config.Labels \"{LABEL_MOUNT}\"}}}}\t\
             {{{{index .Config.Labels \"{LABEL_MARCA}\"}}}}"
        ),
        container_name(name),
    ])?;
    Ok(parse_labels(&out))
}

/// Separa a saída do `inspect` dos labels (campos vazios = label ausente).
///
/// Corta da DIREITA: só o caminho da pasta pode conter TAB, e o corte pela
/// esquerda partiria o caminho no meio.
fn parse_labels(out: &str) -> (Option<PathBuf>, Option<Mount>, bool) {
    let linha = out.lines().next().unwrap_or("");
    let (resto, marca) = linha.rsplit_once('\t').unwrap_or(("", linha));
    let (dir, modo) = resto.rsplit_once('\t').unwrap_or((resto, ""));
    let dir = dir.trim();
    (
        (!dir.is_empty()).then(|| PathBuf::from(dir)),
        Mount::from_option(modo),
        marca.trim() == "1",
    )
}

/// Readota uma sandbox que já está de pé.
///
/// Existe porque o processo do MCP reinicia muito mais que o container: o
/// orquestrador cai (ou é reiniciado) e a sandbox continua lá. Sem isto a
/// única saída era `ui_stop` e esperar o Chromium subir de novo.
///
/// Recusa quando a pasta montada não é a pedida: seguir em frente com
/// `/work` apontando para outro projeto daria um teste que parece certo e
/// mente.
pub fn attach(spec: &SandboxSpec) -> Result<Sandbox> {
    let port = published_port(&spec.name)?;
    let (montada, modo, nosso) = labels(&spec.name)?;
    if !nosso {
        bail!(
            "existe um container chamado \"{}\" que não foi criado por esta \
             versão do Orchestrator — não sei em que pasta nem em que modo ele \
             está, e chutar daria um teste que parece certo e mente. Encerre \
             com ui_stop e abra de novo.",
            container_name(&spec.name)
        );
    }
    let viva = Sandbox {
        name: spec.name.clone(),
        container: container_name(&spec.name),
        cdp_url: format!("http://127.0.0.1:{port}"),
        port,
        // Sem label de pasta numa sandbox NOSSA, a leitura é "não há pasta
        // montada" — nunca "deve ser a que pediram".
        workdir: montada,
        mount: modo.unwrap_or_default(),
        adopted: true,
    };
    serve(&viva, spec)?;
    Ok(viva)
}

/// A sandbox que já está de pé serve para o que está sendo pedido agora?
///
/// Vale nos dois reaproveitamentos — readotar o container de outro processo
/// e reusar a sessão deste. Nos dois, seguir em frente com `/work` em outra
/// pasta (ou prometendo uma escrita que não vai persistir) daria um teste
/// que parece certo e mente.
pub fn serve(viva: &Sandbox, spec: &SandboxSpec) -> Result<()> {
    match (&spec.workdir, &viva.workdir) {
        (Some(pedida), Some(atual)) if !mesma_pasta(pedida, atual) => bail!(
            "a sandbox \"{}\" já está de pé com /work em {} — e você pediu {}. \
             Encerre com ui_stop antes de abri-la na outra pasta.",
            viva.name,
            atual.display(),
            pedida.display()
        ),
        (Some(pedida), None) => bail!(
            "a sandbox \"{}\" está de pé SEM pasta do host montada (/work vazio) \
             — e você pediu {}. Encerre com ui_stop e abra de novo.",
            viva.name,
            pedida.display()
        ),
        _ => {}
    }
    // Modo só significa algo quando há pasta montada.
    if viva.workdir.is_some() && !atende(viva.mount, spec.mount) {
        bail!("{}", recusa_de_modo(&viva.name, viva.mount, spec.mount));
    }
    Ok(())
}

/// O modo que já está de pé atende ao que foi pedido?
///
/// Pedir escrita real numa sandbox descartável tem de falhar: o orquestrador
/// acharia que o resultado ficou gravado na pasta do usuário, e não ficou.
/// O contrário não falha — a resposta da tool já diz em que modo a sandbox
/// está, e recusar cada `ui_open` seguinte só atrapalharia.
pub fn atende(atual: Mount, pedido: Mount) -> bool {
    pedido != Mount::ReadWrite || atual == Mount::ReadWrite
}

/// A recusa quando o modo de pé não serve para o que foi pedido.
pub fn recusa_de_modo(name: &str, atual: Mount, pedido: Mount) -> String {
    format!(
        "a sandbox \"{name}\" está de pé com {} e você pediu {}. \
         O modo de montagem não muda com o container vivo: encerre com \
         ui_stop e abra de novo.",
        atual.descricao(),
        pedido.descricao()
    )
}

/// Garante uma sandbox de pé: sobe se não existe, readota se já existe.
pub fn up(spec: &SandboxSpec) -> Result<Sandbox> {
    if is_running(&spec.name) {
        return attach(spec);
    }
    // Um container morto com o mesmo nome faria o `run` recusar ("name
    // already in use") sem dizer que basta removê-lo. Ele não tem nada que
    // valha guardar — a escrita dele já foi descartada com o `--rm`.
    if exists(&spec.name) {
        let _ = podman(&["rm".into(), "-f".into(), container_name(&spec.name)]);
    }
    start(spec)
}

/// Sobe a sandbox e devolve por onde falar com ela.
pub fn start(spec: &SandboxSpec) -> Result<Sandbox> {
    if !image_exists() {
        bail!(
            "a imagem `{IMAGE}` ainda não existe — construa com \
             `podman build -t {IMAGE} -f packaging/sandbox/Containerfile .` \
             (demora alguns minutos na primeira vez)"
        );
    }
    if is_running(&spec.name) {
        bail!(
            "já existe uma sandbox chamada \"{}\" — use ui_stop para encerrá-la",
            spec.name
        );
    }
    let abertas = list().unwrap_or_default();
    if abertas.len() >= MAX_SANDBOXES {
        bail!("{}", recusa_por_teto(&abertas));
    }
    let port = if spec.port == 0 {
        free_port()?
    } else {
        spec.port
    };
    podman(&run_args(spec, port)).map_err(|e| explica_overlay(e, spec))?;
    Ok(Sandbox {
        name: spec.name.clone(),
        container: container_name(&spec.name),
        cdp_url: format!("http://127.0.0.1:{port}"),
        port,
        workdir: spec.workdir.clone(),
        mount: spec.mount,
        adopted: false,
    })
}

/// A recusa quando já há sandboxes demais de pé.
pub fn recusa_por_teto(abertas: &[Aberta]) -> String {
    let nomes: Vec<String> = abertas.iter().map(|a| a.name.clone()).collect();
    format!(
        "já há {} sandbox(es) de pé e o teto é {MAX_SANDBOXES}: {}. \
         Cada uma carrega um navegador inteiro — encerre uma com ui_stop \
         antes de abrir outra (confira se não é só o nome que está diferente).",
        abertas.len(),
        nomes.join(", ")
    )
}

/// Traduz a falha típica do overlay em algo acionável.
fn explica_overlay(e: anyhow::Error, spec: &SandboxSpec) -> anyhow::Error {
    let msg = e.to_string().to_lowercase();
    if spec.mount == Mount::Overlay && (msg.contains("overlay") || msg.contains("fuse")) {
        return e.context(
            "a montagem descartável (`:O`) depende do overlay do podman — \
             no modo rootless isso vem do pacote `fuse-overlayfs`. Confira com \
             `podman run --rm -v /tmp:/work:O <imagem> true`.",
        );
    }
    e
}

/// Mesma pasta, resolvendo link simbólico quando der.
fn mesma_pasta(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    norm(a) == norm(b)
}

/// Encerra a sandbox. Devolve `false` se ela já não estava de pé.
///
/// Encerrar o que já morreu não é erro — e o `--rm` faz esse instante
/// acontecer sempre: o `podman kill` volta assim que manda o sinal, e a
/// remoção termina depois. Quem só checou antes do kill pegava a corrida e
/// recebia um erro confuso.
pub fn stop(name: &str) -> Result<bool> {
    if !is_running(name) {
        return Ok(false);
    }
    match podman(&["kill".into(), container_name(name)]) {
        Ok(_) => Ok(true),
        // Sumiu entre a checagem e o kill: o `--rm` removeu em paralelo.
        Err(_) if !is_running(name) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Roda um comando DENTRO da sandbox e devolve `(stdout+stderr, sucesso)`.
///
/// É como o orquestrador testa um binário: `ui_exec` chega aqui.
pub fn exec(name: &str, argv: &[String]) -> Result<(String, bool)> {
    let out = Command::new("podman")
        .args(exec_args(name, argv))
        .output()
        .context("não consegui executar `podman exec`")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str(&format!("\n[stderr] {}", err.trim()));
    }
    if out.status.code() == Some(RC_TIMEOUT) {
        text.push_str(&format!("\n[cortei em {EXEC_TIMEOUT}s] {AVISO_TIMEOUT}"));
    }
    Ok((text, out.status.success()))
}

/// O que dizer a quem prendeu a sandbox num comando que não termina.
const AVISO_TIMEOUT: &str = "este comando não terminou sozinho. Serviço longo \
    (servidor web, `python3 -m http.server`, `node server.js`) tem de ir para SEGUNDO PLANO, senão a chamada \
    fica presa nele: [\"sh\",\"-c\",\"(setsid ./servidor >/tmp/srv.log 2>&1 &); \
    sleep 2; curl -s localhost:8080 | head -3\"].";

/// Argumentos do `podman exec`, com o prazo embutido.
pub fn exec_args(name: &str, argv: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "exec".into(),
        container_name(name),
        // `timeout` vem do coreutils, que está na imagem.
        "timeout".into(),
        EXEC_TIMEOUT.to_string(),
    ];
    args.extend(argv.iter().cloned());
    args
}

/// Copia um arquivo de dentro da sandbox para o host (screenshots).
pub fn copy_out(name: &str, inside: &str, dest: &Path) -> Result<()> {
    podman(&[
        "cp".into(),
        format!("{}:{inside}", container_name(name)),
        dest.display().to_string(),
    ])?;
    Ok(())
}

/// Quanto tempo de pé, sem o "ago" que o podman devolve em inglês.
fn desde(bruto: &str) -> String {
    bruto
        .trim()
        .strip_suffix(" ago")
        .unwrap_or(bruto.trim())
        .to_string()
}

/// Uma sandbox de pé, como mostrada na listagem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aberta {
    pub name: String,
    pub since: String,
    /// Pasta do host em `/work` (vem do label).
    pub workdir: Option<PathBuf>,
    /// Modo de montagem (vem do label). É o que distingue, na lista, uma
    /// sandbox descartável de uma aberta com escrita real.
    pub mount: Option<Mount>,
}

/// Lista as sandboxes de pé, dizendo em que pasta cada uma está.
pub fn list() -> Result<Vec<Aberta>> {
    let out = podman(&[
        "ps".into(),
        "--filter".into(),
        "name=orch-sbx-".into(),
        "--format".into(),
        // A pasta vai por ÚLTIMO: é o único campo que pode conter TAB.
        format!(
            "{{{{.Names}}}}\t{{{{.RunningFor}}}}\t\
             {{{{index .Labels \"{LABEL_MOUNT}\"}}}}\t\
             {{{{index .Labels \"{LABEL_WORKDIR}\"}}}}"
        ),
    ])?;
    Ok(parse_list(&out))
}

/// Separa a listagem do `podman ps` (nome, desde, pasta).
fn parse_list(out: &str) -> Vec<Aberta> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            // `splitn(4)`: a pasta é o último campo e pode conter TAB.
            let mut campos = l.splitn(4, '\t');
            let nome = campos.next().unwrap_or("").trim();
            let since = campos.next().unwrap_or("").trim();
            let modo = campos.next().unwrap_or("").trim();
            let dir = campos.next().unwrap_or("").trim();
            Aberta {
                name: nome
                    .strip_prefix("orch-sbx-")
                    .unwrap_or(nome)
                    .to_string(),
                since: desde(since),
                workdir: (!dir.is_empty()).then(|| PathBuf::from(dir)),
                mount: Mount::from_option(modo),
            }
        })
        .collect()
}

/// Uma porta livre no host, perguntando ao sistema operacional.
fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("procurando uma porta livre para o DevTools")?;
    Ok(listener.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_makes_a_valid_container_name() {
        // `container_name` lê ORCHESTRATOR_PROJECT — outro teste no mesmo
        // processo mexe nela, então garante que não sobrou nada aqui.
        std::env::remove_var("ORCHESTRATOR_PROJECT");
        assert_eq!(sanitize("frontend"), "frontend");
        assert_eq!(sanitize("Meu Teste!"), "meu-teste");
        assert_eq!(sanitize("--"), "sandbox");
        assert_eq!(sanitize(""), "sandbox");
        assert_eq!(sanitize(&"x".repeat(80)).len(), 40);
        assert_eq!(container_name("Meu Teste"), "orch-sbx-meu-teste");
    }

    #[test]
    fn container_name_is_isolated_per_project() {
        // Precisa ficar isolado do teste acima (ambos mexem no MESMO env var
        // do processo): garante que não sobrou nada de uma run anterior.
        std::env::remove_var("ORCHESTRATOR_PROJECT");
        assert_eq!(container_name("teste"), "orch-sbx-teste");
        std::env::set_var("ORCHESTRATOR_PROJECT", "Minha Loja");
        assert_eq!(container_name("teste"), "orch-sbx-minha-loja-teste");
        // Outro projeto com o MESMO nome de sandbox não pode virar o mesmo
        // container — é exatamente o conflito que isto existe para evitar.
        std::env::set_var("ORCHESTRATOR_PROJECT", "Outra Loja");
        assert_eq!(container_name("teste"), "orch-sbx-outra-loja-teste");
        std::env::remove_var("ORCHESTRATOR_PROJECT");
        // O nome também nomeia o PNG do screenshot no host: nem barra nem
        // ponto podem sobrar, senão um ".." no nome escaparia da pasta.
        let travessia = sanitize("../../home/eu/.config/autostart/x");
        assert!(!travessia.contains('/') && !travessia.contains('.'), "{travessia}");
        assert_eq!(sanitize(".."), "sandbox");
    }

    #[test]
    fn run_args_isolate_by_default() {
        let args = run_args(&SandboxSpec::new("t"), 5000);
        let joined = args.join(" ");
        // Sem privilégio e sem capabilities.
        assert!(joined.contains("no-new-privileges"));
        assert!(joined.contains("--cap-drop ALL"));
        // O CDP só escuta no localhost do host, nunca na rede.
        assert!(joined.contains("127.0.0.1:5000:9222"));
        // Sem pasta pedida, nada do host é montado.
        assert!(!joined.contains("-v"));
        assert!(joined.ends_with(IMAGE));
    }

    #[test]
    fn workdir_is_writable_inside_but_never_on_the_host() {
        let mut spec = SandboxSpec::new("t");
        spec.workdir = Some(PathBuf::from("/tmp/projeto"));
        let args = run_args(&spec, 5000).join(" ");
        // Overlay: o teste grava dentro da sandbox, o host fica intocado.
        assert!(
            args.contains("/tmp/projeto:/work:O"),
            "o padrão tem de ser o overlay descartável: {args}"
        );
        // `:O` com `:Z` o podman recusa.
        assert!(!args.contains(":O,Z"), "{args}");
        // Sem keep-id o container não lê pasta do usuário (rootless).
        assert!(args.contains("--userns keep-id"), "{args}");
        // A pasta fica registrada em label: é como readotar depois.
        assert!(args.contains("--label orch.workdir=/tmp/projeto"), "{args}");
        assert!(args.contains("--label orch.mount=O"), "{args}");
        // E a marca de versão vai sempre, com pasta ou sem.
        assert!(args.contains("--label orch.sandbox=1"), "{args}");
        let sem_pasta = run_args(&SandboxSpec::new("t"), 5000).join(" ");
        assert!(sem_pasta.contains("--label orch.sandbox=1"), "{sem_pasta}");
    }

    #[test]
    fn asking_for_real_writes_mounts_read_write() {
        let mut spec = SandboxSpec::new("t");
        spec.workdir = Some(PathBuf::from("/tmp/projeto"));
        spec.mount = Mount::ReadWrite;
        let args = run_args(&spec, 5000).join(" ");
        assert!(args.contains("/tmp/projeto:/work:rw,Z"), "{args}");
        assert!(args.contains("--label orch.mount=rw,Z"), "{args}");

        spec.mount = Mount::ReadOnly;
        let args = run_args(&spec, 5000).join(" ");
        assert!(args.contains("/tmp/projeto:/work:ro,Z"), "{args}");
    }

    #[test]
    fn mount_options_survive_a_round_trip() {
        for m in [Mount::Overlay, Mount::ReadOnly, Mount::ReadWrite] {
            assert_eq!(Mount::from_option(m.option()), Some(m), "{m:?}");
        }
        assert_eq!(Mount::from_option("ro"), Some(Mount::ReadOnly));
        assert_eq!(Mount::from_option(""), None);
        assert_eq!(Mount::from_option("z"), None);
        assert_eq!(Mount::default(), Mount::Overlay);
    }

    #[test]
    fn published_port_is_read_from_podman_output() {
        assert_eq!(parse_published_port("127.0.0.1:41763"), Some(41763));
        assert_eq!(parse_published_port("  127.0.0.1:41763 \n"), Some(41763));
        // Formato com a porta interna à esquerda.
        assert_eq!(
            parse_published_port("9222/tcp -> 127.0.0.1:41763"),
            Some(41763)
        );
        // IPv6 também aparece em algumas máquinas.
        assert_eq!(parse_published_port("[::]:41763"), Some(41763));
        assert_eq!(parse_published_port(""), None);
        assert_eq!(parse_published_port("nada aqui"), None);
    }

    #[test]
    fn parsers_survive_a_tab_inside_the_folder_name() {
        // Improvável, mas o estrago seria silencioso: a sandbox pareceria
        // montada em outra pasta e a readoção recusaria sem motivo.
        let (dir, modo, nosso) = parse_labels("/home/eu/pasta\tesquisita\tO\t1");
        assert_eq!(dir, Some(PathBuf::from("/home/eu/pasta\tesquisita")));
        assert_eq!(modo, Some(Mount::Overlay));
        assert!(nosso);

        let abertas = parse_list("orch-sbx-x\t2 minutes ago\tO\t/a\tb");
        assert_eq!(abertas[0].workdir, Some(PathBuf::from("/a\tb")));
    }

    #[test]
    fn a_container_without_our_mark_is_not_ours_to_adopt() {
        // Sandbox nossa, com pasta: os três labels vêm preenchidos.
        let (dir, modo, nosso) = parse_labels("/x\tO\t1");
        assert_eq!((dir, modo, nosso), (Some(PathBuf::from("/x")), Some(Mount::Overlay), true));
        // Sandbox nossa, sem pasta montada: ausência é INFORMAÇÃO.
        assert_eq!(parse_labels("\t\t1"), (None, None, true));
        // Container de outra versão (ou estranho): ignorância, não informação.
        assert_eq!(parse_labels("\t\t"), (None, None, false));
        assert_eq!(parse_labels(""), (None, None, false));
    }

    #[test]
    fn labels_tell_which_folder_and_mode_a_sandbox_uses() {
        let (dir, modo, nosso) = parse_labels("/home/eu/projeto\tO\t1");
        assert_eq!(dir, Some(PathBuf::from("/home/eu/projeto")));
        assert_eq!(modo, Some(Mount::Overlay));
        assert!(nosso);
        let (dir, modo, _) = parse_labels("/x\trw,Z\t1");
        assert_eq!(dir, Some(PathBuf::from("/x")));
        assert_eq!(modo, Some(Mount::ReadWrite));
    }

    #[test]
    fn listing_shows_the_folder_and_the_mode_of_each_sandbox() {
        let abertas = parse_list(
            "orch-sbx-conferencia\t2 minutes ago\trw,Z\t/home/eu/projeto\n\
             orch-sbx-solta\t5 seconds ago\t\t\n",
        );
        assert_eq!(
            abertas,
            vec![
                Aberta {
                    name: "conferencia".into(),
                    since: "2 minutes".into(),
                    workdir: Some(PathBuf::from("/home/eu/projeto")),
                    // O modo na lista é o que distingue uma sandbox
                    // descartável de uma que grava no projeto de verdade.
                    mount: Some(Mount::ReadWrite),
                },
                Aberta {
                    name: "solta".into(),
                    since: "5 seconds".into(),
                    workdir: None,
                    mount: None,
                },
            ]
        );
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn overlay_failures_say_what_to_install() {
        let spec = SandboxSpec::new("t");
        let e = explica_overlay(
            anyhow::anyhow!("podman run: mounting overlay failed: no fuse-overlayfs"),
            &spec,
        );
        let texto = format!("{e:#}");
        assert!(texto.contains("fuse-overlayfs"), "{texto}");
        // Erro de outra natureza passa inteiro, sem palpite.
        let e = explica_overlay(anyhow::anyhow!("porta 5000 ocupada"), &spec);
        assert_eq!(format!("{e:#}"), "porta 5000 ocupada");
    }

    #[test]
    fn same_folder_ignores_a_trailing_symlink() {
        assert!(mesma_pasta(Path::new("/tmp"), Path::new("/tmp")));
        assert!(!mesma_pasta(Path::new("/tmp/a"), Path::new("/tmp/b")));
    }

    /// Uma sandbox de pé, como `attach`/`ui_open` a veem.
    #[cfg(test)]
    fn sandbox_fake(dir: Option<&str>, mount: Mount) -> Sandbox {
        let _ = &MAX_SANDBOXES;
        Sandbox {
            name: "t".into(),
            container: container_name("t"),
            cdp_url: "http://127.0.0.1:5000".into(),
            port: 5000,
            workdir: dir.map(PathBuf::from),
            mount,
            adopted: true,
        }
    }

    #[test]
    fn reusing_a_sandbox_from_another_folder_is_refused() {
        let viva = sandbox_fake(Some("/tmp/projeto-a"), Mount::Overlay);
        let mut pedido = SandboxSpec::new("t");
        pedido.workdir = Some(PathBuf::from("/tmp/projeto-a"));
        assert!(serve(&viva, &pedido).is_ok());

        // Mesma sandbox, outra pasta: testar aqui daria um resultado que
        // parece certo e fala do projeto errado.
        pedido.workdir = Some(PathBuf::from("/tmp/projeto-b"));
        let e = format!("{:#}", serve(&viva, &pedido).unwrap_err());
        assert!(e.contains("projeto-a") && e.contains("projeto-b"), "{e}");
        assert!(e.contains("ui_stop"), "{e}");

        // Pedir escrita real sobre uma descartável viva também recusa.
        pedido.workdir = Some(PathBuf::from("/tmp/projeto-a"));
        pedido.mount = Mount::ReadWrite;
        let e = format!("{:#}", serve(&viva, &pedido).unwrap_err());
        assert!(e.contains("ui_stop"), "{e}");

        // Sandbox de pé SEM pasta montada não serve para quem pediu pasta:
        // /work estaria vazio e ele acharia que a CLI não produziu nada.
        let solta = sandbox_fake(None, Mount::Overlay);
        let mut qualquer = SandboxSpec::new("t");
        qualquer.workdir = Some(PathBuf::from("/tmp/projeto-b"));
        let e = format!("{:#}", serve(&solta, &qualquer).unwrap_err());
        assert!(e.contains("SEM pasta") && e.contains("ui_stop"), "{e}");
        // Mas serve para quem também não pediu pasta nenhuma.
        assert!(serve(&solta, &SandboxSpec::new("t")).is_ok());
    }

    #[test]
    fn a_command_inside_the_sandbox_runs_under_a_deadline() {
        // Sem prazo, um servidor em primeiro plano congelava o MCP inteiro
        // — inclusive o ui_stop, que era a única saída.
        let args = exec_args("conferencia", &["sh".into(), "-c".into(), "echo oi".into()]);
        assert_eq!(
            args,
            vec![
                "exec",
                "orch-sbx-conferencia",
                "timeout",
                "120",
                "sh",
                "-c",
                "echo oi"
            ]
        );
        // O prazo entra ANTES do comando, senão não vale para nada.
        let pos_timeout = args.iter().position(|a| a == "timeout").unwrap();
        let pos_cmd = args.iter().position(|a| a == "sh").unwrap();
        assert!(pos_timeout < pos_cmd);
        // E o aviso ensina o jeito certo de subir serviço.
        assert!(AVISO_TIMEOUT.contains("setsid"), "{AVISO_TIMEOUT}");
        assert!(AVISO_TIMEOUT.contains("SEGUNDO PLANO"), "{AVISO_TIMEOUT}");
    }

    #[test]
    fn too_many_sandboxes_is_refused_with_the_names() {
        let abertas: Vec<Aberta> = ["a", "b", "c"]
            .iter()
            .map(|n| Aberta {
                name: n.to_string(),
                since: "1 minute".into(),
                workdir: None,
                mount: None,
            })
            .collect();
        let r = recusa_por_teto(&abertas);
        // Precisa dizer QUAIS estão abertas: o caso comum é nome digitado
        // diferente criando container novo.
        assert!(r.contains("a, b, c"), "{r}");
        assert!(r.contains("ui_stop"), "{r}");
        assert_eq!(MAX_SANDBOXES, 3);
    }

    #[test]
    fn a_live_sandbox_only_refuses_when_the_request_is_stronger() {
        // Pedir escrita real numa descartável mentiria sobre o resultado.
        assert!(!atende(Mount::Overlay, Mount::ReadWrite));
        assert!(!atende(Mount::ReadOnly, Mount::ReadWrite));
        assert!(atende(Mount::ReadWrite, Mount::ReadWrite));
        // Pedir menos não é problema: a resposta diz o modo real.
        assert!(atende(Mount::ReadWrite, Mount::Overlay));
        assert!(atende(Mount::Overlay, Mount::Overlay));
        assert!(atende(Mount::Overlay, Mount::ReadOnly));

        let r = recusa_de_modo("t", Mount::Overlay, Mount::ReadWrite);
        assert!(r.contains("ui_stop"), "{r}");
        assert!(r.contains("descartável"), "{r}");
    }

    #[test]
    fn time_running_is_reported_without_the_english_ago() {
        assert_eq!(desde("20 seconds ago"), "20 seconds");
        assert_eq!(desde(" 2 minutes ago "), "2 minutes");
        assert_eq!(desde("Less than a second ago"), "Less than a second");
        assert_eq!(desde(""), "");
    }

    #[test]
    fn free_port_returns_something_usable() {
        let p = free_port().unwrap();
        assert!(p > 1024);
        // Duas chamadas seguidas não devolvem a mesma porta presa.
        assert!(std::net::TcpListener::bind(("127.0.0.1", p)).is_ok());
    }
}
