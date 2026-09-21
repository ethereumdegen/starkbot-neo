import type { ConnectionRow, LoginStart, ProviderAccountStatus } from "../bridge/api";
import { Pill, type Tone } from "./Pill";

export interface LoginState {
  start: LoginStart;
  /** Wall-clock ms at which the loopback wait gives up. */
  deadline: number;
  pasted: string;
  /** Why the loopback wait ended, when it ended badly. */
  error?: string;
  working: boolean;
}

const STATUS: Record<ProviderAccountStatus, { tone: Tone; label: string }> = {
  connected: { tone: "ok", label: "Connected" },
  signed_out: { tone: "unknown", label: "Signed out" },
  rate_limited: { tone: "warn", label: "Rate limited" },
  unavailable: { tone: "fail", label: "Unavailable" },
};

export function ProviderCard({
  row,
  login,
  secondsLeft,
  busy,
  onSignIn,
  onSignOut,
  onOpenPage,
  onPastedChange,
  onFinishPaste,
  onCancel,
  onUseForInference,
}: {
  row: ConnectionRow;
  login?: LoginState;
  secondsLeft: number;
  busy: boolean;
  onSignIn: () => void;
  onSignOut: () => void;
  onOpenPage: () => void;
  onPastedChange: (value: string) => void;
  onFinishPaste: () => void;
  onCancel: () => void;
  onUseForInference: () => void;
}) {
  const status = STATUS[row.status];
  const connected = row.status === "connected";
  const waiting = login !== undefined && login.error === undefined;
  const minutes = Math.floor(Math.max(secondsLeft, 0) / 60);
  const seconds = Math.max(secondsLeft, 0) % 60;

  return (
    <article className="card">
      <div className="row">
        <h3>{row.display_name}</h3>
        <Pill tone={status.tone} label={status.label} />
      </div>
      <div className="meta mono">{row.provider}</div>
      {connected && (
        <div className="meta">
          {row.email ?? "no email reported"}
          {row.plan ? ` · ${row.plan}` : ""}
          {row.selected ? " · used for inference" : ""}
        </div>
      )}

      {login === undefined ? (
        <div className="actions">
          {connected ? (
            <button className="danger" onClick={onSignOut} disabled={busy}>
              Sign out
            </button>
          ) : (
            <button className="primary" onClick={onSignIn} disabled={busy}>
              Sign in
            </button>
          )}
          {connected && !row.selected && (
            <button onClick={onUseForInference} disabled={busy}>
              Use for inference
            </button>
          )}
        </div>
      ) : (
        <div className="login">
          <div className="meta">
            {waiting ? (
              <span className="busy">
                Waiting for your browser… {minutes}:{String(seconds).padStart(2, "0")}
              </span>
            ) : (
              <span className="fail">{login.error}</span>
            )}
          </div>
          <div className="url mono" title="The authorize URL — public values only">
            {login.start.authorize_url}
          </div>
          <div className="actions">
            <button className="primary" onClick={onOpenPage} disabled={login.working}>
              Open in browser
            </button>
            <button onClick={onCancel} disabled={login.working}>
              Cancel
            </button>
            <span className="meta mono">redirect {login.start.redirect_uri}</span>
          </div>
          <label className="meta" htmlFor={`paste-${row.provider}`}>
            Paste the redirect URL instead
          </label>
          <div className="paste">
            <input
              id={`paste-${row.provider}`}
              value={login.pasted}
              placeholder={`${login.start.redirect_uri}?code=…`}
              spellCheck={false}
              onChange={(event) => onPastedChange(event.target.value)}
            />
            <button onClick={onFinishPaste} disabled={login.working || login.pasted.trim() === ""}>
              Finish
            </button>
          </div>
        </div>
      )}
    </article>
  );
}
