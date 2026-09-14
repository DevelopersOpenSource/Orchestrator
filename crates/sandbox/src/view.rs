//! Transforma o que o navegador devolve em texto curto e acionável.
//!
//! É aqui que mora a compressão de contexto: a página vira uma lista de
//! elementos com referência, o texto é cortado, e os snapshots seguintes
//! mostram só o que mudou. Sem isso, testar pela sandbox custaria mais token
//! do que o trabalho que ela verifica.

use anyhow::{Context, Result};
use serde::Deserialize;

/// Teto de elementos listados por snapshot.
pub const ELEMENT_LIMIT: usize = 60;
/// Teto de caracteres do texto da página.
pub const TEXT_LIMIT: usize = 1_500;

/// O que o script de leitura devolve.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct Snapshot {
    pub url: String,
    pub title: String,
    pub elements: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub text: String,
}

/// Script de leitura da página, com os limites aplicados.
pub fn read_script() -> String {
    include_str!("page.js")
        .replace("__LIMIT__", &ELEMENT_LIMIT.to_string())
        .replace("__TEXT_LIMIT__", &TEXT_LIMIT.to_string())
}

/// Script de ação sobre um elemento já referenciado.
pub fn action_script(kind: &str, reference: &str, value: &str) -> String {
    include_str!("action.js")
        .replace("__KIND__", kind)
        .replace("__REF__", reference)
        .replace("__VALUE__", &serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()))
}

impl Snapshot {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("não consegui ler o resultado da página")
    }
}

/// Estado da página entre passos, para mostrar só a diferença.
#[derive(Debug, Default)]
pub struct PageView {
    last: Option<Snapshot>,
}

/// Como apresentar um snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Lista inteira (primeiro passo, ou quando ele pede).
    Full,
    /// Só o que mudou desde o passo anterior.
    Changes,
}

impl PageView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Formata o snapshot para o orquestrador e guarda como referência.
    pub fn render(&mut self, snap: Snapshot, detail: Detail) -> String {
        let previous = self.last.replace(snap.clone());
        let mut out = format!("{} — {}\n", snap.title.trim(), snap.url);

        match (detail, previous) {
            (Detail::Changes, Some(prev)) if prev.url == snap.url => {
                let (added, removed) = diff(&prev, &snap);
                if added.is_empty() && removed.is_empty() && prev.text == snap.text {
                    out.push_str("(nada mudou na página)\n");
                } else {
                    if !added.is_empty() {
                        out.push_str(&format!("apareceu:\n{}\n", added.join("\n")));
                    }
                    if !removed.is_empty() {
                        out.push_str(&format!("sumiu:\n{}\n", removed.join("\n")));
                    }
                    if prev.text != snap.text {
                        out.push_str(&format!(
                            "texto agora:\n{}\n",
                            first_lines(&snap.text, 12)
                        ));
                    }
                }
                out.push_str("(ui_snapshot full relista todos os elementos)\n");
            }
            _ => {
                out.push_str(&format!(
                    "elementos ({}{}):\n{}\n",
                    snap.elements.len(),
                    if snap.truncated { ", cortado" } else { "" },
                    snap.elements.join("\n")
                ));
                if !snap.text.trim().is_empty() {
                    out.push_str(&format!("texto:\n{}\n", first_lines(&snap.text, 20)));
                }
            }
        }
        out.push_str(HINT);
        out
    }

    /// Esquece o passo anterior (navegação para outro endereço).
    pub fn reset(&mut self) {
        self.last = None;
    }

    pub fn last(&self) -> Option<&Snapshot> {
        self.last.as_ref()
    }
}

/// O lembrete que acompanha TODA resposta da sandbox: sem isso o
/// orquestrador não sabe que pode pedir a imagem quando o texto não bastar.
pub const HINT: &str =
    "— aja por referência: ui_click e3 · ui_type e5 \"texto\" · ui_select e7 \"valor\". \
     Se o texto não bastar (layout, cor, algo visual), peça ui_screenshot: ela salva \
     um PNG e devolve o caminho, e você abre com Read só se precisar ver.";

/// Elementos que apareceram e que sumiram entre dois snapshots.
pub fn diff(prev: &Snapshot, now: &Snapshot) -> (Vec<String>, Vec<String>) {
    // Compara ignorando a referência: o `e12` de antes pode ser outro nó.
    let key = |l: &String| l.split_once("] ").map(|(_, r)| r.to_string()).unwrap_or_else(|| l.clone());
    let before: Vec<String> = prev.elements.iter().map(key).collect();
    let after: Vec<String> = now.elements.iter().map(key).collect();

    let added: Vec<String> = now
        .elements
        .iter()
        .filter(|l| !before.contains(&key(l)))
        .cloned()
        .collect();
    let removed: Vec<String> = prev
        .elements
        .iter()
        .filter(|l| !after.contains(&key(l)))
        .cloned()
        .collect();
    (added, removed)
}

/// Primeiras `n` linhas não vazias de um texto.
fn first_lines(text: &str, n: usize) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let shown = kept.iter().take(n).cloned().collect::<Vec<_>>().join("\n");
    if kept.len() > n {
        format!("{shown}\n…(+{} linhas)", kept.len() - n)
    } else {
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(url: &str, elements: &[&str], text: &str) -> Snapshot {
        Snapshot {
            url: url.into(),
            title: "Página".into(),
            elements: elements.iter().map(|s| s.to_string()).collect(),
            truncated: false,
            text: text.into(),
        }
    }

    #[test]
    fn first_render_lists_everything_and_teaches_the_next_step() {
        let mut view = PageView::new();
        let out = view.render(
            snap("http://x/", &["[e1] button \"Salvar\"", "[e2] link \"Sair\""], "olá"),
            Detail::Changes,
        );
        assert!(out.contains("[e1] button \"Salvar\""));
        assert!(out.contains("[e2]"));
        assert!(out.contains("texto:"));
        // O lembrete do screenshot vai em toda resposta.
        assert!(out.contains("ui_screenshot"));
        assert!(out.contains("ui_click"));
    }

    #[test]
    fn second_render_shows_only_what_changed() {
        let mut view = PageView::new();
        view.render(
            snap("http://x/", &["[e1] button \"Salvar\"", "[e2] link \"Sair\""], "antes"),
            Detail::Changes,
        );
        let out = view.render(
            snap(
                "http://x/",
                &["[e1] button \"Salvar\"", "[e2] link \"Sair\"", "[e3] alert \"Erro!\""],
                "antes",
            ),
            Detail::Changes,
        );
        assert!(out.contains("apareceu:"));
        assert!(out.contains("Erro!"));
        // O que já era conhecido NÃO é repetido: é a economia de contexto.
        assert!(!out.contains("[e1] button \"Salvar\""));
        assert!(out.contains("ui_snapshot full"));
    }

    #[test]
    fn removed_elements_are_reported() {
        let mut view = PageView::new();
        view.render(
            snap("http://x/", &["[e1] button \"Carregando\""], "t"),
            Detail::Changes,
        );
        let out = view.render(snap("http://x/", &["[e1] button \"Pronto\""], "t"), Detail::Changes);
        assert!(out.contains("apareceu:"));
        assert!(out.contains("Pronto"));
        assert!(out.contains("sumiu:"));
        assert!(out.contains("Carregando"));
    }

    #[test]
    fn nothing_changed_says_so_in_one_line() {
        let mut view = PageView::new();
        let s = snap("http://x/", &["[e1] button \"Salvar\""], "igual");
        view.render(s.clone(), Detail::Changes);
        let out = view.render(s, Detail::Changes);
        assert!(out.contains("(nada mudou na página)"));
        assert!(out.lines().count() < 6, "resposta deveria ser curta:\n{out}");
    }

    #[test]
    fn navigating_elsewhere_shows_the_full_page_again() {
        let mut view = PageView::new();
        view.render(snap("http://a/", &["[e1] link \"ir\""], "a"), Detail::Changes);
        let out = view.render(snap("http://b/", &["[e1] button \"voltar\""], "b"), Detail::Changes);
        assert!(out.contains("elementos (1)"), "outra URL = página nova:\n{out}");
    }

    #[test]
    fn full_detail_relists_even_when_unchanged() {
        let mut view = PageView::new();
        let s = snap("http://x/", &["[e1] button \"Salvar\""], "t");
        view.render(s.clone(), Detail::Changes);
        let out = view.render(s, Detail::Full);
        assert!(out.contains("[e1] button \"Salvar\""));
    }

    #[test]
    fn diff_ignores_reference_renumbering() {
        // O mesmo botão pode ganhar outro número entre snapshots; isso não é
        // mudança e não deve poluir a resposta.
        let a = snap("u", &["[e1] button \"Salvar\"", "[e2] link \"Sair\""], "t");
        let b = snap("u", &["[e7] button \"Salvar\"", "[e9] link \"Sair\""], "t");
        let (added, removed) = diff(&a, &b);
        assert!(added.is_empty(), "{added:?}");
        assert!(removed.is_empty(), "{removed:?}");
    }

    #[test]
    fn parse_reads_what_the_page_script_returns() {
        let json = r#"{"url":"http://x/","title":"T","elements":["[e1] button \"ok\""],
                       "truncated":true,"text":"corpo"}"#;
        let s = Snapshot::parse(json).unwrap();
        assert_eq!(s.url, "http://x/");
        assert!(s.truncated);
        assert_eq!(s.elements.len(), 1);
        assert!(Snapshot::parse("{").is_err());
    }

    #[test]
    fn scripts_have_their_placeholders_filled() {
        let read = read_script();
        assert!(!read.contains("__LIMIT__"));
        assert!(read.contains(&ELEMENT_LIMIT.to_string()));

        let act = action_script("type", "e5", "olá \"mundo\"");
        assert!(!act.contains("__KIND__") && !act.contains("__REF__") && !act.contains("__VALUE__"));
        assert!(act.contains(r#""e5""#));
        // O valor entra como literal JSON — aspas do usuário não quebram o script.
        assert!(act.contains(r#""olá \"mundo\"""#));
    }

    #[test]
    fn long_text_is_trimmed_with_a_marker() {
        let long = (1..=40).map(|i| format!("linha {i}")).collect::<Vec<_>>().join("\n");
        let mut view = PageView::new();
        let out = view.render(snap("u", &[], &long), Detail::Full);
        assert!(out.contains("…(+"), "texto deveria ser cortado:\n{out}");
        assert!(!out.contains("linha 40"));
    }
}
