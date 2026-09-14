// Age sobre um elemento pela referência devolvida no último snapshot.
// Reencontra o nó por `data-orch-ref` quando o mapa se perdeu (navegação).
(() => {
  const ref = "__REF__";
  const kind = "__KIND__";
  const value = __VALUE__;

  const el =
    (window.__orch && window.__orch.refs && window.__orch.refs.get(ref)) ||
    document.querySelector(`[data-orch-ref="${ref}"]`);
  if (!el) return JSON.stringify({ ok: false, error: `não achei ${ref} — chame ui_snapshot de novo` });

  el.scrollIntoView({ block: "center" });
  const describe = (el.getAttribute("aria-label") || el.innerText || el.value || el.tagName)
    .toString()
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 60);

  try {
    if (kind === "click") {
      el.click();
    } else if (kind === "type") {
      // Digitar só faz sentido em campo de texto: sem esta checagem o erro
      // que chega ao orquestrador é um "Illegal invocation" sem sentido.
      const editable =
        el.tagName === "INPUT" ||
        el.tagName === "TEXTAREA" ||
        el.isContentEditable ||
        el.getAttribute("contenteditable") === "true";
      if (!editable) {
        return JSON.stringify({
          ok: false,
          error: `${ref} é um <${el.tagName.toLowerCase()}>, não um campo de texto — use ui_click nele, ou ui_type na referência do campo`,
        });
      }
      if (el.isContentEditable) {
        el.focus();
        el.textContent = value;
        el.dispatchEvent(new Event("input", { bubbles: true }));
        return JSON.stringify({ ok: true, target: describe });
      }
      el.focus();
      const setter = Object.getOwnPropertyDescriptor(
        el instanceof HTMLTextAreaElement
          ? HTMLTextAreaElement.prototype
          : HTMLInputElement.prototype,
        "value"
      );
      if (setter && setter.set) setter.set.call(el, value);
      else el.value = value;
      // Frameworks (React e afins) só enxergam a mudança pelos eventos.
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    } else if (kind === "select") {
      el.focus();
      el.value = value;
      el.dispatchEvent(new Event("change", { bubbles: true }));
    } else {
      return JSON.stringify({ ok: false, error: `ação desconhecida: ${kind}` });
    }
  } catch (e) {
    return JSON.stringify({ ok: false, error: String(e) });
  }
  return JSON.stringify({ ok: true, target: describe });
})()
