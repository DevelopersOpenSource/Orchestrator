//! Descoberta de capacidades de uma CLI de agente pelo próprio `--help`.
//!
//! Em vez de assumir um conjunto fixo de flags, o orquestrador pergunta à
//! CLI o que ela suporta (como uma pessoa leria o help) e só usa o que
//! existe NAQUELA build: modelos, efforts, permission modes etc. Se uma
//! versão futura ganhar uma flag nova (ex.: `--fast`), ela passa a ser
//! detectada sem mudança de código aqui.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

/// O que uma CLI de agente expõe via linha de comando.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliCapabilities {
    /// Todas as flags longas vistas no help (ex.: `--model`).
    pub flags: BTreeSet<String>,
    /// Valores aceitos por `--permission-mode` (vazio se a flag não existe).
    pub permission_modes: Vec<String>,
    /// Níveis aceitos por `--effort` (vazio se a flag não existe).
    pub efforts: Vec<String>,
    /// Aliases de modelo citados no help de `--model` + aliases conhecidos.
    pub model_aliases: Vec<String>,
}

impl CliCapabilities {
    /// A CLI suporta esta flag longa? (`supports("--model")`)
    pub fn supports(&self, flag: &str) -> bool {
        self.flags.contains(flag)
    }
}

/// Cache por binário: o help só é lido uma vez por processo.
fn cache() -> &'static Mutex<HashMap<String, CliCapabilities>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CliCapabilities>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Roda `<binary> --help` e extrai as capacidades. Erros (binário ausente,
/// help vazio) viram capacidades vazias — quem chama decide degradar.
pub fn discover(binary: &str) -> CliCapabilities {
    if let Some(hit) = cache().lock().ok().and_then(|c| c.get(binary).cloned()) {
        return hit;
    }
    let help = std::process::Command::new(binary)
        .arg("--help")
        .output()
        .ok()
        .map(|out| {
            let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            s
        })
        .unwrap_or_default();
    let caps = parse_help(&help);
    if let Ok(mut c) = cache().lock() {
        c.insert(binary.to_string(), caps.clone());
    }
    caps
}

/// Uma linha do help inicia uma nova opção? (indentação rasa + `-`)
fn is_option_start(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    indent <= 6 && line.trim_start().starts_with('-')
}

/// Devolve o bloco de texto (linha inicial + continuações) da opção `flag`.
fn option_block<'a>(help: &'a str, flag: &str) -> String {
    let mut block = String::new();
    let mut inside = false;
    for line in help.lines() {
        if is_option_start(line) {
            if inside {
                break;
            }
            // A flag pode vir depois de formas curtas: `-r, --resume [...]`.
            let names: Vec<&str> = line
                .trim_start()
                .split([' ', ','])
                .filter(|t| t.starts_with('-'))
                .collect();
            if names.iter().any(|n| *n == flag) {
                inside = true;
            }
        }
        if inside {
            block.push_str(line);
            block.push('\n');
        }
    }
    block
}

/// Extrai os valores de um `(choices: "a", "b", ...)` dentro do bloco.
fn choices_of(block: &str) -> Vec<String> {
    let Some(idx) = block.find("choices:") else {
        return Vec::new();
    };
    let rest = &block[idx..];
    let end = rest.find(')').unwrap_or(rest.len());
    quoted_words(&rest[..end])
}

/// Palavras entre aspas (simples ou duplas) no texto.
fn quoted_words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '"' || c == '\'' {
            if let Some(end) = text[i + 1..].find(c) {
                let word = &text[i + 1..i + 1 + end];
                if !word.is_empty() && !word.contains(' ') {
                    out.push(word.to_string());
                }
                // pula até depois da aspa de fechamento (byte i + 1 + end)
                while let Some(&(j, _)) = chars.peek() {
                    if j > i + 1 + end {
                        break;
                    }
                    chars.next();
                }
            }
        }
    }
    out
}

/// Lista `(a, b, c)` simples (sem `choices:`) dentro do bloco — usada pelo
/// `--effort <level>` do Claude Code: `(low, medium, high, xhigh, max)`.
fn paren_list(block: &str) -> Vec<String> {
    for (i, _) in block.match_indices('(') {
        let rest = &block[i + 1..];
        let Some(end) = rest.find(')') else { continue };
        let inner = &rest[..end];
        if inner.contains("choices:") {
            continue;
        }
        let words: Vec<String> = inner
            .split(',')
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect();
        if words.len() >= 2 && words.len() == inner.split(',').count() {
            return words;
        }
    }
    Vec::new()
}

/// Interpreta o texto do `--help` de uma CLI.
pub fn parse_help(help: &str) -> CliCapabilities {
    // Flags longas: token começando com `--` seguido de letras/hífens.
    let mut flags = BTreeSet::new();
    for line in help.lines() {
        if !is_option_start(line) {
            continue;
        }
        for token in line.trim_start().split([' ', ',']) {
            let token = token.trim_end_matches(['<', '[']);
            if let Some(name) = token.strip_prefix("--") {
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                {
                    flags.insert(format!("--{name}"));
                }
            }
        }
    }

    let permission_modes = choices_of(&option_block(help, "--permission-mode"));

    let effort_block = option_block(help, "--effort");
    let mut efforts = choices_of(&effort_block);
    if efforts.is_empty() {
        efforts = paren_list(&effort_block);
    }

    // Aliases de modelo citados no help + os conhecidos, sem duplicar.
    let mut model_aliases: Vec<String> = quoted_words(&option_block(help, "--model"))
        .into_iter()
        .filter(|w| !w.starts_with("claude-"))
        .collect();
    for known in ["haiku", "sonnet", "opus"] {
        if !model_aliases.iter().any(|m| m == known) {
            model_aliases.push(known.to_string());
        }
    }

    CliCapabilities {
        flags,
        permission_modes,
        efforts,
        model_aliases,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorte fiel do help real do Claude Code.
    const HELP: &str = r#"Usage: claude [options] [command] [prompt]

Options:
  --effort <level>                      Effort level for the current session
                                        (low, medium, high, xhigh, max)
  --fallback-model <model>              Enable automatic fallback to specified
                                        model(s) when the default model is
                                        overloaded (only works with --print)
  --model <model>                       Model for the current session. Provide
                                        an alias for the latest model (e.g.
                                        'fable', 'opus', or 'sonnet') or a
                                        model's full name (e.g.
                                        'claude-fable-5').
  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: "acceptEdits", "auto",
                                        "bypassPermissions", "manual",
                                        "dontAsk", "plan")
  -r, --resume [value]                  Resume a conversation by session ID
  --verbose                             Override verbose mode setting from
                                        config
"#;

    #[test]
    fn parses_flags_and_choices() {
        let caps = parse_help(HELP);
        assert!(caps.supports("--model"));
        assert!(caps.supports("--effort"));
        assert!(caps.supports("--permission-mode"));
        assert!(caps.supports("--resume"));
        assert!(!caps.supports("--fast"), "esta build não tem --fast");

        assert_eq!(
            caps.permission_modes,
            vec!["acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan"]
        );
        assert_eq!(caps.efforts, vec!["low", "medium", "high", "xhigh", "max"]);
        // aliases do help + conhecidos, sem nomes completos claude-*
        assert!(caps.model_aliases.contains(&"fable".to_string()));
        assert!(caps.model_aliases.contains(&"sonnet".to_string()));
        assert!(caps.model_aliases.contains(&"haiku".to_string()));
        assert!(!caps.model_aliases.iter().any(|m| m.starts_with("claude-")));
    }

    #[test]
    fn empty_help_degrades_to_empty_caps() {
        let caps = parse_help("");
        assert!(caps.flags.is_empty());
        assert!(caps.permission_modes.is_empty());
        // aliases conhecidos continuam disponíveis como chute educado
        assert_eq!(caps.model_aliases, vec!["haiku", "sonnet", "opus"]);
    }
}
