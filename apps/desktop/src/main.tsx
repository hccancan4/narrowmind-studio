import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { ChatPreview } from "./ChatPreview";
import "./index.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root mount point");

// Route on URL hash. The Tauri WebviewWindow opens /#/chat-preview for the floating
// chat preview window; the main app keeps /. Keeps us off React Router for now —
// a hash check is fewer moving parts.
const isChatPreview = window.location.hash.startsWith("#/chat-preview");

ReactDOM.createRoot(root).render(
  <React.StrictMode>{isChatPreview ? <ChatPreview /> : <App />}</React.StrictMode>,
);
