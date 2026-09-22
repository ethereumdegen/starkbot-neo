import { isTauri } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorOf, type DictationStatus } from "../bridge/api";

export interface Dictation {
  status: DictationStatus;
  error: string | null;
  spectrum: number[];
  start: () => Promise<void>;
  stop: () => Promise<void>;
  cancel: () => Promise<void>;
}

/** Own one native recording; recognized text only edits the caller's draft. */
export function useDictation(onTranscript: (text: string) => void): Dictation {
  const [status, setStatus] = useState<DictationStatus>("idle");
  const [error, setError] = useState<string | null>(null);
  const [spectrum, setSpectrum] = useState<number[]>([]);
  const phase = useRef<DictationStatus>("idle");
  const mounted = useRef(false);
  const generation = useRef(0);
  const timer = useRef<number | null>(null);
  const cancelling = useRef<Promise<void> | null>(null);
  const transcript = useRef(onTranscript);

  useEffect(() => { transcript.current = onTranscript; }, [onTranscript]);

  const clearPoll = useCallback(() => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = null;
  }, []);

  const setPhase = useCallback((next: DictationStatus) => {
    phase.current = next;
    if (mounted.current) setStatus(next);
  }, []);

  const current = useCallback((epoch: number) =>
    mounted.current && epoch === generation.current, []);

  const fail = useCallback(async (failure: unknown, epoch: number) => {
    if (!current(epoch)) return;
    setError(errorOf(failure).message);
    setSpectrum([]);
    // Lost IPC can leave capture running even when a command rejected.
    // Do not claim Idle until the native microphone has been released.
    try {
      await api.voiceCancel();
      if (current(epoch)) setPhase("idle");
    } catch (cancelFailure) {
      if (current(epoch)) setError(`${errorOf(failure).message} ${errorOf(cancelFailure).message}`);
    }
  }, [current, setPhase]);

  const poll = useCallback((epoch: number) => {
    const tick = async () => {
      if (!current(epoch) || phase.current !== "listening") return;
      try {
        const [next, bins] = await api.voiceStatus();
        if (!current(epoch)) return;
        setPhase(next);
        setSpectrum(next === "listening" ? bins : []);
        if (next === "listening") timer.current = window.setTimeout(() => { void tick(); }, 50);
      } catch (failure) {
        await fail(failure, epoch);
      }
    };
    void tick();
  }, [current, fail, setPhase]);

  const start = useCallback(async () => {
    if (phase.current !== "idle" || cancelling.current) return;
    if (!isTauri()) {
      setError("Microphone dictation requires the desktop app. Open Starkbot Neo and add an OpenAI key in Connections.");
      return;
    }
    const epoch = ++generation.current;
    clearPoll();
    setError(null);
    setSpectrum([]);
    setPhase("starting");
    try {
      await api.voiceStart();
      if (!current(epoch)) return;
      setPhase("listening");
      poll(epoch);
    } catch (failure) {
      await fail(failure, epoch);
    }
  }, [clearPoll, current, fail, poll, setPhase]);

  const stop = useCallback(async () => {
    if (phase.current !== "listening" || cancelling.current) return;
    const epoch = ++generation.current;
    clearPoll();
    setSpectrum([]);
    setError(null);
    setPhase("transcribing");
    try {
      const text = await api.voiceStop();
      if (!current(epoch)) return;
      setPhase("idle");
      transcript.current(text);
    } catch (failure) {
      await fail(failure, epoch);
    }
  }, [clearPoll, current, fail, setPhase]);

  const cancel = useCallback((): Promise<void> => {
    if (cancelling.current) return cancelling.current;
    const epoch = ++generation.current;
    clearPoll();
    setSpectrum([]);
    setError(null);
    if (!isTauri() || phase.current === "idle") {
      setPhase("idle");
      return Promise.resolve();
    }
    const pending = (async () => {
      try {
        await api.voiceCancel();
        if (current(epoch)) setPhase("idle");
      } catch (failure) {
        if (current(epoch)) setError(errorOf(failure).message);
      } finally {
        cancelling.current = null;
      }
    })();
    cancelling.current = pending;
    return pending;
  }, [clearPoll, current, setPhase]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      ++generation.current;
      clearPoll();
      if (isTauri() && phase.current !== "idle") void api.voiceCancel().catch(() => {});
    };
  }, [clearPoll]);

  return { status, error, spectrum, start, stop, cancel };
}
