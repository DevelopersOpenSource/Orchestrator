//! Remoção de segredos antes de embeddar ou persistir qualquer texto.
//!
//! Cobre formatos comuns de credenciais: chaves OpenAI/Anthropic (`sk-...`),
//! tokens GitHub (`ghp_...` e variantes), AWS Access Key IDs (`AKIA...`),
//! tokens Bearer e padrões `password=...` / `senha=...`.

use regex::Regex;
use std::sync::OnceLock;

const REDACTED: &str = "[REDACTED]";

fn patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // OpenAI/Anthropic style keys: sk-..., sk-proj-..., sk-ant-...
            r"\bsk-[A-Za-z0-9_-]{16,}\b",
            // GitHub tokens: ghp_, gho_, ghu_, ghs_, ghr_ + 36 chars
            r"\bgh[pousr]_[A-Za-z0-9]{36,}\b",
            // GitHub fine-grained PAT
            r"\bgithub_pat_[A-Za-z0-9_]{22,}\b",
            // AWS Access Key ID
            r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
            // Slack tokens
            r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
            // Bearer tokens (JWTs, opaque tokens)
            r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}",
            // password= / passwd= / pwd= / senha= assignments
            r#"(?i)\b(?:password|passwd|pwd|senha)\s*[:=]\s*["']?[^\s"']+["']?"#,
        ]
        .iter()
        .map(|p| Regex::new(p).expect("regex de scrub inválida"))
        .collect()
    })
}

/// Substitui segredos conhecidos por `[REDACTED]`.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for re in patterns() {
        out = re.replace_all(&out, REDACTED).into_owned();
    }
    out
}

/// Indica se o texto contém algum segredo detectável.
pub fn contains_secret(text: &str) -> bool {
    patterns().iter().any(|re| re.is_match(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_openai_key() {
        let s = scrub("use a chave sk-abc123DEF456ghi789JKL012 para autenticar");
        assert!(!s.contains("sk-abc123"));
        assert!(s.contains(REDACTED));
    }

    #[test]
    fn scrubs_github_token() {
        let s = scrub("token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
        assert!(!s.contains("ghp_"));
        assert!(s.contains(REDACTED));
    }

    #[test]
    fn scrubs_aws_key() {
        let s = scrub("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE");
        assert!(!s.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn scrubs_bearer_token() {
        let s = scrub("Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.abc");
        assert!(!s.contains("eyJhbGci"));
        assert!(s.contains(REDACTED));
    }

    #[test]
    fn scrubs_password_assignment() {
        let s = scrub("conectar com password=SuperSecreta123 no banco");
        assert!(!s.contains("SuperSecreta123"));
        let s2 = scrub("senha: 'hunter2!'");
        assert!(!s2.contains("hunter2"));
    }

    #[test]
    fn leaves_clean_text_untouched() {
        let text = "usar Arc<Mutex<T>> para estado compartilhado no serviço";
        assert_eq!(scrub(text), text);
        assert!(!contains_secret(text));
    }
}
