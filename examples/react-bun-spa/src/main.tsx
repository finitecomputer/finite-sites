import React from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Routes, Route, Link, useParams } from "react-router-dom";

const style: React.CSSProperties = {
  fontFamily: "system-ui, sans-serif",
  background: "#0b0b0f",
  color: "#e8e8ee",
  minHeight: "100vh",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  margin: 0,
};

function Page({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <main style={{ maxWidth: "28rem", textAlign: "center", padding: "2rem" }}>
      <nav style={{ display: "flex", gap: "1rem", justifyContent: "center", marginBottom: "2rem" }}>
        <Link style={{ color: "#8b8bf0" }} to="/">home</Link>
        <Link style={{ color: "#8b8bf0" }} to="/counter">counter</Link>
        <Link style={{ color: "#8b8bf0" }} to="/greet/finite">greeter</Link>
      </nav>
      <h1 style={{ fontSize: "1.4rem" }}>{title}</h1>
      {children}
      <p style={{ color: "#9a9aa8", fontSize: "0.9rem", lineHeight: 1.5 }}>
        React 19 + React Router 7, bundled with <code>bun build</code>, deployed
        as a Project Output with <code>spa = true</code>. Refresh on any route.
      </p>
    </main>
  );
}

function Counter() {
  const [count, setCount] = React.useState(0);
  return (
    <Page title="counter">
      <button
        style={{ padding: "0.6rem 1.2rem", borderRadius: 8, border: "none", background: "#5b5bd6", color: "white", cursor: "pointer", fontSize: "1rem" }}
        onClick={() => setCount((c) => c + 1)}
      >
        clicked {count} time{count === 1 ? "" : "s"}
      </button>
    </Page>
  );
}

function Greeter() {
  const { name } = useParams();
  return <Page title={`hello, ${name}!`} />;
}

function App() {
  return (
    <div style={style}>
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<Page title="react + bun, hosted by finite" />} />
          <Route path="/counter" element={<Counter />} />
          <Route path="/greet/:name" element={<Greeter />} />
          <Route path="*" element={<Page title="client-side 404 (still the app shell)" />} />
        </Routes>
      </BrowserRouter>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
