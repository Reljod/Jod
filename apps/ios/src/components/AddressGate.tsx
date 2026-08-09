import { useState } from "react";

export interface AddressGateProps {
  reason: string;
  onSubmit(address: string): void | Promise<void>;
}

/**
 * Where the daemon's address is entered.
 *
 * Only the packaged app ever shows this. Served from a browser, the daemon is
 * the origin the page came from and there is nothing to ask; served from
 * `tauri://localhost`, "same origin" is the app bundle, which contains no API —
 * so the address has to come from somewhere, and guessing it would be worse
 * than asking.
 *
 * The address is not a credential. It is a tailnet hostname, it grants nothing
 * on its own, and the token gate still stands behind it — which is why this is
 * remembered across launches and the token never is.
 */
export function AddressGate({ reason, onSubmit }: AddressGateProps) {
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit() {
    if (address.trim() === "" || busy) return;
    setBusy(true);
    try {
      await onSubmit(address);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="gate">
      <h1>JOD</h1>
      <p>{reason}</p>

      <input
        type="url"
        value={address}
        placeholder="jod-cloud:8787"
        autoCapitalize="none"
        autoCorrect="off"
        autoComplete="off"
        spellCheck={false}
        inputMode="url"
        onChange={(e) => setAddress(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void submit();
        }}
      />

      <button onClick={() => void submit()} disabled={busy || address.trim() === ""}>
        {busy ? "CONNECTING…" : "CONNECT"}
      </button>

      <p className="why">
        Where <code>jod-api</code> is listening — a tailnet name or an address,
        with a port if it is not 80 or 443. Plain <code>http://</code> is assumed
        unless you say otherwise, because the daemon binds loopback and is
        reached over the tailnet.
      </p>
    </div>
  );
}
