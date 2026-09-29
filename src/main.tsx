import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

if (/Mac/.test(navigator.userAgent)) document.documentElement.classList.add("mac");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
