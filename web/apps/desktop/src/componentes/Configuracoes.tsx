import { useEffect, useState } from "react";
import { nucleo, type Foto, type ProvedorEdit } from "../nucleo";

const KINDS: { id: string; rotulo: string }[] = [
  { id: "open_ai_compat", rotulo: "HTTP compatível (OpenAI/Groq/OpenRouter/Ollama)" },
  { id: "claude_cli", rotulo: "Claude Code (CLI)" },
  { id: "codex_cli", rotulo: "Codex (CLI)" },
  { id: "kimi_cli", rotulo: "Kimi (CLI)" },
  { id: "antigravity_cli", rotulo: "Antigravity (CLI)" },
  { id: "opencode_cli", rotulo: "OpenCode (CLI)" },
];

const NOVO: ProvedorEdit = { nome: "", kind: "open_ai_compat", baseUrl: "", model: "", apiKeyEnv: "", tools: true, temChave: false };

function EditorProvedores() {
  const [lista, setLista] = useState<ProvedorEdit[]>([]);
  const [edit, setEdit] = useState<(ProvedorEdit & { chave?: string }) | null>(null);
  const [aviso, setAviso] = useState("");

  const recarregar = () => nucleo.configProvedores().then(setLista).catch((e) => setAviso(String(e)));
  useEffect(() => {
    void recarregar();
  }, []);

  const salvar = async () => {
    if (!edit) return;
    try {
      setAviso(await nucleo.configProvedorSalvar(edit));
      setEdit(null);
      await recarregar();
    } catch (e) {
      setAviso(String(e));
    }
  };
  const remover = async (nome: string) => {
    try {
      setAviso(await nucleo.configProvedorRemover(nome));
      await recarregar();
    } catch (e) {
      setAviso(String(e));
    }
  };

  const campo = (rot: string, val: string, set: (v: string) => void, ph = "") => (
    <label className="config-campo">
      <span className="dica">{rot}</span>
      <input value={val} placeholder={ph} onChange={(e) => set(e.target.value)} />
    </label>
  );

  return (
    <div className="config-bloco">
      <div className="config-modo-topo">
        <strong>Provedores e chaves</strong>
        <button className="botao-leve" style={{ marginLeft: "auto" }} onClick={() => setEdit({ ...NOVO })}>
          + Novo
        </button>
      </div>
      <p className="dica">Adicione/edite provedor, modelo e chave sem reiniciar. A chave fica guardada no app (não no config.json nem no git).</p>
      {edit ? (
        <div className="config-form">
          {campo("Nome", edit.nome, (v) => setEdit({ ...edit, nome: v }), "ex.: Groq")}
          <label className="config-campo">
            <span className="dica">Tipo</span>
            <select value={edit.kind} onChange={(e) => setEdit({ ...edit, kind: e.target.value })}>
              {KINDS.map((k) => (
                <option key={k.id} value={k.id}>
                  {k.rotulo}
                </option>
              ))}
            </select>
          </label>
          {edit.kind === "open_ai_compat" && campo("Endpoint (base_url)", edit.baseUrl, (v) => setEdit({ ...edit, baseUrl: v }), "https://api.groq.com/openai/v1")}
          {campo("Modelo padrão", edit.model, (v) => setEdit({ ...edit, model: v }), "ex.: llama-3.3-70b")}
          {campo("Nome da variável da chave", edit.apiKeyEnv, (v) => setEdit({ ...edit, apiKeyEnv: v }), "ex.: GROQ_API_KEY")}
          <label className="config-campo">
            <span className="dica">Chave {edit.temChave ? "(já há uma salva — deixe em branco para manter)" : ""}</span>
            <input type="password" value={edit.chave ?? ""} placeholder="cole a chave aqui" onChange={(e) => setEdit({ ...edit, chave: e.target.value })} />
          </label>
          <div className="config-form-acoes">
            <button className="botao" onClick={() => void salvar()}>
              Salvar
            </button>
            <button className="botao-leve" onClick={() => setEdit(null)}>
              Cancelar
            </button>
          </div>
        </div>
      ) : (
        <ul className="config-provs">
          {lista.map((p) => (
            <li key={p.nome}>
              <span className="pv-texto">
                <span className="pv-nome">{p.nome}</span>
                <span className="dica">
                  {p.model || "(modelo do provedor)"} · {p.apiKeyEnv ? (p.temChave ? "chave ✓" : "sem chave") : "sem chave"}
                </span>
              </span>
              <span className="ssh-acoes">
                <button className="botao-leve" onClick={() => setEdit({ ...p })}>
                  Editar
                </button>
                <button className="botao-leve" onClick={() => void remover(p.nome)}>
                  Remover
                </button>
              </span>
            </li>
          ))}
        </ul>
      )}
      {aviso && <p className="dica">{aviso}</p>}
    </div>
  );
}

const MODOS: { id: string; titulo: string; risco: string; texto: string }[] = [
  {
    id: "autonomo",
    titulo: "Autônomo",
    risco: "mais seguro",
    texto:
      "Sem você por perto: a IA reduz a própria permissão. O catastrófico é bloqueado e TUDO que é arriscado espera a sua decisão — nada arriscado é auto-aprovado.",
  },
  {
    id: "padrao",
    titulo: "Padrão",
    risco: "equilibrado",
    texto:
      "O catastrófico é bloqueado, o arriscado pausa e você responde. O orquestrador decide sozinho os pedidos de CLI de que tem certeza. Bom para o dia a dia com você por perto.",
  },
  {
    id: "bypass",
    titulo: "Bypass",
    risco: "você assume o risco",
    texto:
      "A trava libera TUDO — nem bloqueio, nem pausa (como um sudo). Use só com você no controle, acompanhando. A IA nunca liga isto sozinha; é uma escolha sua.",
  },
];

/**
 * Configurações do app. Por ora, o nível de permissão do orquestrador — o
 * quanto ele pode agir sozinho (terminal, ssh, comandos) antes de parar para
 * te perguntar.
 */
export function Configuracoes({ aberta, fechar, foto }: { aberta: boolean; fechar: () => void; foto: Foto }) {
  const [aviso, setAviso] = useState("");
  if (!aberta) return null;

  const trocar = async (id: string) => {
    setAviso("");
    try {
      setAviso(await nucleo.definirPermModo(id));
    } catch (e) {
      setAviso(String(e));
    }
  };

  return (
    <div className="sobreposicao" onMouseDown={fechar}>
      <div className="manual config" role="dialog" aria-label="Configurações" onMouseDown={(e) => e.stopPropagation()}>
        <header>
          <h2>Configurações</h2>
          <button className="botao" onClick={fechar}>
            Fechar
          </button>
        </header>
        <div className="config-corpo">
          <strong>Nível de permissão do orquestrador</strong>
          <p className="dica">
            Quanto o orquestrador pode fazer sozinho (rodar comando, terminal, SSH) antes de te perguntar. A regra vale para a IA;
            você continua no controle.
          </p>
          <div className="config-modos">
            {MODOS.map((m) => {
              const ativo = foto.permModo === m.id;
              return (
                <button
                  key={m.id}
                  className={`config-modo ${ativo ? "ativo" : ""} ${m.id === "bypass" ? "perigo" : ""}`}
                  onClick={() => void trocar(m.id)}
                >
                  <span className="config-modo-topo">
                    <strong>{m.titulo}</strong>
                    <span className="dica">{m.risco}</span>
                    {ativo && <span className="config-atual">● atual</span>}
                  </span>
                  <span className="dica">{m.texto}</span>
                </button>
              );
            })}
          </div>
          {aviso && <p className="dica">{aviso}</p>}
          <p className="dica">
            Mesmo no autônomo/padrão, as regras de segurança do projeto (bloquear <code>rm -rf</code>, <code>mkfs</code>, pausar
            deploy/força-bruta) continuam valendo. O bypass é o único que passa por cima delas.
          </p>

          <EditorProvedores />
        </div>
      </div>
    </div>
  );
}
