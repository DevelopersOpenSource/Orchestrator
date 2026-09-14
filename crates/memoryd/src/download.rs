//! Download dos modelos na primeira execução, com checksum.
//!
//! Os arquivos vêm de um COMMIT fixo do Hugging Face (a URL não muda de
//! conteúdo) e só entram na pasta depois de conferir tamanho e SHA-256. Um
//! download interrompido fica como `.part` e recomeça do zero na próxima vez.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Troca a origem dos arquivos (espelho ou servidor local).
pub const BASE_URL_ENV: &str = "ORCHESTRATOR_MODELS_URL";

/// Origem padrão.
pub const HUGGING_FACE: &str = "https://huggingface.co";

/// Um arquivo de modelo, fixado por commit e checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// Repositório no Hugging Face (`dono/nome`).
    pub repo: &'static str,
    pub commit: &'static str,
    /// Caminho dentro do repositório.
    pub path: &'static str,
    /// Nome com que o arquivo fica na pasta local.
    pub local: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

impl ModelFile {
    pub fn url(&self, base: &str) -> String {
        format!(
            "{}/{}/resolve/{}/{}",
            base.trim_end_matches('/'),
            self.repo,
            self.commit,
            self.path
        )
    }
}

/// Andamento do download, para o `health` e o app mostrarem.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    /// Arquivo sendo baixado agora.
    pub arquivo: String,
    pub baixado: u64,
    /// Soma do que falta baixar (0 = nada a baixar).
    pub total: u64,
    pub concluido: bool,
}

impl Progress {
    pub fn percent(&self) -> u8 {
        if self.total == 0 {
            return 100;
        }
        (self.baixado.saturating_mul(100) / self.total).min(100) as u8
    }
}

pub type SharedProgress = Arc<Mutex<Progress>>;

/// Garante os arquivos na pasta, baixando da origem padrão (ou da variável
/// [`BASE_URL_ENV`]) o que faltar.
pub async fn ensure(dir: &Path, files: &[ModelFile], progress: &SharedProgress) -> Result<()> {
    let base = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| HUGGING_FACE.to_string());
    ensure_from(&base, dir, files, progress).await
}

/// Como [`ensure`], com a origem explícita.
pub async fn ensure_from(
    base: &str,
    dir: &Path,
    files: &[ModelFile],
    progress: &SharedProgress,
) -> Result<()> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("criando {}", dir.display()))?;
    let faltam: Vec<&ModelFile> = files.iter().filter(|f| !pronto(dir, f)).collect();
    atualizar(progress, |p| {
        *p = Progress {
            total: faltam.iter().map(|f| f.size).sum(),
            ..Progress::default()
        }
    });
    if !faltam.is_empty() {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()?;
        for f in faltam {
            baixar(&client, base, dir, f, progress)
                .await
                .with_context(|| format!("baixando {}/{}", f.repo, f.path))?;
        }
    }
    atualizar(progress, |p| p.concluido = true);
    Ok(())
}

/// O arquivo já está na pasta, com o tamanho certo e conferido antes?
pub fn pronto(dir: &Path, f: &ModelFile) -> bool {
    let tamanho_ok = std::fs::metadata(dir.join(f.local))
        .map(|m| m.len() == f.size)
        .unwrap_or(false);
    tamanho_ok
        && std::fs::read_to_string(marca(dir, f))
            .map(|s| s.trim() == f.sha256)
            .unwrap_or(false)
}

/// Checksum gravado ao lado do arquivo depois de conferido — evita refazer o
/// SHA-256 de centenas de MB a cada abertura.
fn marca(dir: &Path, f: &ModelFile) -> PathBuf {
    let destino = dir.join(f.local);
    let nome = destino.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    destino.with_file_name(format!(".{nome}.sha256"))
}

fn atualizar(progress: &SharedProgress, f: impl FnOnce(&mut Progress)) {
    if let Ok(mut p) = progress.lock() {
        f(&mut p);
    }
}

async fn baixar(
    client: &reqwest::Client,
    base: &str,
    dir: &Path,
    f: &ModelFile,
    progress: &SharedProgress,
) -> Result<()> {
    let destino = dir.join(f.local);
    let nome = destino.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parcial = destino.with_file_name(format!("{nome}.part"));
    if let Some(pasta) = destino.parent() {
        tokio::fs::create_dir_all(pasta).await?;
    }
    atualizar(progress, |p| p.arquivo = f.path.to_string());

    let resultado = async {
        let mut resp = client.get(f.url(base)).send().await?.error_for_status()?;
        let mut out = tokio::fs::File::create(&parcial).await?;
        let mut hasher = Sha256::new();
        let mut recebido = 0u64;
        while let Some(pedaco) = resp.chunk().await? {
            recebido += pedaco.len() as u64;
            if recebido > f.size {
                bail!("veio maior que os {} bytes esperados", f.size);
            }
            hasher.update(&pedaco);
            out.write_all(&pedaco).await?;
            atualizar(progress, |p| p.baixado += pedaco.len() as u64);
        }
        out.flush().await?;
        drop(out);
        let sha = format!("{:x}", hasher.finalize());
        if recebido != f.size || sha != f.sha256 {
            bail!(
                "checksum não confere: esperado {} com {} bytes, veio {sha} com {recebido} bytes",
                f.sha256,
                f.size
            );
        }
        Ok(())
    }
    .await;

    if let Err(e) = resultado {
        let _ = tokio::fs::remove_file(&parcial).await;
        return Err(e);
    }
    tokio::fs::rename(&parcial, &destino).await?;
    tokio::fs::write(marca(dir, f), f.sha256).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// Servidor HTTP mínimo que devolve sempre o mesmo corpo.
    async fn servidor(corpo: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let h = tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let corpo = corpo.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf).await;
                    let cab = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        corpo.len()
                    );
                    let _ = s.write_all(cab.as_bytes()).await;
                    let _ = s.write_all(&corpo).await;
                });
            }
        });
        (format!("http://{addr}"), h)
    }

    fn arquivo(sha256: String, size: u64) -> ModelFile {
        ModelFile {
            repo: "teste/modelo",
            commit: "0123abcd",
            path: "onnx/modelo.onnx",
            local: "modelo.onnx",
            sha256: Box::leak(sha256.into_boxed_str()),
            size,
        }
    }

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[tokio::test]
    async fn downloads_verifies_and_then_skips_what_is_already_there() {
        let corpo = b"pesos de mentira do modelo".to_vec();
        let (base, servidor) = servidor(corpo.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let f = arquivo(sha(&corpo), corpo.len() as u64);
        let progress = SharedProgress::default();

        ensure_from(&base, dir.path(), &[f], &progress).await.unwrap();
        assert_eq!(std::fs::read(dir.path().join("modelo.onnx")).unwrap(), corpo);
        assert!(pronto(dir.path(), &f));
        {
            let p = progress.lock().unwrap();
            assert!(p.concluido);
            assert_eq!(p.percent(), 100);
        }

        // Sem servidor: o que já foi conferido não é baixado de novo.
        servidor.abort();
        ensure_from(&base, dir.path(), &[f], &progress).await.unwrap();
    }

    #[tokio::test]
    async fn a_wrong_checksum_is_rejected_and_nothing_is_kept() {
        let corpo = b"arquivo adulterado".to_vec();
        let (base, _s) = servidor(corpo.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let f = arquivo("0".repeat(64), corpo.len() as u64);

        let erro = ensure_from(&base, dir.path(), &[f], &SharedProgress::default())
            .await
            .unwrap_err();
        assert!(format!("{erro:#}").contains("checksum"), "{erro:#}");
        assert!(!dir.path().join("modelo.onnx").exists());
        assert!(!dir.path().join("modelo.onnx.part").exists());
        assert!(!pronto(dir.path(), &f));
    }

    #[tokio::test]
    async fn a_download_of_the_wrong_size_is_rejected() {
        let corpo = b"curto demais".to_vec();
        let (base, _s) = servidor(corpo.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let f = arquivo(sha(&corpo), corpo.len() as u64 + 10);

        assert!(ensure_from(&base, dir.path(), &[f], &SharedProgress::default())
            .await
            .is_err());
        assert!(!dir.path().join("modelo.onnx").exists());
    }

    #[test]
    fn urls_point_to_a_fixed_commit() {
        let f = arquivo("a".repeat(64), 1);
        assert_eq!(
            f.url("https://huggingface.co/"),
            "https://huggingface.co/teste/modelo/resolve/0123abcd/onnx/modelo.onnx"
        );
    }
}
