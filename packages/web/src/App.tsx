import { useEffect, useState } from "react";

interface HealthData {
  status: string;
  version: string;
  uptime: number;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export default function App() {
  const [health, setHealth] = useState<HealthData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    async function fetchHealth() {
      try {
        const res = await fetch("/api/health");
        const json: ApiResponse<HealthData> = await res.json();
        if (json.success && json.data) {
          setHealth(json.data);
        } else {
          setError(json.error || "Unknown error");
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to fetch");
      }
    }

    fetchHealth();
    const interval = setInterval(fetchHealth, 5000);
    return () => clearInterval(interval);
  }, []);

  return (
    <main className="container">
      <h1>Noctum</h1>
      <p className="subtitle">AI-powered sandboxed simulation testing</p>

      <div className="status-card">
        <h2>Server Status</h2>
        {error ? (
          <p className="error">Error: {error}</p>
        ) : health ? (
          <dl>
            <dt>Status</dt>
            <dd className="status-ok">{health.status}</dd>
            <dt>Version</dt>
            <dd>{health.version}</dd>
            <dt>Uptime</dt>
            <dd>{health.uptime}s</dd>
          </dl>
        ) : (
          <p>Loading...</p>
        )}
      </div>
    </main>
  );
}
