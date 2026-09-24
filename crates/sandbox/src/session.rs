//! Uma sandbox viva: container + navegador + estado de navegação.
//!
//! Cada método é um passo de teste e devolve TEXTO pronto para o
//! orquestrador — já comprimido e já com o lembrete do que ele pode chamar
//! em seguida (inclusive o screenshot, que só sai quando pedido).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cdp::Cdp;

/// Tentativas de conexão com o navegador (250 ms entre elas).
const CONNECT_ATTEMPTS: usize = 160;
use crate::container::{self, Sandbox, SandboxSpec};
use crate::view::{action_script, read_script, Detail, PageView, Snapshot, HINT};

/// Sessão de teste do orquestrador.
pub struct Session {
    pub sandbox: Sandbox,
    cdp: Option<Cdp>,
    view: PageView,
    /// Onde os screenshots são gravados no host.
    shots_dir: PathBuf,
    shot_count: usize,
}

impl Session {
    /// Sobe a sandbox — ou readota a que já está de pé com este nome.
    ///
    /// Readotar importa porque o processo do MCP reinicia muito mais que o
    /// container: sem isso, um orquestrador reiniciado só conseguia
    /// `ui_stop` e esperar o Chromium subir de novo.
    ///
    /// Não conecta no navegador aqui: só no primeiro passo que precisa da
    /// página, para `ui_exec` não pagar a espera do Chromium.
    pub fn start(spec: &SandboxSpec, shots_dir: &Path) -> Result<Self> {
        let sandbox = container::up(spec)?;
        std::fs::create_dir_all(shots_dir).ok();
        Ok(Self {
            sandbox,
            cdp: None,
            view: PageView::new(),
            shots_dir: shots_dir.to_path_buf(),
            shot_count: 0,
        })
    }

    /// Conecta no navegador, tentando algumas vezes — o Chromium leva alguns
    /// segundos para abrir o DevTools depois que o container sobe.
    fn cdp(&mut self) -> Result<&mut Cdp> {
        if self.cdp.is_none() {
            // O Chromium leva vários segundos para abrir o DevTools na
            // primeira execução do container (perfil novo, fontes, GPU).
            let mut last = None;
            for _ in 0..CONNECT_ATTEMPTS {
                match Cdp::connect(&self.sandbox.cdp_url) {
                    Ok(c) => {
                        self.cdp = Some(c);
                        break;
                    }
                    Err(e) => {
                        last = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                }
            }
            if self.cdp.is_none() {
                return Err(last.unwrap_or_else(|| {
                    anyhow::anyhow!("não consegui falar com o navegador da sandbox")
                }));
            }
        }
        Ok(self.cdp.as_mut().expect("conectado acima"))
    }

    /// Abre um endereço e devolve a página lida como elementos.
    pub fn open(&mut self, url: &str) -> Result<String> {
        let url = normalize_url(url);
        self.view.reset();
        let cdp = self.cdp()?;
        cdp.navigate(&url)?;
        self.snapshot(Detail::Full)
    }

    /// Lê a página agora. `Detail::Changes` mostra só o que mudou.
    pub fn snapshot(&mut self, detail: Detail) -> Result<String> {
        let script = read_script();
        let cdp = self.cdp()?;
        let json = cdp.eval(&script)?;
        let snap = Snapshot::parse(&json)?;
        let console = cdp.take_console();
        let mut out = self.view.render(snap, detail);
        if !console.is_empty() {
            out.push_str(&format!("\nconsole:\n{}\n", tail(&console, 10)));
        }
        Ok(out)
    }

    /// Age num elemento pela referência e devolve o que mudou na página.
    pub fn act(&mut self, kind: &str, reference: &str, value: &str) -> Result<String> {
        let script = action_script(kind, reference, value);
        let cdp = self.cdp()?;
        let raw = cdp.eval(&script)?;
        let result: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("a ação não funcionou");
            return Ok(format!("{err}\n{HINT}"));
        }
        let alvo = result
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or(reference);
        // Dá um instante para a página reagir antes de reler.
        std::thread::sleep(std::time::Duration::from_millis(350));
        let depois = self.snapshot(Detail::Changes)?;
        Ok(format!("{kind} em \"{alvo}\" ✔\n{depois}"))
    }

    /// Salva um PNG da tela no host e devolve o caminho.
    ///
    /// A imagem NÃO entra no contexto: o orquestrador recebe o caminho e
    /// decide se vale abrir (com a tool `Read`). É o "pixel sob demanda".
    pub fn screenshot(&mut self) -> Result<PathBuf> {
        let cdp = self.cdp()?;
        let b64 = cdp.screenshot()?;
        let bytes = decode_base64(&b64).context("imagem do navegador ilegível")?;
        self.shot_count += 1;
        // Nome SANITIZADO: ele vem do orquestrador, e um ".." no meio
        // atravessaria a pasta de screenshots e gravaria bytes da página
        // renderizada em qualquer lugar que o usuário possa escrever.
        let path = self.shots_dir.join(format!(
            "{}-{:03}.png",
            container::sanitize(&self.sandbox.name),
            self.shot_count
        ));
        std::fs::write(&path, bytes).with_context(|| format!("gravando {}", path.display()))?;
        Ok(path)
    }

    /// Salva um PNG da tela SEMPRE no mesmo caminho (sobrescreve).
    ///
    /// É a "tela virtual" ao vivo: uma captura periódica em segundo plano
    /// não pode ir empilhando arquivo novo a cada 1.5s, então aqui não há
    /// contador — o chamador decide o caminho fixo (normalmente fora de
    /// `shots_dir`, para não se confundir com os screenshots pedidos pelo
    /// orquestrador via `ui_screenshot`).
    pub fn screenshot_to(&mut self, path: &Path) -> Result<()> {
        let cdp = self.cdp()?;
        let b64 = cdp.screenshot()?;
        let bytes = decode_base64(&b64).context("imagem do navegador ilegível")?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes).with_context(|| format!("gravando {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("publicando {}", path.display()))?;
        Ok(())
    }

    /// Roda um comando dentro da sandbox (testar binário, AppImage, script).
    pub fn exec(&mut self, argv: &[String]) -> Result<String> {
        let (out, ok) = container::exec(&self.sandbox.name, argv)?;
        let marca = if ok { "✔" } else { "✖ (saiu com erro)" };
        let corpo = if out.trim().is_empty() {
            "(sem saída)".to_string()
        } else {
            tail(&out.lines().map(str::to_string).collect::<Vec<_>>(), 40)
        };
        Ok(format!("{} {marca}\n{corpo}\n{HINT}", argv.join(" ")))
    }

    /// Uma linha dizendo onde este teste está rodando.
    ///
    /// Vai na frente da resposta quando a sandbox nasceu (ou foi readotada)
    /// no meio do passo: sem isso o orquestrador não saberia em que pasta
    /// acabou de mexer, nem que a escrita dele é descartável.
    pub fn resumo(&self) -> String {
        let verbo = if self.sandbox.adopted {
            "readotei"
        } else {
            "subi"
        };
        match &self.sandbox.workdir {
            Some(d) => format!(
                "{verbo} a sandbox \"{}\" (/work = {}, {})",
                self.sandbox.name,
                d.display(),
                self.sandbox.mount.descricao()
            ),
            // Sem montagem não há modo de que falar — e o que ele precisa
            // saber é que /work está VAZIO, senão conclui que a CLI não
            // produziu nada e refaz trabalho pronto.
            None => format!(
                "{verbo} a sandbox \"{}\" — nenhuma pasta do host montada, \
                 /work está vazio (passe `workdir` no ui_open)",
                self.sandbox.name
            ),
        }
    }

    /// Encerra a sandbox. `false` se ela já não estava de pé.
    pub fn stop(&mut self) -> Result<bool> {
        self.cdp = None;
        container::stop(&self.sandbox.name)
    }
}

/// Aceita "localhost:3000", "exemplo.com" e caminhos de arquivo.
pub fn normalize_url(raw: &str) -> String {
    let raw = raw.trim();
    if raw.starts_with("http://")
        || raw.starts_with("https://")
        || raw.starts_with("file://")
        || raw.starts_with("about:")
    {
        return raw.to_string();
    }
    if raw.starts_with('/') {
        return format!("file://{raw}");
    }
    format!("http://{raw}")
}

/// Últimas `n` linhas, avisando o que ficou de fora.
fn tail(lines: &[String], n: usize) -> String {
    if lines.len() <= n {
        return lines.join("\n");
    }
    let start = lines.len() - n;
    format!(
        "(… {start} linhas antes)\n{}",
        lines[start..].join("\n")
    )
}

/// Decodifica base64 (o PNG vem assim do DevTools) sem puxar uma dependência.
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [255u8; 256];
    for (i, c) in TABLE.iter().enumerate() {
        rev[*c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for c in input.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = rev[c as usize];
        if v == 255 {
            return None;
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::container::{Mount, Sandbox};

    fn sessao_fake(dir: Option<&str>, mount: Mount, adopted: bool) -> Session {
        Session {
            sandbox: Sandbox {
                name: "t".into(),
                container: "orch-sbx-t".into(),
                cdp_url: "http://127.0.0.1:5000".into(),
                port: 5000,
                workdir: dir.map(PathBuf::from),
                mount,
                adopted,
            },
            cdp: None,
            view: PageView::new(),
            shots_dir: PathBuf::from("/tmp/shots"),
            shot_count: 0,
        }
    }

    #[test]
    fn the_summary_never_promises_a_mode_for_a_folder_that_is_not_there() {
        let com = sessao_fake(Some("/home/eu/projeto"), Mount::Overlay, false);
        let t = com.resumo();
        assert!(t.starts_with("subi a sandbox"), "{t}");
        assert!(t.contains("/work = /home/eu/projeto"), "{t}");
        assert!(t.contains("descartável"), "{t}");

        // Sem pasta montada, dizer "escrita real na pasta do host" seria
        // contraditório: não há pasta do host nenhuma.
        let sem = sessao_fake(None, Mount::ReadWrite, true);
        let t = sem.resumo();
        assert!(t.starts_with("readotei a sandbox"), "{t}");
        assert!(t.contains("vazio"), "{t}");
        assert!(!t.contains("escrita real"), "{t}");
    }

    #[test]
    fn normalize_url_accepts_what_a_person_would_type() {
        assert_eq!(normalize_url("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize_url("exemplo.com/a"), "http://exemplo.com/a");
        assert_eq!(normalize_url("https://x.com"), "https://x.com");
        assert_eq!(normalize_url("/work/index.html"), "file:///work/index.html");
        assert_eq!(normalize_url("  localhost:8080 "), "http://localhost:8080");
    }

    #[test]
    fn base64_decodes_a_known_payload() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        // PNG começa com esta assinatura; garante que bytes altos passam.
        let png = decode_base64("iVBORw0KGgo=").unwrap();
        assert_eq!(&png[..4], &[0x89, 0x50, 0x4e, 0x47]);
        assert!(decode_base64("nao*base64").is_none());
    }

    #[test]
    fn tail_keeps_the_end_and_says_what_was_cut() {
        let lines: Vec<String> = (1..=50).map(|i| format!("l{i}")).collect();
        let out = tail(&lines, 5);
        assert!(out.starts_with("(… 45 linhas antes)"));
        assert!(out.ends_with("l50"));
        assert_eq!(tail(&lines[..3], 5), "l1\nl2\nl3");
    }
}
