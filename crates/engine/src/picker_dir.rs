//! Seletor de pasta do SISTEMA, para escolher o projeto pelo explorador de
//! arquivos em vez de digitar o caminho.
//!
//! Digitar caminho na mão é onde o usuário mais erra (e não dá para navegar
//! nem ver o que existe). Aqui chamamos o diálogo nativo do ambiente: KDE
//! (`kdialog`), GNOME e afins (`zenity`/`yad`) ou, no Windows, o seletor do
//! próprio Explorer via PowerShell. Sem nenhum deles, quem chama cai de volta
//! no caminho digitado — a função diz qual foi o caso.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Por que não abriu um seletor, quando não abriu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickError {
    /// O usuário fechou/cancelou o diálogo.
    Cancelled,
    /// Nenhum seletor gráfico disponível nesta máquina.
    NoPicker(String),
    /// O seletor existia mas falhou.
    Failed(String),
}

impl PickError {
    pub fn message(&self) -> String {
        match self {
            PickError::Cancelled => "seleção cancelada".to_string(),
            PickError::NoPicker(dicas) => format!(
                "não achei um seletor de pastas nesta máquina ({dicas}) — \
                 informe o caminho no comando"
            ),
            PickError::Failed(e) => format!("o seletor de pastas falhou: {e}"),
        }
    }
}

/// Candidatos a seletor, na ordem de preferência para o ambiente atual.
///
/// Separado da execução para poder ser testado: a ordem importa (num desktop
/// KDE, abrir um diálogo GTK fica visualmente deslocado).
pub fn candidates(desktop: &str) -> Vec<&'static str> {
    let kde = desktop.to_lowercase().contains("kde");
    if kde {
        vec!["kdialog", "zenity", "yad"]
    } else {
        vec!["zenity", "yad", "kdialog"]
    }
}

/// Monta o comando de cada seletor.
pub fn command_for(programa: &str, titulo: &str, inicio: &str) -> Option<(String, Vec<String>)> {
    match programa {
        "kdialog" => Some((
            "kdialog".into(),
            vec![
                "--title".into(),
                titulo.into(),
                "--getexistingdirectory".into(),
                inicio.into(),
            ],
        )),
        "zenity" => Some((
            "zenity".into(),
            vec![
                "--file-selection".into(),
                "--directory".into(),
                format!("--title={titulo}"),
                format!("--filename={}/", inicio.trim_end_matches('/')),
            ],
        )),
        "yad" => Some((
            "yad".into(),
            vec![
                "--file".into(),
                "--directory".into(),
                format!("--title={titulo}"),
                format!("--filename={}/", inicio.trim_end_matches('/')),
            ],
        )),
        _ => None,
    }
}

/// Abre o seletor de pastas do sistema e devolve o que foi escolhido.
pub fn pick_directory(titulo: &str, inicio: &Path) -> Result<PathBuf, PickError> {
    let inicio_str = inicio.display().to_string();

    #[cfg(windows)]
    {
        return pick_directory_windows(titulo, &inicio_str);
    }

    #[cfg(not(windows))]
    {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let tentados = candidates(&desktop);
        for programa in &tentados {
            if which(programa).is_none() {
                continue;
            }
            let Some((cmd, args)) = command_for(programa, titulo, &inicio_str) else {
                continue;
            };
            let saida = Command::new(&cmd)
                .args(&args)
                .output()
                .map_err(|e| PickError::Failed(e.to_string()))?;
            let escolhido = String::from_utf8_lossy(&saida.stdout).trim().to_string();
            if escolhido.is_empty() {
                // Cancelar é uma escolha do usuário, não um erro do programa.
                return Err(PickError::Cancelled);
            }
            return Ok(PathBuf::from(escolhido));
        }
        Err(PickError::NoPicker(format!(
            "tentei {}",
            tentados.join(", ")
        )))
    }
}

/// No Windows, o seletor do próprio Explorer via PowerShell.
#[cfg(windows)]
fn pick_directory_windows(titulo: &str, inicio: &str) -> Result<PathBuf, PickError> {
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         $d.Description = '{titulo}'; $d.SelectedPath = '{inicio}'; \
         if ($d.ShowDialog() -eq 'OK') {{ Write-Output $d.SelectedPath }}"
    );
    let saida = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .output()
        .map_err(|e| PickError::Failed(e.to_string()))?;
    let escolhido = String::from_utf8_lossy(&saida.stdout).trim().to_string();
    if escolhido.is_empty() {
        return Err(PickError::Cancelled);
    }
    Ok(PathBuf::from(escolhido))
}

/// O programa existe no PATH?
pub fn which(programa: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(programa))
        .find(|p| p.is_file())
}

/// Há algum seletor gráfico disponível? (para a paleta dizer o estado)
pub fn available() -> bool {
    if cfg!(windows) {
        return true;
    }
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    candidates(&desktop).iter().any(|p| which(p).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kde_prefers_its_own_dialog() {
        assert_eq!(candidates("KDE")[0], "kdialog");
        assert_eq!(candidates("kde-plasma")[0], "kdialog");
        // Fora do KDE, o diálogo GTK vem primeiro.
        assert_eq!(candidates("GNOME")[0], "zenity");
        assert_eq!(candidates("")[0], "zenity");
        // Nenhum ambiente perde opções — só muda a ordem.
        for d in ["KDE", "GNOME", ""] {
            assert_eq!(candidates(d).len(), 3);
        }
    }

    #[test]
    fn builds_the_right_command_line_for_each_picker() {
        let (cmd, args) = command_for("kdialog", "Escolha", "/home/x").unwrap();
        assert_eq!(cmd, "kdialog");
        assert!(args.contains(&"--getexistingdirectory".to_string()));
        assert!(args.contains(&"/home/x".to_string()));

        let (_, args) = command_for("zenity", "Escolha", "/home/x").unwrap();
        assert!(args.contains(&"--directory".to_string()));
        assert!(args.iter().any(|a| a == "--filename=/home/x/"));

        assert!(command_for("inexistente", "t", "/").is_none());
    }

    #[test]
    fn errors_explain_themselves() {
        assert!(PickError::Cancelled.message().contains("cancelada"));
        let sem = PickError::NoPicker("tentei zenity".into());
        assert!(sem.message().contains("informe o caminho"));
        assert!(PickError::Failed("boom".into()).message().contains("boom"));
    }

    #[test]
    fn which_finds_a_program_that_exists() {
        // `sh` existe em qualquer Unix; um nome inventado, não.
        if !cfg!(windows) {
            assert!(which("sh").is_some());
        }
        assert!(which("programa-que-nao-existe-mesmo-123").is_none());
    }
}
