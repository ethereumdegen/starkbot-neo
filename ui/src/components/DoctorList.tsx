import type { CheckView, Fix, Health } from "../bridge/api";
import { Pill, type Tone } from "./Pill";

const HEALTH: Record<Health, { tone: Tone; label: string }> = {
  ok: { tone: "ok", label: "OK" },
  warn: { tone: "warn", label: "Warn" },
  fail: { tone: "fail", label: "Fail" },
  unknown: { tone: "unknown", label: "Unknown" },
};

/**
 * The readiness report, with the control that fixes each row. The core's own
 * `fix` strings are `neo` commands; this window never shows one — it shows
 * the button instead.
 */
export function DoctorList({
  checks,
  busy,
  onFix,
}: {
  checks: CheckView[];
  busy: boolean;
  onFix: (fix: Fix) => void;
}) {
  return (
    <div className="checks">
      {checks.map((check) => {
        const health = HEALTH[check.health];
        return (
          <div className="check" key={check.name}>
            <div>
              <Pill tone={health.tone} label={health.label} />
            </div>
            <div>
              <div>{check.name}</div>
              <div className="meta">{check.detail}</div>
            </div>
            <div className="actions">{fixControl(check.fix, busy, onFix)}</div>
          </div>
        );
      })}
    </div>
  );
}

function fixControl(fix: Fix | undefined, busy: boolean, onFix: (fix: Fix) => void) {
  if (fix === undefined) {
    return null;
  }
  if (fix.kind === "manual") {
    return <span className="meta">{fix.detail}</span>;
  }
  const label =
    fix.kind === "set_key"
      ? `Add the ${fix.account} key`
      : fix.kind === "sign_in"
        ? "Sign in"
        : "Choose a runtime";
  return (
    <button disabled={busy} onClick={() => onFix(fix)}>
      {label}
    </button>
  );
}
