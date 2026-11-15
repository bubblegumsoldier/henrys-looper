import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
// The design system, verbatim from design/ (tokens, icons, components, live), then what the app
// adds on top (interaction states, pulses, drawer, popovers).
import "./design/tokens.css";
import "./design/icons.css";
import "./design/app.css";
import "./design/live.css";
import "./shell/shell.css";

// Development only: lets a shell script drive the app for screenshots (see dev/autopilot.ts).
if (import.meta.env.DEV) void import("./dev/autopilot");

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
