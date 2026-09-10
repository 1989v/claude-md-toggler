import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import Compare from "./Compare";
import "./styles.css";

// The compare window loads the same bundle; the hash picks the root. A second
// entry point would duplicate the build config for one screen.
const isCompare = window.location.hash === "#compare";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>{isCompare ? <Compare /> : <App />}</React.StrictMode>,
);
