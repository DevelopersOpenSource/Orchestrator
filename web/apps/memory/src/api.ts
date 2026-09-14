// Cliente da API da memória (mesma origem: a página é servida pelo memoryd).

import type { GraphData, Kind } from "@orchestrator/globe";

export interface Memory {
  id: string;
  project: string;
  kind: Kind;
  scope: "project" | "global";
  origin: "user" | "agent";
  author: string;
  title: string;
  body: string;
  priority: number;
  created_at: string;
  updated_at: string;
  score?: number;
}

export interface Health {
  ok: boolean;
  models: string;
  models_ready: boolean;
  chroma: boolean;
  semantic: boolean;
  memories: number;
  port: number;
  version: string;
}

export interface SearchResult {
  query: string;
  took_ms: number;
  semantic: boolean;
  reranked: boolean;
  hits: Memory[];
}

export interface SearchOptions {
  project?: string;
  kind?: string;
  limit?: number;
  rerank?: boolean;
  signal?: AbortSignal;
}

export interface NewMemory {
  project?: string;
  scope?: "project" | "global";
  kind: Kind;
  title: string;
  body: string;
  priority: number;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

function query(params: Record<string, string | number | boolean | undefined>): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== "") q.set(k, String(v));
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

async function call<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(`./api/${path}`, init);
  const body = await response.json().catch(() => ({}));
  if (!response.ok || body.ok === false) {
    throw new ApiError(body.error ?? `a API respondeu ${response.status}`, response.status);
  }
  return body as T;
}

function write(token: string, method: string, body?: unknown): RequestInit {
  return {
    method,
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  };
}

export const api = {
  health: () => call<Health>("health"),

  search: (q: string, o: SearchOptions = {}) =>
    call<SearchResult>(`search${query({ q, project: o.project, kind: o.kind, limit: o.limit, rerank: o.rerank })}`, {
      signal: o.signal,
    }),

  memories: (project?: string) =>
    call<{ memories: Memory[] }>(`memories${query({ project })}`).then((r) => r.memories),

  graph: (project?: string) => call<GraphData>(`graph${query({ project })}`),

  create: (token: string, memory: NewMemory) =>
    call<{ memory: Memory }>("memories", write(token, "POST", memory)).then((r) => r.memory),

  update: (token: string, id: string, changes: Partial<Pick<Memory, "kind" | "title" | "body" | "priority">>) =>
    call<{ memory: Memory }>(`memories/${encodeURIComponent(id)}`, write(token, "PUT", changes)).then((r) => r.memory),

  remove: (token: string, id: string) => call<{ ok: boolean }>(`memories/${encodeURIComponent(id)}`, write(token, "DELETE")),
};
