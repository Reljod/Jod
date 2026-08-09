import { useState } from "react";

export interface AuthGateProps {
  reason: string;
  onSubmit(token: string): void | Promise<void>;
}

/**
 * Where a bearer token becomes a session cookie.
 *
 * The token is typed once and **never stored by this app**. `POST /v1/session`
 * exchanges it for an `HttpOnly; Secure; SameSite=Strict` cookie, and from then
 * on the cookie is the credential. Keeping the token in `localStorage` would
 * hand a long-lived write credential to anything that ever runs on this page,
 * and on a phone that credential can start processes on the box.
 *
 * The field is `type="password"` for the shoulder-surfing case, with
 * autocomplete off — a password manager offering to save this would be saving
 * the wrong thing.
 */
export function AuthGate({ reason, onSubmit }: AuthGateProps) {
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit() {
    if (token.trim() === "" || busy) return;
    setBusy(true);
    try {
      await onSubmit(token);
    } finally {
      setBusy(false);
      // Drop it either way: on success the cookie has taken over, and on
      // failure a wrong token is not worth keeping on screen.
      setToken("");
    }
  }

  return (
    <div className="gate">
      <h1>JOD</h1>
      <p>{reason}</p>

      <input
        type="password"
        value={token}
        placeholder="Bearer token"
        autoCapitalize="none"
        autoCorrect="off"
        autoComplete="off"
        spellCheck={false}
        inputMode="text"
        onChange={(e) => setToken(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void submit();
        }}
      />

      <button onClick={() => void submit()} disabled={busy || token.trim() === ""}>
        {busy ? "CONNECTING…" : "CONNECT"}
      </button>

      <p className="why">
        The token is exchanged for a session cookie and is not stored on this
        device. Issue one on the box with a <code>write</code> scope to delegate,
        or <code>read</code> to watch only.
      </p>
    </div>
  );
}
