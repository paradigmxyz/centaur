import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/app/App";
import { createRouter } from "@/app/router";
import { registerPwa } from "@/features/auth-pages/pwa";
import "@/index.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");

registerPwa();

createRoot(root).render(
  <StrictMode>
    <App router={createRouter()} />
  </StrictMode>,
);
