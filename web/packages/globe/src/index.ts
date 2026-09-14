// O globo de memórias: cada memória é um nó na superfície de uma esfera,
// ligada por um ramo ao seu polo (Global ou o projeto), e — quando o índice
// vetorial está de pé — aos vizinhos de sentido mais próximos.
//
// O mesmo componente serve a página da memória e o app desktop. Sem WebGL,
// cai para um grafo 2D com as mesmas cores e interações.

import ForceGraph3D from "3d-force-graph";
import ForceGraph from "force-graph";
import SpriteText from "three-spritetext";
import type { Object3D } from "three";
import { forceRadial } from "d3-force-3d";

export type Kind = "security" | "architecture" | "practice" | "syntax" | "decision";

export interface GraphNode {
  id: string;
  type: "hub" | "memory";
  label: string;
  kind?: Kind;
  project?: string;
  scope?: string;
  origin?: string;
  author?: string;
  priority?: number;
  count?: number;
  x?: number;
  y?: number;
  z?: number;
}

export interface GraphLink {
  source: string | GraphNode;
  target: string | GraphNode;
  type: "branch" | "semantic";
  weight?: number;
}

export interface GraphData {
  nodes: GraphNode[];
  links: GraphLink[];
  semantic: boolean;
}

export const KIND_COLORS: Record<Kind, string> = {
  security: "#e5534b",
  architecture: "#6b95e8",
  practice: "#3fb0bf",
  syntax: "#5fb97a",
  decision: "#b387d9",
};

export const KIND_LABELS: Record<Kind, string> = {
  security: "segurança",
  architecture: "arquitetura",
  practice: "prática",
  syntax: "sintaxe",
  decision: "decisão",
};

export interface GlobeOptions {
  /** Clique num nó (ou no vazio, com `null`). */
  onSelect?: (node: GraphNode | null) => void;
  /** Força o modo sem animação (senão segue `prefers-reduced-motion`). */
  reducedMotion?: boolean;
  background?: string;
}

export interface Globe {
  readonly mode: "3d" | "2d";
  setData(data: GraphData): void;
  /** Acende estes nós (resultado de busca) e apaga o resto. */
  highlight(ids: string[]): void;
  /** Leva a câmera até o nó. */
  focus(id: string): void;
  resize(width: number, height: number): void;
  dispose(): void;
}

const ACCENT = "#6aa9d8";
const HUB_COLOR = "#e7e7ea";
const DIM = "#3a3a42";
const RADIUS = 170;

export function webglAvailable(): boolean {
  try {
    const canvas = document.createElement("canvas");
    return Boolean(canvas.getContext("webgl2") ?? canvas.getContext("webgl"));
  } catch {
    return false;
  }
}

/** Texto de usuário vai para dentro de HTML (tooltip): nunca sem escapar. */
export function escapeHtml(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/** Posições iniciais espalhadas numa esfera (espiral de Fibonacci). */
export function fibonacciSphere(count: number, radius: number): Array<[number, number, number]> {
  const golden = Math.PI * (3 - Math.sqrt(5));
  return Array.from({ length: count }, (_, i) => {
    const y = count === 1 ? 0 : 1 - (i / (count - 1)) * 2;
    const r = Math.sqrt(1 - y * y);
    const theta = golden * i;
    return [Math.cos(theta) * r * radius, y * radius, Math.sin(theta) * r * radius];
  });
}

function idOf(end: string | GraphNode): string {
  return typeof end === "string" ? end : end.id;
}

function tooltip(node: GraphNode): string {
  if (node.type === "hub") {
    const n = node.count ?? 0;
    return `<div class="globe-tip"><strong>${escapeHtml(node.label)}</strong><br>${n} memória${n === 1 ? "" : "s"}</div>`;
  }
  const kind = node.kind ? KIND_LABELS[node.kind] : "";
  const who = node.origin === "agent" ? `IA: ${node.author ?? ""}` : node.scope === "global" ? "global" : "dono";
  return `<div class="globe-tip"><strong>${escapeHtml(node.label)}</strong><br>${escapeHtml(kind)} · ${escapeHtml(who)}${
    node.project && node.scope !== "global" ? ` · ${escapeHtml(node.project)}` : ""
  }</div>`;
}

class State {
  highlighted = new Set<string>();
  hovered: string | null = null;
  neighbors = new Set<string>();
  links: GraphLink[] = [];

  setHover(id: string | null): void {
    this.hovered = id;
    this.neighbors = new Set();
    if (!id) return;
    for (const l of this.links) {
      const a = idOf(l.source);
      const b = idOf(l.target);
      if (a === id) this.neighbors.add(b);
      if (b === id) this.neighbors.add(a);
    }
  }

  nodeColor(node: GraphNode): string {
    const lit = this.highlighted.size === 0 || this.highlighted.has(node.id) || node.id === this.hovered || this.neighbors.has(node.id);
    if (!lit) return DIM;
    if (this.highlighted.has(node.id)) return node.type === "hub" ? HUB_COLOR : ACCENT;
    return node.type === "hub" ? HUB_COLOR : KIND_COLORS[node.kind ?? "decision"];
  }

  linkActive(link: GraphLink): boolean {
    const a = idOf(link.source);
    const b = idOf(link.target);
    if (this.hovered && (a === this.hovered || b === this.hovered)) return true;
    return this.highlighted.has(a) || this.highlighted.has(b);
  }
}

function nodeSize(node: GraphNode): number {
  return node.type === "hub" ? 6 + Math.min(node.count ?? 0, 40) * 0.25 : 1 + (node.priority ?? 0) * 0.2;
}

export function createGlobe(element: HTMLElement, options: GlobeOptions = {}): Globe {
  return webglAvailable() ? create3d(element, options) : create2d(element, options);
}

function create3d(element: HTMLElement, options: GlobeOptions): Globe {
  const reduced = options.reducedMotion ?? window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const state = new State();
  const graph = new ForceGraph3D(element, { controlType: "orbit" });

  graph
    .backgroundColor(options.background ?? "#111113")
    .showNavInfo(false)
    .nodeId("id")
    .nodeLabel((n) => tooltip(n as GraphNode))
    .nodeVal((n) => nodeSize(n as GraphNode))
    .nodeColor((n) => state.nodeColor(n as GraphNode))
    .nodeOpacity(0.95)
    .nodeResolution(12)
    .nodeThreeObjectExtend(true)
    .nodeThreeObject((n) => {
      const node = n as GraphNode;
      if (node.type !== "hub") return undefined as unknown as Object3D;
      const sprite = new SpriteText(node.label, 5, "#a1a1aa");
      sprite.fontFace = "IBM Plex Sans, system-ui, sans-serif";
      sprite.position.y = 10;
      return sprite;
    })
    .linkColor((l) => ((l as GraphLink).type === "semantic" ? "rgba(106,169,216,0.45)" : "rgba(190,190,200,0.28)"))
    .linkOpacity(0.6)
    .linkWidth((l) => (state.linkActive(l as GraphLink) ? 1.1 : 0))
    .linkDirectionalParticles((l) => (!reduced && state.linkActive(l as GraphLink) ? 3 : 0))
    .linkDirectionalParticleWidth(1.8)
    .linkDirectionalParticleSpeed(0.006)
    .linkDirectionalParticleColor(() => "#cfe6f7")
    .onNodeHover((n) => {
      state.setHover(n ? (n as GraphNode).id : null);
      element.style.cursor = n ? "pointer" : "";
      refresh();
    })
    .onNodeClick((n) => {
      options.onSelect?.(n as GraphNode);
      focusNode(n as GraphNode);
    })
    .onBackgroundClick(() => options.onSelect?.(null));

  // Forma de globo: tudo puxado para a superfície da esfera.
  graph.d3Force("radial", forceRadial(RADIUS).strength(0.9));
  graph.d3Force("charge")?.strength?.(-12);
  graph.d3Force("link")?.distance?.((l: GraphLink) => (l.type === "branch" ? 40 : 25));

  // Enquadra o globo inteiro quando a simulação assenta (a cada dado novo).
  // Um "voar até o nó" pedido antes disso espera o enquadramento terminar:
  // duas animações de câmera ao mesmo tempo deixavam a tela vazia.
  let enquadrado = false;
  let focoPendente: string | null = null;
  const ENQUADRAR_MS = reduced ? 0 : 700;
  graph.onEngineStop(() => {
    if (enquadrado) return;
    enquadrado = true;
    graph.zoomToFit(ENQUADRAR_MS, 60);
    if (focoPendente) {
      const id = focoPendente;
      focoPendente = null;
      window.setTimeout(() => focusNode(acharNo(id)), ENQUADRAR_MS + 50);
    }
  });

  function acharNo(id: string): GraphNode | undefined {
    return (graph.graphData().nodes as GraphNode[]).find((n) => n.id === id);
  }

  const controls = graph.controls() as { autoRotate?: boolean; autoRotateSpeed?: number; addEventListener?: (e: string, f: () => void) => void };
  let resume: number | undefined;
  if (!reduced && controls) {
    controls.autoRotate = true;
    controls.autoRotateSpeed = 0.35;
    // Parar de girar enquanto a pessoa mexe; voltar alguns segundos depois.
    controls.addEventListener?.("start", () => {
      controls.autoRotate = false;
      window.clearTimeout(resume);
    });
    controls.addEventListener?.("end", () => {
      window.clearTimeout(resume);
      resume = window.setTimeout(() => (controls.autoRotate = true), 5000);
    });
  }

  function refresh(): void {
    graph.nodeColor(graph.nodeColor()).linkWidth(graph.linkWidth()).linkDirectionalParticles(graph.linkDirectionalParticles());
  }

  function focusNode(node: GraphNode | undefined): void {
    if (!node || node.x === undefined || node.y === undefined || node.z === undefined) return;
    // Longe o bastante para o globo continuar inteiro na tela.
    const distance = RADIUS * 1.5;
    const ratio = 1 + distance / Math.max(1, Math.hypot(node.x, node.y, node.z));
    graph.cameraPosition(
      { x: node.x * ratio, y: node.y * ratio, z: node.z * ratio },
      { x: node.x, y: node.y, z: node.z },
      reduced ? 0 : 1200,
    );
  }

  return {
    mode: "3d",
    setData(data) {
      const start = fibonacciSphere(data.nodes.length, RADIUS);
      const nodes = data.nodes.map((n, i) => ({ ...n, x: start[i][0], y: start[i][1], z: start[i][2] }));
      state.links = data.links;
      enquadrado = false;
      graph.graphData({ nodes, links: data.links.map((l) => ({ ...l })) });
    },
    highlight(ids) {
      state.highlighted = new Set(ids);
      refresh();
    },
    focus(id) {
      if (!enquadrado) {
        focoPendente = id;
        return;
      }
      focusNode(acharNo(id));
    },
    resize(width, height) {
      graph.width(width).height(height);
    },
    dispose() {
      window.clearTimeout(resume);
      graph._destructor();
    },
  };
}

function create2d(element: HTMLElement, options: GlobeOptions): Globe {
  const state = new State();
  const graph = new ForceGraph(element);
  graph
    .backgroundColor(options.background ?? "#111113")
    .nodeId("id")
    .nodeLabel((n) => tooltip(n as GraphNode))
    .nodeVal((n) => nodeSize(n as GraphNode))
    .nodeColor((n) => state.nodeColor(n as GraphNode))
    .linkColor((l) => ((l as GraphLink).type === "semantic" ? "rgba(106,169,216,0.45)" : "rgba(190,190,200,0.28)"))
    .linkWidth((l) => (state.linkActive(l as GraphLink) ? 1.5 : 0.6))
    .onNodeHover((n) => {
      state.setHover(n ? (n as GraphNode).id : null);
      element.style.cursor = n ? "pointer" : "";
      graph.nodeColor(graph.nodeColor());
    })
    .onNodeClick((n) => {
      options.onSelect?.(n as GraphNode);
      const node = n as GraphNode;
      if (node.x !== undefined && node.y !== undefined) graph.centerAt(node.x, node.y, 800).zoom(3, 800);
    })
    .onBackgroundClick(() => options.onSelect?.(null));

  return {
    mode: "2d",
    setData(data) {
      state.links = data.links;
      graph.graphData({ nodes: data.nodes.map((n) => ({ ...n })), links: data.links.map((l) => ({ ...l })) });
    },
    highlight(ids) {
      state.highlighted = new Set(ids);
      graph.nodeColor(graph.nodeColor()).linkWidth(graph.linkWidth());
    },
    focus(id) {
      const node = (graph.graphData().nodes as GraphNode[]).find((n) => n.id === id);
      if (node?.x !== undefined && node.y !== undefined) graph.centerAt(node.x, node.y, 800).zoom(3, 800);
    },
    resize(width, height) {
      graph.width(width).height(height);
    },
    dispose() {
      graph._destructor();
    },
  };
}
