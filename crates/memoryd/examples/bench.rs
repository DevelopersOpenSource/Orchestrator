//! Compara os conjuntos de modelos da memória em velocidade e qualidade.
//!
//! - **atual**: `multilingual-e5-small` FP32 + `jina-reranker-v2-base-multilingual`
//!   (licença não comercial — não pode ir no app publicado);
//! - **candidatos**: `multilingual-e5-small` int8 + um reranker de licença
//!   livre por `--reranker nome=arquivo.onnx` (o tokenizer fica na mesma pasta).
//!
//! ```text
//! cargo run --release -p orchestrator-memoryd --example bench -- \
//!     --e5 <pasta do e5 int8> --reranker mmarco-q8=<pasta>/model_quint8_avx2.onnx [--sem-atual]
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use fastembed::{
    EmbeddingModel, InitOptions, InitOptionsUserDefined, Pooling, RerankInitOptions,
    RerankInitOptionsUserDefined, RerankerModel, TextEmbedding, TextRerank, TokenizerFiles,
    UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};
use orchestrator_memory::rerank::cross_score;
use orchestrator_memoryd::models::{models_dir, passage, query};

/// As memórias do teste ao vivo (projeto "loja" + globais) e distrações
/// realistas, para o reranker ter com o que se confundir.
const MEMORIAS: &[(&str, &str)] = &[
    ("Commits em português, no imperativo", "Mensagens de commit sempre em português, curtas e no imperativo: 'adiciona', 'corrige'."),
    ("Testes antes de dar a tarefa como pronta", "Nenhuma tarefa é concluída sem rodar a suíte de testes e mostrar a saída."),
    ("nunca apagar em massa", "Remoção recursiva é proibida.\ndeny-regex: \\brm\\s+-rf"),
    ("Banco de dados é PostgreSQL via sqlx", "O backend usa PostgreSQL com a crate sqlx e migrações na pasta migrations/."),
    ("Frontend usa Vite + React", "Escolhido Vite pela velocidade do servidor de desenvolvimento."),
    ("Estilo de código Rust", "cargo fmt e clippy sem avisos antes de abrir PR."),
    ("Logs estruturados", "Todo log do backend sai em JSON com request_id."),
    ("Variáveis de ambiente", "Toda variável nova entra no .env.example com comentário."),
    ("Integração contínua", "O CI roda no GitHub Actions a cada push."),
    ("Imagens do site", "Imagens são servidas em WebP com fallback em PNG."),
    ("Autenticação", "Login por JWT com refresh token de 7 dias."),
    ("Filas de trabalho", "Tarefas demoradas vão para uma fila no Redis."),
    ("Deploy", "Produção sobe com Docker Compose na VPS."),
    ("Nome de branch", "Branches seguem feature/, fix/ e chore/."),
    ("Cache HTTP", "Respostas públicas da API têm cache de 5 minutos."),
    ("Traduções", "Textos da interface ficam em i18n/pt-BR.json."),
    ("Senhas", "Senhas são guardadas com argon2id."),
    ("Datas", "Datas são gravadas em UTC e convertidas só na interface."),
    ("Paginação", "Listas longas da API paginam por cursor, não por offset."),
    ("Erros da API", "Erros saem em JSON com código e mensagem legível."),
    ("Componentes React", "Componentes em PascalCase, um por arquivo."),
    ("Testes de ponta a ponta", "Fluxos de compra têm teste E2E com Playwright."),
    ("Monitoramento", "Erros de produção vão para o Sentry."),
    ("Uploads", "Arquivos enviados pelos clientes vão para o S3."),
];

/// Pergunta e o título que deveria vir primeiro (`None` = nada relevante).
const PERGUNTAS: &[(&str, Option<&str>)] = &[
    ("qual SGBD a gente usa no backend?", Some("Banco de dados é PostgreSQL via sqlx")),
    ("como devo escrever a mensagem ao versionar alterações?", Some("Commits em português, no imperativo")),
    ("o que fazer antes de dizer que terminei?", Some("Testes antes de dar a tarefa como pronta")),
    ("que ferramenta de build o front usa?", Some("Frontend usa Vite + React")),
    ("posso limpar a pasta dist com rm -rf?", Some("nunca apagar em massa")),
    ("how should rust code be formatted before a pull request?", Some("Estilo de código Rust")),
    ("qual é a capital da França?", None),
    ("receita de bolo de cenoura", None),
    ("quem ganhou o campeonato de futebol?", None),
];

struct Conjunto {
    nome: String,
    embed: TextEmbedding,
    rerank: TextRerank,
    carga_ms: f64,
}

fn tokenizer(dir: &Path) -> Result<TokenizerFiles> {
    let ler = |f: &str| std::fs::read(dir.join(f)).with_context(|| format!("lendo {}", dir.join(f).display()));
    Ok(TokenizerFiles {
        tokenizer_file: ler("tokenizer.json")?,
        config_file: ler("config.json")?,
        special_tokens_map_file: ler("special_tokens_map.json")?,
        tokenizer_config_file: ler("tokenizer_config.json")?,
    })
}

fn atual() -> Result<Conjunto> {
    let t0 = Instant::now();
    let cache = models_dir();
    let embed = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::MultilingualE5Small).with_cache_dir(cache.clone()),
    )?;
    let rerank = TextRerank::try_new(
        RerankInitOptions::new(RerankerModel::JINARerankerV2BaseMultiligual).with_cache_dir(cache),
    )?;
    Ok(Conjunto { nome: "atual (E5 fp32 + jina)".into(), embed, rerank, carga_ms: ms(t0) })
}

fn candidato(e5: &Path, nome: &str, onnx: &Path) -> Result<Conjunto> {
    let t0 = Instant::now();
    let embed = TextEmbedding::try_new_from_user_defined(
        UserDefinedEmbeddingModel::new(std::fs::read(e5.join("model_int8.onnx"))?, tokenizer(e5)?)
            .with_pooling(Pooling::Mean),
        InitOptionsUserDefined::new(),
    )?;
    let pasta = onnx.parent().context("reranker sem pasta")?;
    let rerank = TextRerank::try_new_from_user_defined(
        UserDefinedRerankingModel::new(onnx.to_path_buf(), tokenizer(pasta)?),
        RerankInitOptionsUserDefined::new(),
    )
    .with_context(|| format!("carregando {}", onnx.display()))?;
    Ok(Conjunto { nome: format!("E5 int8 + {nome}"), embed, rerank, carga_ms: ms(t0) })
}

fn ms(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}

fn percentil(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn cosseno(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

fn medir(c: &mut Conjunto) -> Result<()> {
    let docs: Vec<String> = MEMORIAS.iter().map(|(t, b)| format!("{t}\n{b}")).collect();
    let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let passagens: Vec<String> = docs.iter().map(|d| passage(d)).collect();

    let t0 = Instant::now();
    let vetores = c.embed.embed(passagens.clone(), None)?;
    let indexar_ms = ms(t0);

    println!("\n=== {} ===", c.nome);
    println!("carga dos modelos: {:.0} ms · indexar {} memórias: {:.0} ms", c.carga_ms, docs.len(), indexar_ms);

    let mut acertos_vetor = 0;
    let mut acertos_rerank = 0;
    let mut menor_relevante = f32::MAX;
    let mut maior_irrelevante = f32::MIN;
    let mut maior_distracao = f32::MIN;
    for (pergunta, esperado) in PERGUNTAS {
        let qv = c.embed.embed(vec![query(pergunta)], None)?.pop().unwrap();
        let mut por_vetor: Vec<(usize, f32)> = vetores.iter().enumerate().map(|(i, v)| (i, cosseno(&qv, v))).collect();
        por_vetor.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let ranking = c.rerank.rerank(*pergunta, refs.clone(), false, None)?;
        let topo = &ranking[0];
        let titulo_topo = MEMORIAS[topo.index].0;
        let relev_topo = cross_score(topo.score);
        let cos_topo = por_vetor[0].1;
        match esperado {
            Some(t) => {
                let pos_vetor = por_vetor.iter().position(|(i, _)| MEMORIAS[*i].0 == *t).unwrap() + 1;
                let alvo = ranking.iter().find(|r| MEMORIAS[r.index].0 == *t).unwrap();
                let relev_alvo = cross_score(alvo.score);
                let segunda = ranking.iter().find(|r| MEMORIAS[r.index].0 != *t).map(|r| cross_score(r.score)).unwrap_or(0.0);
                if pos_vetor <= 3 { acertos_vetor += 1; }
                if titulo_topo == *t { acertos_rerank += 1; }
                menor_relevante = menor_relevante.min(relev_alvo);
                maior_distracao = maior_distracao.max(segunda);
                println!(
                    "  {} {pergunta}\n      vetor: posição {pos_vetor} (cos {cos_topo:.3}) · reranker: {:.3} (melhor distração {:.3}){}",
                    if titulo_topo == *t { "✔" } else { "✖" },
                    relev_alvo, segunda,
                    if titulo_topo == *t { String::new() } else { format!(" — veio \"{titulo_topo}\"") }
                );
            }
            None => {
                maior_irrelevante = maior_irrelevante.max(relev_topo);
                println!("  · {pergunta}\n      maior relevância: {relev_topo:.3} (\"{titulo_topo}\") · cos {cos_topo:.3}");
            }
        }
    }
    let com_resposta = PERGUNTAS.iter().filter(|(_, e)| e.is_some()).count();
    println!(
        "qualidade: vetor acha no top-3 em {acertos_vetor}/{com_resposta} · reranker acerta o 1º em {acertos_rerank}/{com_resposta}"
    );
    println!(
        "separação: menor relevante {menor_relevante:.3} · maior pergunta sem relação {maior_irrelevante:.3} · maior distração {maior_distracao:.3}"
    );

    for n in [12usize, 24] {
        let candidatos: Vec<&str> = refs.iter().copied().cycle().take(n).collect();
        let mut t_vetor = Vec::new();
        let mut t_rerank = Vec::new();
        for rodada in 0..30 {
            let (pergunta, _) = PERGUNTAS[rodada % PERGUNTAS.len()];
            let t0 = Instant::now();
            c.embed.embed(vec![query(pergunta)], None)?;
            let v = ms(t0);
            let t0 = Instant::now();
            c.rerank.rerank(pergunta, candidatos.clone(), false, None)?;
            let r = ms(t0);
            if rodada >= 3 {
                t_vetor.push(v);
                t_rerank.push(r);
            }
        }
        let mut total: Vec<f64> = t_vetor.iter().zip(&t_rerank).map(|(a, b)| a + b).collect();
        println!(
            "velocidade ({n} candidatos): vetor p50 {:.1} ms · reranker p50 {:.1} ms / p95 {:.1} · busca completa p50 {:.1} ms",
            percentil(&mut t_vetor, 0.5),
            percentil(&mut t_rerank, 0.5), percentil(&mut t_rerank, 0.95),
            percentil(&mut total, 0.5)
        );
    }
    Ok(())
}

/// Relevância crua e cosseno de cada memória para consultas avulsas.
fn diagnosticar(c: &mut Conjunto, consultas: &[String]) -> Result<()> {
    let docs: Vec<String> = MEMORIAS.iter().map(|(t, b)| format!("{t}\n{b}")).collect();
    let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let passagens: Vec<String> = docs.iter().map(|d| passage(d)).collect();
    let vetores = c.embed.embed(passagens, None)?;
    println!("\n=== diagnóstico: {} ===", c.nome);
    for q in consultas {
        let qv = c.embed.embed(vec![query(q)], None)?.pop().unwrap();
        let ranking = c.rerank.rerank(q.as_str(), refs.clone(), false, None)?;
        println!("  {q}");
        for r in ranking.iter().take(4) {
            println!(
                "      {:.3}  cos {:.3}  {}",
                cross_score(r.score),
                cosseno(&qv, &vetores[r.index]),
                MEMORIAS[r.index].0
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let valor = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1));
    let Some(e5) = valor("--e5").map(PathBuf::from) else {
        bail!("uso: bench --e5 <pasta> --reranker nome=arquivo.onnx [...] [--sem-atual]");
    };
    let mut conjuntos = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a != "--reranker" {
            continue;
        }
        let spec = args.get(i + 1).context("--reranker sem valor")?;
        let (nome, onnx) = spec.split_once('=').context("--reranker espera nome=arquivo.onnx")?;
        conjuntos.push(candidato(&e5, nome, Path::new(onnx))?);
    }
    if !args.iter().any(|a| a == "--sem-atual") {
        conjuntos.push(atual()?);
    }
    if let Some(i) = args.iter().position(|a| a == "--diagnostico") {
        let consultas: Vec<String> = args[i + 1..].iter().take_while(|a| !a.starts_with("--")).cloned().collect();
        for c in &mut conjuntos {
            diagnosticar(c, &consultas)?;
        }
        return Ok(());
    }
    println!("threads disponíveis: {}", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
    for c in &mut conjuntos {
        medir(c)?;
    }
    Ok(())
}
