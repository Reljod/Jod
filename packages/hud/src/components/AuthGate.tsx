import { useState } from "react";

interface Props {
  reason: string;
  onSubmit(token: string): Promise<void>;
}

/**
 * Shown when the orchestrator is reachable but this browser has no session.
 *
 * The token is posted straight to `POST /v1/session` and never kept — the
 * `HttpOnly` cookie the server sets is the credential from then on, which is
 * also the only way `EventSource` can authenticate at all, since it cannot set
 * an `Authorization` header.
 */
export function AuthGate({ reason, onSubmit }: Props) {
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    if (!token.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(token.trim());
      setToken("");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="authgate-backdrop">
      <div className="authgate">
        <h2>◈ AUTHENTICATION REQUIRED</h2>
        <p className="why">{reason}</p>
        <label>
          BEARER TOKEN
          <input
            type="password"
            autoFocus
            value={token}
            placeholder="jod_…"
            onChange={(e) => setToken(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void submit()}
          />
        </label>
        <p className="note">
          Exchanged for an <code>HttpOnly</code> session cookie. The token itself is
          never stored by this page.
        </p>
        {error && <p className="err">{error}</p>}
        <button className="go" disabled={!token.trim() || busy} onClick={() => void submit()}>
          {busy ? "AUTHENTICATING…" : "OPEN SESSION"}
        </button>
      </div>
    </div>
  );
}
