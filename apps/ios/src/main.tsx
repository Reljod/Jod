import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./App";
import { JodClient } from "./client";
import { Conversation } from "./conversation";
import "./styles.css";

/**
 * Wiring, and nothing else.
 *
 * The base URL is empty on purpose: the daemon binds loopback and is reached
 * over the tailnet, so every route is same-origin and relative. In development
 * Vite proxies `/v1` to wherever `JOD_API_ORIGIN` points, which keeps the
 * session cookie behaving exactly as it will on the device.
 */
const conversation = new Conversation({
  client: new JodClient(),
  harness: "claude_code",
});

conversation.greet(
  "/help for commands · THINK reasoning · TOOLS output · AGENTS fleet · TEAM board",
);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App conversation={conversation} />
  </StrictMode>,
);
