//! Descobre de que se trata uma pasta, para o Orchestrator adotá-la como
//! projeto sem cerimônia.
//!
//! Abrir a CLI numa pasta deveria bastar: sem cadastrar projeto, sem escolher
//! caminho, sem começar outra conversa. Aqui olhamos os sinais que um
//! repositório dá de si mesmo — manifesto de pacote, README, estrutura — e
//! montamos nome e descrição. É leitura rasa e sem rede: só o que está no
//! disco, na raiz e um nível abaixo.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// O que deu para inferir sobre a pasta.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectHint {
    /// Nome do projeto (do manifesto quando houver, senão o da pasta).
    pub name: String,
    /// Uma linha sobre o que é, quando a pasta diz.
    pub summary: String,
    /// Tecnologias/ecossistemas reconhecidos, em ordem de evidência.
    pub stack: Vec<String>,
    /// Arquivos que sustentaram a conclusão (para o usuário conferir).
    pub evidence: Vec<String>,
}

impl ProjectHint {
    /// Frase curta para mostrar ao usuário ao adotar a pasta.
    pub fn describe(&self) -> String {
        let mut partes = vec![format!("projeto \"{}\"", self.name)];
        if !self.stack.is_empty() {
            partes.push(self.stack.join(" + "));
        }
        if !self.summary.is_empty() {
            partes.push(self.summary.clone());
        }
        partes.join(" · ")
    }
}

/// Sinais que reconhecemos, do mais específico para o mais genérico.
const MANIFESTOS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust"),
    ("package.json", "Node"),
    ("pyproject.toml", "Python"),
    ("requirements.txt", "Python"),
    ("go.mod", "Go"),
    ("pom.xml", "Java/Maven"),
    ("build.gradle", "Java/Gradle"),
    ("build.gradle.kts", "Kotlin/Gradle"),
    ("Gemfile", "Ruby"),
    ("composer.json", "PHP"),
    ("pubspec.yaml", "Dart/Flutter"),
    ("mix.exs", "Elixir"),
    ("CMakeLists.txt", "C/C++"),
    ("Makefile", "Make"),
    ("Dockerfile", "Docker"),
    ("docker-compose.yml", "Docker Compose"),
];

/// Extensões que indicam a linguagem quando não há manifesto.
const EXTENSOES: &[(&str, &str)] = &[
    ("rs", "Rust"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("js", "JavaScript"),
    ("jsx", "JavaScript"),
    ("py", "Python"),
    ("go", "Go"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("rb", "Ruby"),
    ("php", "PHP"),
    ("c", "C"),
    ("h", "C"),
    ("cpp", "C++"),
    ("cs", "C#"),
    ("swift", "Swift"),
    ("dart", "Dart"),
    ("ex", "Elixir"),
    ("sh", "Shell"),
    ("html", "Web"),
    ("css", "Web"),
];

/// Inspeciona a pasta e devolve o que der para concluir.
///
/// Nunca falha: uma pasta vazia devolve só o nome dela — é melhor adotar um
/// projeto magro do que exigir cadastro antes de começar.
pub fn describe_project(dir: &Path) -> ProjectHint {
    let mut hint = ProjectHint {
        name: nome_da_pasta(dir),
        ..Default::default()
    };

    // 1. Manifestos: dão nome, descrição e a tecnologia de uma vez.
    for (arquivo, tecnologia) in MANIFESTOS {
        let caminho = dir.join(arquivo);
        if !caminho.is_file() {
            continue;
        }
        hint.evidence.push((*arquivo).to_string());
        if !hint.stack.iter().any(|s| s == tecnologia) {
            hint.stack.push((*tecnologia).to_string());
        }
        if let Ok(texto) = fs::read_to_string(&caminho) {
            if let Some((nome, desc)) = do_manifesto(arquivo, &texto) {
                if let Some(nome) = nome {
                    hint.name = nome;
                }
                if hint.summary.is_empty() {
                    if let Some(desc) = desc {
                        hint.summary = desc;
                    }
                }
            }
        }
    }

    // 2. README: quando o manifesto não explicou, ele costuma explicar.
    if hint.summary.is_empty() {
        for nome in ["README.md", "README.MD", "readme.md", "README.txt", "README"] {
            let caminho = dir.join(nome);
            if let Ok(texto) = fs::read_to_string(&caminho) {
                if let Some(linha) = primeira_frase(&texto) {
                    hint.summary = linha;
                    hint.evidence.push(nome.to_string());
                    break;
                }
            }
        }
    }

    // 3. Sem manifesto: deduz a tecnologia pelo que há de arquivo.
    if hint.stack.is_empty() {
        for (tecnologia, _) in linguagens_por_extensao(dir).into_iter().take(2) {
            hint.stack.push(tecnologia);
        }
    }
    hint
}

/// Nome legível a partir do caminho.
fn nome_da_pasta(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty() && n != "/")
        .unwrap_or_else(|| "projeto".to_string())
}

/// Extrai `(nome, descrição)` de um manifesto conhecido.
///
/// Leitura deliberadamente simples: linha a linha, sem puxar um parser de
/// TOML/JSON para cada ecossistema. Se o formato fugir do comum, o campo fica
/// vazio e os outros sinais assumem.
fn do_manifesto(arquivo: &str, texto: &str) -> Option<(Option<String>, Option<String>)> {
    match arquivo {
        "Cargo.toml" | "pyproject.toml" => {
            let nome = valor_toml(texto, "name");
            let desc = valor_toml(texto, "description");
            Some((nome, desc))
        }
        "package.json" | "composer.json" => {
            let nome = valor_json(texto, "name");
            let desc = valor_json(texto, "description");
            Some((nome, desc))
        }
        "go.mod" => {
            let modulo = texto
                .lines()
                .find_map(|l| l.trim().strip_prefix("module "))
                .map(|m| {
                    m.trim()
                        .rsplit('/')
                        .next()
                        .unwrap_or(m.trim())
                        .to_string()
                });
            Some((modulo, None))
        }
        "pubspec.yaml" => {
            let nome = valor_yaml(texto, "name");
            let desc = valor_yaml(texto, "description");
            Some((nome, desc))
        }
        _ => None,
    }
}

/// Primeiro `chave = "valor"` do texto (TOML raso).
fn valor_toml(texto: &str, chave: &str) -> Option<String> {
    texto.lines().find_map(|linha| {
        let l = linha.trim();
        let resto = l.strip_prefix(chave)?.trim_start();
        let resto = resto.strip_prefix('=')?.trim();
        limpa_aspas(resto)
    })
}

/// Primeiro `"chave": "valor"` do texto (JSON raso).
///
/// Procura a chave em qualquer posição da linha: JSON de manifesto vem tanto
/// identado quanto tudo numa linha só, e olhar apenas o começo perdia o
/// segundo caso.
fn valor_json(texto: &str, chave: &str) -> Option<String> {
    let alvo = format!("\"{chave}\"");
    for linha in texto.lines() {
        let Some(pos) = linha.find(&alvo) else { continue };
        let resto = linha[pos + alvo.len()..].trim_start();
        let Some(resto) = resto.strip_prefix(':') else { continue };
        // Recorta só até o fim do valor, senão o resto do objeto vem junto.
        let resto = resto.trim_start();
        let valor = if let Some(sem_aspa) = resto.strip_prefix('"') {
            sem_aspa.split('"').next().unwrap_or("")
        } else {
            resto.split([',', '}']).next().unwrap_or("").trim()
        };
        if let Some(v) = limpa_aspas(valor) {
            return Some(v);
        }
    }
    None
}

/// Primeiro `chave: valor` do texto (YAML raso).
fn valor_yaml(texto: &str, chave: &str) -> Option<String> {
    let alvo = format!("{chave}:");
    texto.lines().find_map(|linha| {
        // Só chave de primeiro nível — indentada é de outra seção.
        if linha.starts_with(char::is_whitespace) {
            return None;
        }
        let resto = linha.trim().strip_prefix(&alvo)?.trim();
        let valor = limpa_aspas(resto).unwrap_or_else(|| resto.to_string());
        (!valor.is_empty()).then_some(valor)
    })
}

/// Tira aspas e devolve `None` se sobrar vazio.
fn limpa_aspas(bruto: &str) -> Option<String> {
    let v = bruto.trim().trim_matches('"').trim_matches('\'').trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// Primeira frase útil de um README: pula títulos, badges e linhas vazias.
fn primeira_frase(texto: &str) -> Option<String> {
    const MAX: usize = 160;
    for linha in texto.lines().take(40) {
        let l = linha.trim();
        if l.is_empty()
            || l.starts_with('#')
            || l.starts_with('!')
            || l.starts_with('[')
            || l.starts_with('<')
            || l.starts_with("---")
            || l.starts_with("```")
        {
            continue;
        }
        let frase = l.split(". ").next().unwrap_or(l).trim_end_matches('.');
        if frase.chars().count() < 8 {
            continue;
        }
        let curta: String = frase.chars().take(MAX).collect();
        return Some(curta);
    }
    None
}

/// Conta arquivos por linguagem (raiz + um nível), do mais comum ao menos.
fn linguagens_por_extensao(dir: &Path) -> Vec<(String, usize)> {
    let mapa: BTreeMap<&str, &str> = EXTENSOES.iter().copied().collect();
    let mut contagem: BTreeMap<String, usize> = BTreeMap::new();
    let mut conta = |caminho: &Path| {
        if let Some(ext) = caminho.extension().and_then(|e| e.to_str()) {
            if let Some(lang) = mapa.get(ext) {
                *contagem.entry((*lang).to_string()).or_insert(0) += 1;
            }
        }
    };
    let ignorar = |nome: &str| {
        nome.starts_with('.')
            || matches!(
                nome,
                "node_modules" | "target" | "dist" | "build" | "venv" | "__pycache__"
            )
    };
    if let Ok(entradas) = fs::read_dir(dir) {
        for e in entradas.flatten() {
            let nome = e.file_name().to_string_lossy().into_owned();
            if ignorar(&nome) {
                continue;
            }
            let caminho = e.path();
            if caminho.is_file() {
                conta(&caminho);
            } else if caminho.is_dir() {
                if let Ok(filhos) = fs::read_dir(&caminho) {
                    for f in filhos.flatten().take(200) {
                        let p = f.path();
                        if p.is_file() {
                            conta(&p);
                        }
                    }
                }
            }
        }
    }
    let mut v: Vec<(String, usize)> = contagem.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pasta(arquivos: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (nome, conteudo) in arquivos {
            let caminho = dir.path().join(nome);
            if let Some(pai) = caminho.parent() {
                fs::create_dir_all(pai).unwrap();
            }
            fs::write(caminho, conteudo).unwrap();
        }
        dir
    }

    #[test]
    fn reads_name_and_description_from_cargo() {
        let d = pasta(&[(
            "Cargo.toml",
            "[package]\nname = \"minha-api\"\ndescription = \"API de cobrança\"\nversion = \"0.1.0\"\n",
        )]);
        let h = describe_project(d.path());
        assert_eq!(h.name, "minha-api");
        assert_eq!(h.summary, "API de cobrança");
        assert_eq!(h.stack, vec!["Rust"]);
        assert!(h.evidence.contains(&"Cargo.toml".to_string()));
    }

    #[test]
    fn reads_package_json() {
        let d = pasta(&[(
            "package.json",
            "{\n  \"name\": \"loja-web\",\n  \"description\": \"Frontend da loja\",\n  \"version\": \"1.0.0\"\n}",
        )]);
        let h = describe_project(d.path());
        assert_eq!(h.name, "loja-web");
        assert_eq!(h.summary, "Frontend da loja");
        assert_eq!(h.stack, vec!["Node"]);
    }

    #[test]
    fn falls_back_to_the_readme_when_the_manifest_is_quiet() {
        let d = pasta(&[
            ("go.mod", "module github.com/eu/servico-pagamentos\n\ngo 1.22\n"),
            (
                "README.md",
                "# Serviço de pagamentos\n\n![badge](x.svg)\n\nProcessa cobranças recorrentes. Roda em Kubernetes.\n",
            ),
        ]);
        let h = describe_project(d.path());
        // Nome vem do módulo Go (última parte do caminho).
        assert_eq!(h.name, "servico-pagamentos");
        // Descrição vem do README, pulando título e badge.
        assert_eq!(h.summary, "Processa cobranças recorrentes");
        assert_eq!(h.stack, vec!["Go"]);
    }

    #[test]
    fn without_a_manifest_it_guesses_from_the_files() {
        let d = pasta(&[
            ("app.py", "print('oi')"),
            ("util.py", "x = 1"),
            ("index.html", "<html></html>"),
        ]);
        let h = describe_project(d.path());
        // Nome cai para o da pasta; tecnologia vem da contagem.
        assert!(!h.name.is_empty());
        assert_eq!(h.stack.first().map(String::as_str), Some("Python"));
    }

    #[test]
    fn ignores_dependency_and_build_directories() {
        let d = pasta(&[
            ("app.py", "x"),
            ("node_modules/lib/a.js", "x"),
            ("node_modules/lib/b.js", "x"),
            ("node_modules/lib/c.js", "x"),
            ("target/debug/d.rs", "x"),
        ]);
        let h = describe_project(d.path());
        assert_eq!(
            h.stack.first().map(String::as_str),
            Some("Python"),
            "node_modules/target não podem decidir a tecnologia: {:?}",
            h.stack
        );
    }

    #[test]
    fn an_empty_folder_still_becomes_a_project() {
        let d = tempfile::tempdir().unwrap();
        let h = describe_project(d.path());
        assert!(!h.name.is_empty(), "precisa de um nome para poder adotar");
        assert!(h.summary.is_empty());
        assert!(h.stack.is_empty());
        // A frase de apresentação funciona mesmo magra.
        assert!(h.describe().contains(&h.name));
    }

    #[test]
    fn recognizes_several_ecosystems_together() {
        let d = pasta(&[
            ("Cargo.toml", "[package]\nname = \"servico\"\n"),
            ("Dockerfile", "FROM debian"),
            ("docker-compose.yml", "services: {}"),
        ]);
        let h = describe_project(d.path());
        assert!(h.stack.contains(&"Rust".to_string()));
        assert!(h.stack.contains(&"Docker".to_string()));
        assert_eq!(h.evidence.len(), 3);
    }

    #[test]
    fn describe_reads_like_a_sentence() {
        let d = pasta(&[(
            "Cargo.toml",
            "[package]\nname = \"cobranca\"\ndescription = \"Fatura recorrente\"\n",
        )]);
        let frase = describe_project(d.path()).describe();
        assert!(frase.contains("cobranca"));
        assert!(frase.contains("Rust"));
        assert!(frase.contains("Fatura recorrente"));
    }

    #[test]
    fn readme_helpers_skip_noise_and_cut_long_lines() {
        assert_eq!(primeira_frase("# Título\n\n\nUma coisa útil aqui"), Some("Uma coisa útil aqui".into()));
        // Linha curta demais não serve de descrição.
        assert_eq!(primeira_frase("# T\n\nok\n\nDescrição de verdade agora"), Some("Descrição de verdade agora".into()));
        // Sem nada aproveitável.
        assert_eq!(primeira_frase("# Só título\n\n```\ncodigo\n```"), None);
        let longo = "a".repeat(400);
        assert_eq!(primeira_frase(&longo).unwrap().chars().count(), 160);
    }

    #[test]
    fn shallow_parsers_do_not_confuse_sections() {
        // `name` de outra seção não deve virar o nome do projeto.
        let toml = "[package]\nname = \"certo\"\n\n[dependencies.foo]\nname = \"errado\"\n";
        assert_eq!(valor_toml(toml, "name"), Some("certo".into()));
        // YAML indentado é de subseção.
        let yaml = "name: certo\ndependencies:\n  name: errado\n";
        assert_eq!(valor_yaml(yaml, "name"), Some("certo".into()));
        assert_eq!(valor_json("{\"name\": \"x\"}", "nome"), None);
        // JSON numa linha só (comum em manifesto gerado) também é lido.
        let compacto = r#"{"name":"loja","description":"Catálogo de produtos","v":1}"#;
        assert_eq!(valor_json(compacto, "name"), Some("loja".into()));
        assert_eq!(
            valor_json(compacto, "description"),
            Some("Catálogo de produtos".into())
        );
        // Valor não-texto não vira descrição.
        assert_eq!(valor_json(compacto, "v"), Some("1".into()));
    }
}
