// Lê a página como uma LISTA DE ELEMENTOS, não como imagem.
//
// É o que deixa o orquestrador testar sem gastar contexto: em vez de um
// screenshot (caro e difícil de agir em cima), ele recebe linhas do tipo
//   [e12] button "Salvar" (foco)
// e age por referência — ui_click e12. O mapa ref→elemento fica em
// window.__orch para as ações seguintes acharem o mesmo nó.
(() => {
  const LIMIT = __LIMIT__;
  const TEXT_MAX = 80;

  const seen = new Map();
  window.__orch = { refs: seen };

  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 2 || r.height < 2) return false;
    const s = getComputedStyle(el);
    if (s.visibility === "hidden" || s.display === "none" || s.opacity === "0")
      return false;
    // Fora da janela (acima/à esquerda) não interessa agora.
    return r.bottom > 0 && r.right > 0 && r.top < innerHeight + r.height;
  };

  const label = (el) => {
    const raw =
      el.getAttribute("aria-label") ||
      el.getAttribute("placeholder") ||
      el.getAttribute("title") ||
      el.value ||
      el.innerText ||
      el.textContent ||
      "";
    const t = raw.replace(/\s+/g, " ").trim();
    return t.length > TEXT_MAX ? t.slice(0, TEXT_MAX - 1) + "…" : t;
  };

  const role = (el) => {
    const explicit = el.getAttribute("role");
    if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === "a") return "link";
    if (tag === "input") return `input:${el.type || "text"}`;
    if (tag === "textarea") return "textarea";
    if (tag === "select") return "select";
    if (tag === "button") return "button";
    return tag;
  };

  const state = (el) => {
    const bits = [];
    if (el.disabled) bits.push("desabilitado");
    if (el.checked) bits.push("marcado");
    if (el === document.activeElement) bits.push("foco");
    if (el.required) bits.push("obrigatório");
    const v = el.value;
    if (typeof v === "string" && v && el.type !== "password")
      bits.push(`valor="${v.slice(0, 40)}"`);
    return bits.length ? ` (${bits.join(", ")})` : "";
  };

  const SELECTOR =
    'a[href], button, input, textarea, select, summary, [role="button"], ' +
    '[role="link"], [role="tab"], [role="checkbox"], [role="menuitem"], ' +
    "[onclick], [contenteditable='true'], [tabindex]:not([tabindex='-1'])";

  const lines = [];
  let n = 0;
  for (const el of document.querySelectorAll(SELECTOR)) {
    if (n >= LIMIT) break;
    if (!visible(el)) continue;
    const ref = "e" + ++n;
    seen.set(ref, el);
    el.setAttribute("data-orch-ref", ref);
    lines.push(`[${ref}] ${role(el)} "${label(el)}"${state(el)}`);
  }

  // Texto da página, para o orquestrador entender o que está vendo sem
  // precisar da imagem. Cortado: o objetivo é caber no contexto.
  const bodyText = (document.body ? document.body.innerText : "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();

  return JSON.stringify({
    url: location.href,
    title: document.title,
    elements: lines,
    truncated: n >= LIMIT,
    text: bodyText.length > __TEXT_LIMIT__
      ? bodyText.slice(0, __TEXT_LIMIT__) + "\n…(texto cortado)"
      : bodyText,
  });
})()
