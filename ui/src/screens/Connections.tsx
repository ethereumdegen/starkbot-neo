import { useCallback, useEffect, useRef, useState } from "react";

import {
  api,
  errorOf,
  onLoginDone,
  onLoginFailed,
  type BootstrapView,
  type Fix,
} from "../bridge/api";
import { DoctorList } from "../components/DoctorList";
import { KeyCard } from "../components/KeyCard";
import { Pill } from "../components/Pill";
import { ProviderCard, type LoginState } from "../components/ProviderCard";
import { RuntimePicker } from "../components/RuntimePicker";

interface Banner {
  tone: "ok" | "warn" | "fail";
  text: string;
}

export function Connections() {
  const [boot, setBoot] = useState<BootstrapView | null>(null);
  const [logins, setLogins] = useState<Record<string, LoginState>>({});
  const [models, setModels] = useState<Record<string, number>>({});
  const [banner, setBanner] = useState<Banner | null>(null);
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const keyFields = useRef<Record<string, HTMLInputElement | null>>({});
  const runtimeSection = useRef<HTMLElement | null>(null);

  /**
   * Verify the subscriptions against the Keychain and fill in the catalogue.
   * Deliberately off the busy path: on an unsigned development build macOS
   * asks the user whether this binary may read the credential, and the
   * screen has to stay usable while that sheet is up.
   */
  const verify = useCallback(async (painted: BootstrapView) => {
    try {
      const [connections, openai, anthropic] = await Promise.all([
        api.connections(),
        api.listModels("openai"),
        api.listModels("anthropic"),
      ]);
      const changed = connections.some(
        (row, index) => row.status !== painted.connections[index]?.status,
      );
      // A verification that moved a row also moved the store, so the rest of
      // the screen (runtime options, doctor) needs re-reading with it.
      setBoot(changed ? await api.getBootstrap() : { ...painted, connections });
      setModels({ openai: openai.length, anthropic: anthropic.length });
    } catch (thrown) {
      setBanner({ tone: "warn", text: errorOf(thrown).message });
    }
  }, []);

  /**
   * Paint from the store: `get_bootstrap` touches no credential, so the
   * window is up immediately. Verification follows on its own.
   */
  const refresh = useCallback(async () => {
    const painted = await api.getBootstrap();
    setBoot(painted);
    void verify(painted);
  }, [verify]);

  /** One place where a command's failure becomes something the user can read. */
  const run = useCallback(
    async (task: () => Promise<void>) => {
      setBusy(true);
      try {
        await task();
      } catch (thrown) {
        const error = errorOf(thrown);
        setBanner({ tone: "fail", text: error.message });
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  useEffect(() => {
    void run(refresh);
  }, [refresh, run]);

  useEffect(() => {
    const done = onLoginDone((row) => {
      setLogins((current) => {
        const next = { ...current };
        delete next[row.provider];
        return next;
      });
      setBanner({ tone: "ok", text: `Signed in to ${row.display_name}.` });
      void run(refresh);
    });
    const failed = onLoginFailed((failure) => {
      setLogins((current) => {
        const login = current[failure.provider];
        if (login === undefined) {
          return current;
        }
        return {
          ...current,
          [failure.provider]: {
            ...login,
            error: `${failure.error.message} — paste the redirect URL to finish.`,
          },
        };
      });
    });
    return () => {
      void done.then((off) => off());
      void failed.then((off) => off());
    };
  }, [refresh, run]);

  useEffect(() => {
    if (Object.keys(logins).length === 0) {
      return;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [logins]);

  const focusKey = useCallback((account: string) => {
    const field = keyFields.current[account];
    field?.scrollIntoView({ block: "center" });
    field?.focus();
  }, []);

  const beginLogin = useCallback(
    (provider: string) =>
      run(async () => {
        const start = await api.beginLogin(provider);
        setNow(Date.now());
        setLogins((current) => ({
          ...current,
          [provider]: {
            start,
            deadline: Date.now() + start.timeout_secs * 1000,
            pasted: "",
            working: false,
          },
        }));
        // The loopback listener is up as soon as `begin_login` returns, so the
        // vendor page can be opened straight away.
        await api.openLoginPage(provider);
      }),
    [run],
  );

  const applyFix = useCallback(
    (fix: Fix) => {
      if (fix.kind === "set_key") {
        focusKey(fix.account);
      } else if (fix.kind === "sign_in") {
        void beginLogin(fix.provider);
      } else if (fix.kind === "choose_runtime") {
        runtimeSection.current?.scrollIntoView({ block: "start", behavior: "smooth" });
      }
    },
    [beginLogin, focusKey],
  );

  if (boot === null) {
    return (
      <div className="app">
        <header className="app-header">
          <h1>Starkbot Neo</h1>
          <span className="sub">opening the store…</span>
        </header>
      </div>
    );
  }

  const ready = boot.inference.ready;

  return (
    <div className="app">
      <header className="app-header">
        <h1>Connections</h1>
        <span className="sub">
          Sign in to a subscription, or add a key. Everything on this screen stays in Rust.
        </span>
      </header>

      <div className="app-body">
        {banner !== null && (
          <div className={`banner ${banner.tone}`} role="status">
            <span>{banner.text}</span>
            <button className="link" onClick={() => setBanner(null)}>
              Dismiss
            </button>
          </div>
        )}

        <div className="readiness">
          <Pill tone={ready ? "ok" : "fail"} label={ready ? "inference: ok" : "inference: fail"} />
          <div>
            <div className="headline">
              {ready
                ? `Running on ${boot.inference.provider} · ${boot.inference.model}`
                : "No usable inference connection yet"}
            </div>
            <div className="meta">
              {ready
                ? "Starkbot can reach a model. A TypeSafe key is still what the navigator needs."
                : "Sign in to Claude Pro/Max or ChatGPT Plus/Pro below, or add an API key, then pick the runtime."}
            </div>
          </div>
          <button
            style={{ marginLeft: "auto" }}
            disabled={busy}
            onClick={() => void run(refresh)}
          >
            Refresh
          </button>
        </div>

        <section>
          <h2>Subscriptions</h2>
          <div className="cards">
            {boot.connections.map((row) => (
              <ProviderCard
                key={row.provider}
                row={row}
                login={logins[row.provider]}
                secondsLeft={Math.round(((logins[row.provider]?.deadline ?? now) - now) / 1000)}
                busy={busy}
                onSignIn={() => void beginLogin(row.provider)}
                onSignOut={() =>
                  void run(async () => {
                    await api.disconnect(row.provider);
                    setBanner({ tone: "ok", text: `Signed out of ${row.display_name}.` });
                    await refresh();
                  })
                }
                onOpenPage={() => void run(() => api.openLoginPage(row.provider).then(() => undefined))}
                onPastedChange={(value) =>
                  setLogins((current) => {
                    const login = current[row.provider];
                    if (login === undefined) {
                      return current;
                    }
                    return { ...current, [row.provider]: { ...login, pasted: value } };
                  })
                }
                onFinishPaste={() =>
                  void run(async () => {
                    const login = logins[row.provider];
                    if (login === undefined) {
                      return;
                    }
                    await api.finishLoginPasted(row.provider, login.pasted);
                  })
                }
                onCancel={() =>
                  void run(async () => {
                    await api.cancelLogin(row.provider);
                    setLogins((current) => {
                      const next = { ...current };
                      delete next[row.provider];
                      return next;
                    });
                  })
                }
                onUseForInference={() =>
                  void run(async () => {
                    await api.setInferenceRuntime(row.provider);
                    await refresh();
                  })
                }
              />
            ))}
          </div>
        </section>

        <section ref={runtimeSection}>
          <h2>Inference runtime</h2>
          <RuntimePicker
            inference={boot.inference}
            models={boot.models}
            busy={busy}
            onSelect={(provider) =>
              void run(async () => {
                await api.setInferenceRuntime(provider);
                await refresh();
              })
            }
            onModelSelect={(model) =>
              void run(async () => {
                await api.setInferenceRuntime(boot.inference.provider, model);
                await refresh();
              })
            }
          />
        </section>

        <section>
          <h2>Keys</h2>
          <div className="keys">
            {boot.keys.map((row) => (
              <KeyCard
                key={row.account}
                row={row}
                busy={busy}
                modelCount={
                  row.refreshable && row.state !== "missing" ? models[row.account] : undefined
                }
                focusRef={(element) => {
                  keyFields.current[row.account] = element;
                }}
                onSave={(value) =>
                  run(async () => {
                    const saved = await api.setKey(row.account, value);
                    setBanner({ tone: "ok", text: `${saved.label}: ${saved.state}.` });
                    await refresh();
                  })
                }
                onCheck={() =>
                  void run(async () => {
                    const checked = await api.checkKey(row.account);
                    setBanner({ tone: "ok", text: `${checked.label}: ${checked.state}.` });
                    await refresh();
                  })
                }
                onRemove={() =>
                  void run(async () => {
                    await api.removeKey(row.account);
                    await refresh();
                  })
                }
                onRefreshModels={() =>
                  void run(async () => {
                    const refreshed = await api.refreshModels(row.account);
                    setBanner({
                      tone: "ok",
                      text: `${row.label}: ${refreshed.length} models cached.`,
                    });
                    await refresh();
                  })
                }
              />
            ))}
          </div>
        </section>

        <section>
          <h2>Doctor</h2>
          <DoctorList checks={boot.doctor} busy={busy} onFix={applyFix} />
        </section>
      </div>

      <footer className="app-footer">
        <span className="mono">bridge v{boot.bridge_version}</span>
        <span className="mono">{boot.store_path}</span>
      </footer>
    </div>
  );
}
