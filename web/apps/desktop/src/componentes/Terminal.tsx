import { useEffect, useRef } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { nucleo } from "../nucleo";

function cor(nome: string, reserva: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(nome).trim() || reserva;
}

/** Uma CLI real: o xterm.js recebe os bytes do PTY e devolve o que se digita. */
export function Terminal({ indice, focado, aoFocar }: { indice: number; focado: boolean; aoFocar: () => void }) {
  const caixa = useRef<HTMLDivElement>(null);
  const termo = useRef<XTerm | null>(null);
  const repintar = useRef<() => void>(() => {});

  useEffect(() => {
    const el = caixa.current;
    if (!el) return;
    let ativo = true;
    const term = new XTerm({
      fontFamily: '"JetBrains Mono", ui-monospace, monospace',
      fontSize: 12.5,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      allowProposedApi: false,
      theme: {
        background: cor("--term", "#0e0e10"),
        foreground: cor("--tx", "#e7e7ea"),
        cursor: cor("--ac", "#6aa9d8"),
        selectionBackground: cor("--ac-bg", "#1b2a36"),
      },
    });
    const ajuste = new FitAddon();
    term.loadAddon(ajuste);
    term.open(el);
    // Sem WebGL de propósito: o renderizador acelerado apagava o terminal
    // sozinho (perda de contexto do canvas, tela cinza ao trocar de aba). O
    // renderizador padrão (DOM) é um pouco mais lento mas NUNCA some.
    termo.current = term;

    const redimensionar = () => {
      if (!ativo || el.clientWidth === 0 || el.clientHeight === 0) return;
      ajuste.fit();
      void nucleo.redimensionarTerminal(indice, term.rows, term.cols);
    };
    // Refit + REPAINT forçado — o que estava cinza volta a aparecer sem esperar
    // uma tecla. `refresh` redesenha todas as linhas visíveis.
    const forcarRepintura = () => {
      if (!ativo || el.clientWidth === 0 || el.clientHeight === 0) return;
      ajuste.fit();
      void nucleo.redimensionarTerminal(indice, term.rows, term.cols);
      term.refresh(0, Math.max(0, term.rows - 1));
    };
    repintar.current = () => requestAnimationFrame(forcarRepintura);

    const observador = new ResizeObserver(redimensionar);
    observador.observe(el);
    // Quando o terminal volta a ficar VISÍVEL (troca de aba), repinta.
    const visivel = new IntersectionObserver(
      (entradas) => {
        if (entradas.some((e) => e.isIntersecting)) repintar.current();
      },
      { threshold: 0.01 },
    );
    visivel.observe(el);
    redimensionar();

    const digitado = term.onData((dados) => void nucleo.escreverTerminal(indice, dados));
    void nucleo
      .assinarTerminal(indice, (bytes) => {
        if (ativo) term.write(bytes);
      })
      .catch((e) => term.write(`\r\n\x1b[31m${String(e)}\x1b[0m\r\n`));

    return () => {
      ativo = false;
      observador.disconnect();
      visivel.disconnect();
      digitado.dispose();
      term.dispose();
      termo.current = null;
    };
  }, [indice]);

  useEffect(() => {
    if (focado) {
      termo.current?.focus();
      // Ao focar (inclui voltar pra aba), garante que a tela apareça.
      repintar.current();
    }
  }, [focado]);

  return <div className="xterm-caixa" ref={caixa} onMouseDown={aoFocar} />;
}
