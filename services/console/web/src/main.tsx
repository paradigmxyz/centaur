import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { z } from "zod";

import { App } from "@/app/App";
import { createRouter } from "@/app/router";
import { registerPwa } from "@/features/auth-pages/pwa";
import "@/index.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

// zod compiles object schemas with `new Function` unless jitless, and its
// feature probe alone trips the CSP (script-src 'self', no 'unsafe-eval').
z.config({ jitless: true });

registerPwa();

createRoot(root).render(
  <StrictMode>
    <App router={createRouter()} />
  </StrictMode>,
);
