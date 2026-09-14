// Exercita page.js e action.js num DOM falso — sem navegador, sem container.
import { readFileSync } from "node:fs";

const read = readFileSync("crates/sandbox/src/page.js", "utf8")
  .replace("__LIMIT__", "60")
  .replace("__TEXT_LIMIT__", "1500");

// DOM mínimo, só o que os scripts usam.
function makeEl(tag, props = {}) {
  const el = {
    tagName: tag.toUpperCase(),
    attrs: props.attrs || {},
    innerText: props.text || "",
    textContent: props.text || "",
    value: props.value,
    type: props.type,
    disabled: props.disabled || false,
    checked: props.checked || false,
    required: props.required || false,
    _w: props.w ?? 100,
    _h: props.h ?? 20,
    _display: props.display || "block",
    clicked: 0,
    events: [],
    getAttribute: (k) => el.attrs[k] ?? null,
    setAttribute: (k, v) => (el.attrs[k] = v),
    getBoundingClientRect: () => ({
      width: el._w, height: el._h, top: 0, bottom: el._h, right: el._w, left: 0,
    }),
    scrollIntoView: () => {},
    focus: () => (global.document.activeElement = el),
    click: () => el.clicked++,
    dispatchEvent: (e) => el.events.push(e.type),
  };
  return el;
}

const botao = makeEl("button", { text: "Salvar alterações" });
const campo = makeEl("input", { type: "email", value: "", attrs: { placeholder: "Seu e-mail" }, required: true });
const link = makeEl("a", { text: "Sair", attrs: { href: "/sair" } });
const oculto = makeEl("button", { text: "Invisível", display: "none" });
const minusculo = makeEl("button", { text: "Pixel", w: 1, h: 1 });
const elementos = [botao, campo, link, oculto, minusculo];

global.window = {};
global.innerHeight = 800;
global.getComputedStyle = (el) => ({ visibility: "visible", display: el._display, opacity: "1" });
global.location = { href: "http://localhost:3000/form" };
global.document = {
  title: "Formulário",
  activeElement: null,
  body: { innerText: "Preencha o formulário\n\n\n\nEnvie" },
  querySelectorAll: () => elementos,
  querySelector: (sel) => {
    const m = sel.match(/data-orch-ref="(\w+)"/);
    return m ? elementos.find((e) => e.attrs["data-orch-ref"] === m[1]) : null;
  },
};
global.HTMLInputElement = function () {};
global.HTMLTextAreaElement = function () {};
global.Event = class { constructor(t) { this.type = t; } };
Object.getOwnPropertyDescriptor = () => undefined;

const snap = JSON.parse(eval(read));
const falhas = [];
const ok = (cond, msg) => { if (!cond) falhas.push(msg); };

ok(snap.url === "http://localhost:3000/form", "url");
ok(snap.title === "Formulário", "título");
ok(snap.elements.length === 3, `deveria listar 3 visíveis, listou ${snap.elements.length}: ${JSON.stringify(snap.elements)}`);
ok(snap.elements[0].startsWith("[e1] button "), `formato: ${snap.elements[0]}`);
ok(snap.elements[0].includes("Salvar alterações"), "texto do botão");
ok(snap.elements[1].includes("input:email"), `tipo do campo: ${snap.elements[1]}`);
ok(snap.elements[1].includes("Seu e-mail"), "placeholder vira rótulo");
ok(snap.elements[1].includes("obrigatório"), "estado obrigatório");
ok(snap.elements[2].includes("link"), "âncora vira link");
ok(!JSON.stringify(snap.elements).includes("Invisível"), "elemento oculto não entra");
ok(!JSON.stringify(snap.elements).includes("Pixel"), "elemento de 1px não entra");
ok(!snap.text.includes("\n\n\n"), "texto normalizado");

// Ação por referência.
const act = readFileSync("crates/sandbox/src/action.js", "utf8")
  .replace("__KIND__", "click").replace("__REF__", "e1").replace("__VALUE__", '""');
const r1 = JSON.parse(eval(act));
ok(r1.ok === true, `clique deveria funcionar: ${JSON.stringify(r1)}`);
ok(botao.clicked === 1, "o botão foi clicado");

const digitar = readFileSync("crates/sandbox/src/action.js", "utf8")
  .replace("__KIND__", "type").replace("__REF__", "e2").replace("__VALUE__", '"eu@exemplo.com"');
const r2 = JSON.parse(eval(digitar));
ok(r2.ok === true, `digitar deveria funcionar: ${JSON.stringify(r2)}`);
ok(campo.value === "eu@exemplo.com", `valor: ${campo.value}`);
ok(campo.events.includes("input") && campo.events.includes("change"), "eventos de framework disparados");

const ruim = readFileSync("crates/sandbox/src/action.js", "utf8")
  .replace("__KIND__", "click").replace("__REF__", "e99").replace("__VALUE__", '""');
const r3 = JSON.parse(eval(ruim));
ok(r3.ok === false && r3.error.includes("ui_snapshot"), "referência inválida ensina o próximo passo");

if (falhas.length) { console.error("FALHAS:\n- " + falhas.join("\n- ")); process.exit(1); }
console.log(`ok — ${snap.elements.length} elementos lidos, ações funcionando`);
console.log("exemplo do que o orquestrador recebe:\n" + snap.elements.join("\n"));
