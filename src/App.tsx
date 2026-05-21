import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type ProfileSummary = {
  name: string;
  path: string;
  is_active: boolean;
};

function App() {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [active, setActive] = useState<string>("origin");
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    try {
      const list = await invoke<ProfileSummary[]>("list_profiles");
      const current = await invoke<string>("get_active_profile");
      setProfiles(list);
      setActive(current);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  async function onToggle(name: string) {
    try {
      await invoke("toggle_profile", { name });
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <main className="app">
      <header>
        <h1>Claude.md Toggler</h1>
        <p className="subtitle">
          Active: <strong>{active}</strong>
        </p>
      </header>

      {error && <div className="error">{error}</div>}

      <section className="profiles">
        <h2>Profiles</h2>
        <ul>
          <li
            key="origin"
            className={active === "origin" ? "active" : ""}
            onClick={() => onToggle("origin")}
          >
            <span className="dot" />
            origin
            <em>default backup</em>
          </li>
          {profiles.map((p) => (
            <li
              key={p.name}
              className={p.is_active ? "active" : ""}
              onClick={() => onToggle(p.name)}
            >
              <span className="dot" />
              {p.name}
            </li>
          ))}
        </ul>
      </section>

      <footer>
        <button onClick={refresh}>Refresh</button>
      </footer>
    </main>
  );
}

export default App;
