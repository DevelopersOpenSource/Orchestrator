//! Conexões SSH pré-configuradas pelo dono, por projeto.
//!
//! O dono cadastra host, usuário, porta e o CAMINHO da chave privada (a
//! chave em si nunca é copiada nem guardada aqui). Viram um `ssh_config`
//! próprio do projeto com `BatchMode yes` — a IA e os terminais usam sem
//! pedir senha, e sem o arquivo do dono (`~/.ssh/config`) ser tocado.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Chave em `ui_state` onde a lista de um projeto é guardada (JSON).
pub fn state_key(project: &str) -> String {
    format!("ssh.hosts.{project}")
}

/// Servidores que valem em TODO projeto (uma VPS costuma servir a vários).
pub const GLOBAL_KEY: &str = "ssh.hosts.*";

/// Os servidores que um projeto enxerga: os globais e os dele (o do projeto
/// vence quando os dois têm o mesmo nome).
pub fn merge(global_json: Option<&str>, project_json: Option<&str>) -> Vec<SshHost> {
    let mut out: Vec<SshHost> = global_json
        .map(parse)
        .unwrap_or_default()
        .into_iter()
        .map(|mut h| {
            h.global = true;
            h
        })
        .collect();
    for h in project_json.map(parse).unwrap_or_default() {
        out.retain(|g| !g.nome.eq_ignore_ascii_case(&h.nome));
        out.push(SshHost { global: false, ..h });
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshHost {
    /// Apelido usado no comando (`ssh <nome>`).
    pub nome: String,
    pub host: String,
    #[serde(default = "usuario_padrao")]
    pub usuario: String,
    #[serde(default = "porta_padrao")]
    pub porta: u16,
    /// Caminho da chave privada no disco do dono (`~` expandido).
    #[serde(default)]
    pub chave: String,
    /// Vale em todo projeto (guardado em [`GLOBAL_KEY`]).
    #[serde(default)]
    pub global: bool,
}

fn usuario_padrao() -> String {
    "root".into()
}

fn porta_padrao() -> u16 {
    22
}

/// Lê a lista guardada (JSON); texto inválido vira lista vazia.
pub fn parse(json: &str) -> Vec<SshHost> {
    serde_json::from_str(json).unwrap_or_default()
}

/// Apelido seguro para `Host` no ssh_config (sem espaço nem curinga).
pub fn alias(nome: &str) -> String {
    let limpo: String = nome
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect();
    if limpo.trim_matches('-').is_empty() {
        "vps".into()
    } else {
        limpo
    }
}

fn expand(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest).display().to_string(),
        _ => p.to_string(),
    }
}

/// Conteúdo do ssh_config do projeto.
pub fn config_text(hosts: &[SshHost], known_hosts: &Path) -> String {
    let mut out = String::new();
    for h in hosts {
        out.push_str(&format!("Host {}\n", alias(&h.nome)));
        out.push_str(&format!("  HostName {}\n", h.host.trim()));
        out.push_str(&format!("  User {}\n", h.usuario.trim()));
        out.push_str(&format!("  Port {}\n", h.porta));
        if !h.chave.trim().is_empty() {
            out.push_str(&format!("  IdentityFile \"{}\"\n", expand(h.chave.trim())));
            out.push_str("  IdentitiesOnly yes\n");
        }
        // Nunca para esperando senha: sem chave válida, falha na hora.
        out.push_str("  BatchMode yes\n");
        out.push_str("  ConnectTimeout 10\n");
        // Primeira conexão aceita a chave do servidor; mudança depois é erro.
        out.push_str("  StrictHostKeyChecking accept-new\n");
        out.push_str(&format!("  UserKnownHostsFile \"{}\"\n\n", known_hosts.display()));
    }
    out
}

/// Pasta das configs SSH do Orchestrator (`~/.config/orchestrator/ssh`).
pub fn dir() -> PathBuf {
    crate::config::default_config_path()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("ssh")))
        .unwrap_or_else(|| std::env::temp_dir().join("orchestrator-ssh"))
}

/// Grava (0600) o ssh_config do projeto e devolve o caminho.
pub fn write_config(project: &str, hosts: &[SshHost]) -> std::io::Result<PathBuf> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let nome: String = project
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let path = dir.join(format!("{nome}.conf"));
    std::fs::write(&path, config_text(hosts, &dir.join("known_hosts")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_never_prompts_and_uses_the_given_key() {
        let hosts = vec![SshHost {
            nome: "minha vps".into(),
            host: "203.0.113.7".into(),
            usuario: "root".into(),
            porta: 2222,
            chave: "/home/eu/.ssh/id_vps".into(),
            global: false,
        }];
        let t = config_text(&hosts, Path::new("/tmp/kh"));
        assert!(t.starts_with("Host minha-vps\n"));
        assert!(t.contains("Port 2222"));
        assert!(t.contains("IdentityFile \"/home/eu/.ssh/id_vps\""));
        assert!(t.contains("BatchMode yes"));
    }

    #[test]
    fn parse_fills_defaults_and_tolerates_garbage() {
        let h = parse(r#"[{"nome":"a","host":"h"}]"#);
        assert_eq!((h[0].usuario.as_str(), h[0].porta), ("root", 22));
        assert!(parse("não é json").is_empty());
    }

    #[test]
    fn global_hosts_show_in_every_project_and_the_project_wins_on_name() {
        let g = r#"[{"nome":"vps","host":"1.1.1.1"},{"nome":"backup","host":"2.2.2.2"}]"#;
        let p = r#"[{"nome":"vps","host":"9.9.9.9"}]"#;
        let m = merge(Some(g), Some(p));
        assert_eq!(m.len(), 2);
        assert!(m.iter().any(|h| h.nome == "backup" && h.global));
        assert!(m.iter().any(|h| h.nome == "vps" && h.host == "9.9.9.9" && !h.global));
    }
}
