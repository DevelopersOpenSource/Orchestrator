//! Paleta de comandos do chat: digitar `/` e ir achando o que existe.
//!
//! Antes era preciso já saber o nome do comando — nada na tela dizia quais
//! existiam, o que cada um precisava, ou por que um deles não ia funcionar
//! agora. A paleta responde as três coisas: filtra conforme você digita,
//! mostra a forma de uso, e diz o ESTADO de cada comando (pronto, precisa de
//! argumento, ou indisponível e por quê).

/// Um comando oferecido na paleta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// Nome canônico, com a barra (`/cli`).
    pub name: &'static str,
    /// Outros nomes aceitos.
    pub aliases: &'static [&'static str],
    /// Forma de uso, como se digita.
    pub usage: &'static str,
    /// O que ele faz, em uma linha.
    pub about: &'static str,
}

/// Se o comando dá para usar agora — e, quando não dá, por quê.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// Pode rodar do jeito que está.
    Ready,
    /// Roda, mas precisa de argumento (`/cli <nome>`).
    NeedsArgument(String),
    /// Não vai funcionar agora, e o motivo.
    Unavailable(String),
}

impl Readiness {
    pub fn label(&self) -> &str {
        match self {
            Readiness::Ready => "pronto",
            Readiness::NeedsArgument(s) => s,
            Readiness::Unavailable(s) => s,
        }
    }
}

/// Um item já filtrado, com o estado calculado.
#[derive(Debug, Clone)]
pub struct Entry {
    pub command: Command,
    pub readiness: Readiness,
}

/// O que a paleta precisa saber do App para dizer o estado de cada comando.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Ações esperando o dono aprovar ou negar.
    pub pending_decisions: usize,
    /// Perguntas do orquestrador esperando resposta do dono.
    pub open_questions: usize,
    pub open_clis: usize,
    /// Teto de cards por workspace.
    pub max_clis: usize,
    pub projects: usize,
    pub has_claude: bool,
    pub notify_on_done: bool,
    pub memories: usize,
    /// Há um seletor de pastas gráfico nesta máquina?
    pub has_dir_picker: bool,
}

/// Todos os comandos do chat.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "/cli",
        aliases: &[],
        usage: "/cli <nome> [comando]",
        about: "abre uma CLI real nomeada nesta workspace",
    },
    Command {
        name: "/agente",
        aliases: &["/task"],
        usage: "/agente <tarefa>",
        about: "agente headless que trabalha sozinho num card",
    },
    Command {
        name: "/modelo",
        aliases: &["/model"],
        usage: "/modelo [nome]",
        about: "modelo do chat (sem nome abre o seletor)",
    },
    Command {
        name: "/provedor",
        aliases: &["/provider"],
        usage: "/provedor [nome]",
        about: "quem responde no chat: Claude, ChatGPT, Kimi, Google… (sem nome abre a lista)",
    },
    Command {
        name: "/pasta",
        aliases: &["/dir"],
        usage: "/pasta <caminho|->",
        about: "pasta desta workspace (- volta à do projeto)",
    },
    Command {
        name: "/projeto",
        aliases: &["/project"],
        usage: "/projeto [nome]",
        about: "troca de projeto (restaura a conversa dele)",
    },
    Command {
        name: "/novo-projeto",
        aliases: &["/new-project"],
        usage: "/novo-projeto <nome> [caminho]",
        about: "cria um projeto novo e passa a trabalhar nele",
    },
    Command {
        name: "/nova",
        aliases: &["/new"],
        usage: "/nova",
        about: "zera a conversa desta workspace",
    },
    Command {
        name: "/aprovar",
        aliases: &["/approve"],
        usage: "/aprovar [id]",
        about: "aprova a decisão pendente",
    },
    Command {
        name: "/negar",
        aliases: &["/deny"],
        usage: "/negar [id]",
        about: "nega a decisão pendente",
    },
    Command {
        name: "/responder",
        aliases: &["/answer"],
        usage: "/responder <id> <resposta>",
        about: "responde a uma pergunta do orquestrador (número da alternativa ou texto)",
    },
    Command {
        name: "/auto",
        aliases: &[],
        usage: "/auto [on|off]",
        about: "avisar o orquestrador quando uma CLI concluir",
    },
    Command {
        name: "/caps",
        aliases: &[],
        usage: "/caps",
        about: "o que a build do `claude` suporta",
    },
    Command {
        name: "/ajuda",
        aliases: &["/help"],
        usage: "/ajuda",
        about: "abre o manual completo",
    },
];

/// Estado de um comando no contexto atual.
pub fn readiness(cmd: &Command, ctx: &Context) -> Readiness {
    match cmd.name {
        "/cli" if !ctx.has_claude => Readiness::Unavailable(
            "o binário `claude` não está no PATH".into(),
        ),
        "/cli" if ctx.max_clis > 0 && ctx.open_clis >= ctx.max_clis => Readiness::Unavailable(
            format!("workspace cheia ({} cards) — feche um ou use outra", ctx.open_clis),
        ),
        "/cli" => Readiness::NeedsArgument("precisa do nome da CLI".into()),
        "/agente" if !ctx.has_claude => {
            Readiness::Unavailable("o binário `claude` não está no PATH".into())
        }
        "/agente" => Readiness::NeedsArgument("precisa da tarefa".into()),
        "/pasta" if ctx.has_dir_picker => Readiness::Ready,
        "/pasta" => Readiness::NeedsArgument(
            "sem seletor gráfico aqui: informe o caminho (ou `-`)".into(),
        ),
        "/novo-projeto" => Readiness::NeedsArgument(if ctx.has_dir_picker {
            "precisa do nome (a pasta você escolhe no explorador)".into()
        } else {
            "precisa do nome e do caminho".into()
        }),
        "/aprovar" | "/negar" => {
            if ctx.pending_decisions == 0 {
                Readiness::Unavailable("nenhuma decisão pendente agora".into())
            } else {
                Readiness::Ready
            }
        }
        "/responder" if ctx.open_questions == 0 => {
            Readiness::Unavailable("nenhuma pergunta esperando você".into())
        }
        "/responder" => Readiness::NeedsArgument("precisa do id e da resposta".into()),
        "/projeto" if ctx.projects <= 1 => {
            Readiness::Unavailable("só existe um projeto — use /novo-projeto".into())
        }
        "/auto" => Readiness::NeedsArgument(format!(
            "agora: avisos {}",
            if ctx.notify_on_done { "ligados" } else { "DESLIGADOS" }
        )),
        "/nova" if ctx.memories == 0 => Readiness::Ready,
        "/caps" if !ctx.has_claude => {
            Readiness::Unavailable("o binário `claude` não está no PATH".into())
        }
        _ => Readiness::Ready,
    }
}

/// Filtra os comandos pelo que foi digitado depois da `/`.
///
/// Casa por prefixo primeiro (o que a pessoa espera ao digitar), depois por
/// conter o texto em qualquer lugar do nome, alias ou descrição.
pub fn filter(input: &str, ctx: &Context) -> Vec<Entry> {
    let termo = input.trim().trim_start_matches('/').to_lowercase();
    let entry = |c: &Command| Entry {
        command: c.clone(),
        readiness: readiness(c, ctx),
    };
    if termo.is_empty() {
        return COMMANDS.iter().map(entry).collect();
    }
    let nome = |c: &Command| c.name.trim_start_matches('/').to_lowercase();
    let mut prefixo: Vec<Entry> = Vec::new();
    let mut resto: Vec<Entry> = Vec::new();
    for c in COMMANDS {
        if nome(c).starts_with(&termo)
            || c.aliases
                .iter()
                .any(|a| a.trim_start_matches('/').to_lowercase().starts_with(&termo))
        {
            prefixo.push(entry(c));
        } else if nome(c).contains(&termo)
            || c.about.to_lowercase().contains(&termo)
            || c.aliases.iter().any(|a| a.to_lowercase().contains(&termo))
        {
            resto.push(entry(c));
        }
    }
    prefixo.extend(resto);
    prefixo
}

/// A entrada digitada abre a paleta? (começa com `/` e ainda não tem espaço)
pub fn should_open(input: &str) -> bool {
    input.starts_with('/') && !input.contains(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context {
            pending_decisions: 0,
            open_questions: 0,
            open_clis: 0,
            max_clis: 8,
            projects: 1,
            has_claude: true,
            notify_on_done: true,
            memories: 0,
            has_dir_picker: true,
        }
    }

    #[test]
    fn typing_filters_as_you_go() {
        let c = ctx();
        assert_eq!(filter("/", &c).len(), COMMANDS.len());
        let p = filter("/p", &c);
        assert!(p.iter().all(|e| e.command.name.starts_with("/p")
            || e.command.about.contains('p')
            || e.command.name.contains('p')));
        // Prefixo vem antes de casamento no meio.
        let nomes: Vec<&str> = filter("/pa", &c).iter().map(|e| e.command.name).collect();
        assert_eq!(nomes.first(), Some(&"/pasta"));
    }

    #[test]
    fn finds_by_alias_and_by_description() {
        let c = ctx();
        assert!(filter("/task", &c).iter().any(|e| e.command.name == "/agente"));
        assert!(filter("/provider", &c).iter().any(|e| e.command.name == "/provedor"));
        // Quem procura pelo nome da empresa acha o comando.
        assert!(filter("/chatgpt", &c).iter().any(|e| e.command.name == "/provedor"));
        // Descrição: "decisão" leva a /aprovar e /negar.
        let por_texto: Vec<&str> = filter("/decis", &c).iter().map(|e| e.command.name).collect();
        assert!(por_texto.contains(&"/aprovar"), "{por_texto:?}");
    }

    #[test]
    fn tells_why_a_command_will_not_work_now() {
        let mut c = ctx();
        // Sem decisão pendente, aprovar não faz nada — e a paleta diz isso.
        let e = filter("/aprovar", &c).remove(0);
        assert!(matches!(e.readiness, Readiness::Unavailable(_)));
        assert!(e.readiness.label().contains("nenhuma decisão"));

        c.pending_decisions = 2;
        let e = filter("/aprovar", &c).remove(0);
        assert_eq!(e.readiness, Readiness::Ready);
    }

    #[test]
    fn answering_is_only_offered_when_a_question_is_waiting() {
        let mut c = ctx();
        let e = filter("/responder", &c).remove(0);
        assert!(e.readiness.label().contains("nenhuma pergunta"));
        c.open_questions = 1;
        let e = filter("/answer", &c).remove(0);
        assert!(matches!(e.readiness, Readiness::NeedsArgument(_)));
    }

    #[test]
    fn full_workspace_blocks_opening_another_cli() {
        let mut c = ctx();
        c.open_clis = 8;
        let e = filter("/cli", &c).remove(0);
        assert!(matches!(e.readiness, Readiness::Unavailable(_)));
        assert!(e.readiness.label().contains("cheia"));
    }

    #[test]
    fn folder_commands_reflect_whether_a_picker_exists() {
        let mut c = ctx();
        // Com seletor, /pasta funciona sem argumento nenhum.
        assert_eq!(filter("/pasta", &c).remove(0).readiness, Readiness::Ready);
        c.has_dir_picker = false;
        let e = filter("/pasta", &c).remove(0);
        assert!(e.readiness.label().contains("informe o caminho"));
        let n = filter("/novo-projeto", &c).remove(0);
        assert!(n.readiness.label().contains("caminho"));
    }

    #[test]
    fn auto_shows_the_current_setting() {
        let mut c = ctx();
        assert!(filter("/auto", &c).remove(0).readiness.label().contains("ligados"));
        c.notify_on_done = false;
        assert!(filter("/auto", &c).remove(0).readiness.label().contains("DESLIGADOS"));
    }

    #[test]
    fn says_what_argument_is_missing() {
        let c = ctx();
        let e = filter("/cli", &c).remove(0);
        assert!(matches!(e.readiness, Readiness::NeedsArgument(_)));
        assert!(e.readiness.label().contains("nome"));
    }

    #[test]
    fn without_claude_the_cli_commands_are_unavailable() {
        let mut c = ctx();
        c.has_claude = false;
        for nome in ["/cli", "/agente", "/caps"] {
            let e = filter(nome, &c).remove(0);
            assert!(
                matches!(e.readiness, Readiness::Unavailable(_)),
                "{nome} deveria estar indisponível"
            );
            assert!(e.readiness.label().contains("claude"));
        }
    }

    #[test]
    fn single_project_cannot_switch_but_can_create() {
        let c = ctx();
        let trocar = filter("/projeto", &c).remove(0);
        assert!(matches!(trocar.readiness, Readiness::Unavailable(_)));
        assert!(trocar.readiness.label().contains("/novo-projeto"));
        let criar = filter("/novo-projeto", &c).remove(0);
        assert!(matches!(criar.readiness, Readiness::NeedsArgument(_)));
    }

    #[test]
    fn palette_opens_only_while_typing_the_command_name() {
        assert!(should_open("/"));
        assert!(should_open("/cl"));
        // Já digitou o argumento: a paleta sai da frente.
        assert!(!should_open("/cli frontend"));
        assert!(!should_open("texto normal"));
        assert!(!should_open(""));
    }

    #[test]
    fn unknown_text_returns_nothing_instead_of_everything() {
        let c = ctx();
        assert!(filter("/xyzinexistente", &c).is_empty());
    }

    #[test]
    fn every_command_has_usage_and_description() {
        for c in COMMANDS {
            assert!(c.usage.starts_with(c.name), "uso de {}: {}", c.name, c.usage);
            assert!(!c.about.is_empty(), "{} sem descrição", c.name);
        }
    }
}
