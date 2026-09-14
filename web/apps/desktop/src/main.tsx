import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@xterm/xterm/css/xterm.css";
import "./estilos.css";

import { createRoot } from "react-dom/client";
import { App } from "./App";

// Sem StrictMode: ele monta cada efeito duas vezes em desenvolvimento, e cada
// montagem de terminal assina a saída da CLI — a tela sairia duplicada.
createRoot(document.getElementById("app")!).render(<App />);
