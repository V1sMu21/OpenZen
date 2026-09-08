import { mount } from "svelte";
import App from "./App.svelte";
import "./app.css";

const app = mount(App, {
  target: document.getElementById("app")!,
});

// Dev-only console hook: lets a scratch tab drive the REAL chat store
// (handleSSEEvent / addUserMessage / startAssistantMessage) for streaming
// repro. Tree-shaken from production builds by the import.meta.env.DEV guard.
if (import.meta.env.DEV) {
  void import("./lib/stores/chat").then((m) => {
    (window as unknown as Record<string, unknown>).__ozChat = m.chat;
  });
}

export default app;
