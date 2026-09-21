// State is never colour alone (04 §2): every pill carries its word too.
export type Tone = "ok" | "warn" | "fail" | "unknown" | "busy";

export function Pill({ tone, label }: { tone: Tone; label: string }) {
  return (
    <span className={`pill ${tone}`}>
      <span className="dot" aria-hidden="true" />
      {label}
    </span>
  );
}
