import { isTauri } from "@tauri-apps/api/core";
import { LogicalSize, PhysicalPosition, type PhysicalSize } from "@tauri-apps/api/dpi";
import { currentMonitor, getCurrentWindow, type Window } from "@tauri-apps/api/window";

import tauriConfig from "../../../src-tauri/tauri.conf.json";
import { api } from "./api";

const MINI_MIN = new LogicalSize(420, 180);
const FULL_CONFIG = tauriConfig.app.windows.find((window) => window.label === "main")!;
// Tauri exposes no minimum-size getter. This bridge is the sole owner of the
// runtime constraint; derive its full-mode value from the actual window config.
const FULL_MIN = new LogicalSize(FULL_CONFIG.minWidth, FULL_CONFIG.minHeight);

type FullState = {
  position: PhysicalPosition;
  size: PhysicalSize;
  decorated: boolean;
  resizable: boolean;
  topmost: boolean;
  maximized: boolean;
  minimized: boolean;
  fullscreen: boolean;
  compositor: boolean;
  normalBoundsCaptured: boolean;
};

let fullState: FullState | null = null;
let miniActive = false;
let transitions: Promise<void> = Promise.resolve();

function message(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error) {
    return String(error.message);
  }
  return String(error);
}

async function normalize(window: Window): Promise<void> {
  await window.unminimize();
  await window.setFullscreen(false);
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const [minimized, maximized, fullscreen] = await Promise.all([
      window.isMinimized(), window.isMaximized(), window.isFullscreen(),
    ]);
    if (!minimized && !fullscreen) {
      if (!maximized) return;
      // AppKit cannot reliably unzoom until its fullscreen exit has finished.
      await window.unmaximize();
    }
    await new Promise<void>((resolve) => setTimeout(resolve, 32));
  }
  throw new Error("The window manager did not release the minimized/maximized window.");
}

async function normalBounds(window: Window): Promise<Pick<FullState, "position" | "size">> {
  let previous = { position: await window.outerPosition(), size: await window.innerSize() };
  // Unmaximizing is asynchronous on the desktop. Do not save the maximized
  // dimensions as the bounds to which a later unmaximize should return.
  for (let attempt = 0; attempt < 60; attempt += 1) {
    await new Promise<void>((resolve) => setTimeout(resolve, 32));
    const current = { position: await window.outerPosition(), size: await window.innerSize() };
    if (current.position.x === previous.position.x && current.position.y === previous.position.y
      && current.size.width === previous.size.width && current.size.height === previous.size.height) {
      return current;
    }
    previous = current;
  }
  throw new Error("The window's normal bounds did not settle after leaving maximized mode.");
}

async function capture(window: Window): Promise<FullState> {
  const [position, size, decorated, resizable, topmost, maximized, minimized, fullscreen] =
    await Promise.all([
      window.outerPosition(), window.innerSize(), window.isDecorated(), window.isResizable(),
      window.isAlwaysOnTop(), window.isMaximized(), window.isMinimized(), window.isFullscreen(),
    ]);
  const compositor = await api.windowCompositor("capture");
  return {
    position, size, decorated, resizable, topmost, maximized, minimized, fullscreen, compositor,
    normalBoundsCaptured: compositor || (!maximized && !fullscreen && !minimized),
  };
}

async function resize(window: Window, requested: LogicalSize | PhysicalSize): Promise<void> {
  await window.setSize(requested);
  const scale = await window.scaleFactor();
  const expected = requested.type === "Logical" ? requested.toPhysical(scale) : requested;
  for (let attempt = 0; attempt < 60; attempt += 1) {
    const size = await window.innerSize();
    if (Math.abs(size.width - expected.width) <= 1 && Math.abs(size.height - expected.height) <= 1) return;
    await new Promise<void>((resolve) => setTimeout(resolve, 32));
  }
  const observed = await window.innerSize();
  throw new Error(
    `The window manager did not allow the requested window size: expected ${expected.width}×${expected.height} physical pixels, observed ${observed.width}×${observed.height} (scale ${scale}).`,
  );
}

async function enterMini(window: Window, state: FullState): Promise<void> {
  // GTK can report a tiled Wayland window as maximized. The compositor branch
  // releases tiling/fullscreen itself instead of waiting on that GTK flag.
  if (state.compositor) await window.unminimize();
  else await normalize(window);
  await window.setMinSize(MINI_MIN);
  await window.setResizable(true);
  // Hyprland must release its tiled layout before GTK's resize request can work.
  // Its first call also captures normal bounds before we shrink the window.
  if (state.compositor) await api.windowCompositor("mini");
  await window.setDecorations(false);
  const monitor = await currentMonitor();
  if (!monitor) throw new Error("The current monitor is unavailable; mini mode was not opened.");
  const work = monitor.workArea;
  const width = Math.min(560, work.size.width / monitor.scaleFactor);
  const height = Math.min(220, work.size.height / monitor.scaleFactor);
  if (width < MINI_MIN.width || height < MINI_MIN.height) {
    throw new Error("The monitor work area is too small for mini mode (420 × 180 minimum).");
  }
  await resize(window, new LogicalSize(width, height));
  await window.setResizable(false);
  await window.setAlwaysOnTop(true);
  if (state.compositor) {
    // Compositor coordinates, scale and reserved panels are authoritative on
    // Wayland; GTK reports (0,0) there even after a successful compositor move.
    await api.windowCompositor("mini");
  } else {
    await window.setPosition(new PhysicalPosition(
      Math.round(work.position.x + (work.size.width - width * monitor.scaleFactor) / 2),
      Math.round(work.position.y + Math.max(0, work.size.height - (height + 24) * monitor.scaleFactor)),
    ));
  }
  await window.show();
  await window.setFocus();
}

async function restoreFull(window: Window, state: FullState): Promise<void> {
  const failures: string[] = [];
  // A failed operation must not prevent the remaining properties from being
  // restored. Keep the snapshot until every restoration operation succeeds.
  const restore = async (operation: () => Promise<unknown>) => {
    try { await operation(); } catch (error) { failures.push(message(error)); }
  };
  await restore(() => state.compositor ? window.unminimize() : normalize(window));
  await restore(() => window.setResizable(true));
  await restore(() => window.setMinSize(MINI_MIN));
  await restore(() => window.setDecorations(state.decorated));
  if (!state.compositor) {
    await restore(() => window.setPosition(state.position));
    await restore(() => resize(window, state.size));
  }
  await restore(() => window.setAlwaysOnTop(state.topmost));
  await restore(() => window.setMinSize(FULL_MIN));
  await restore(() => window.setResizable(state.resizable));
  if (state.compositor) await restore(() => api.windowCompositor("restore"));
  if (!state.compositor && state.maximized) await restore(() => window.maximize());
  if (!state.compositor && state.fullscreen) await restore(() => window.setFullscreen(true));
  await restore(() => window.show());
  if (state.minimized) await restore(() => window.minimize());
  else await restore(() => window.setFocus());
  if (!state.compositor) {
    await restore(async () => {
      for (let attempt = 0; attempt < 60; attempt += 1) {
        const [minimized, maximized, fullscreen] = await Promise.all([
          window.isMinimized(), window.isMaximized(), window.isFullscreen(),
        ]);
        if (minimized === state.minimized && maximized === state.maximized && fullscreen === state.fullscreen) return;
        await new Promise<void>((resolve) => setTimeout(resolve, 32));
      }
      throw new Error("The window manager did not finish restoring the full window state.");
    });
  }
  if (failures.length) throw new Error(failures.join("; "));
}

async function transition(mini: boolean): Promise<void> {
  if (!isTauri()) return;
  if (mini && miniActive) return;
  if (!mini && !fullState) return;
  const window = getCurrentWindow();
  if (mini) {
    if (!fullState) fullState = await capture(window);
    const state = fullState;
    try {
      if (!state.normalBoundsCaptured) {
        await normalize(window);
        Object.assign(state, await normalBounds(window));
        state.normalBoundsCaptured = true;
      }
      await enterMini(window, state);
      miniActive = true;
    } catch (error) {
      try {
        await restoreFull(window, state);
        await api.windowCompositor("clear");
        fullState = null;
        miniActive = false;
      } catch (rollback) {
        throw new Error(`Could not enter mini mode: ${message(error)}. Restoring the full window also failed: ${message(rollback)}`);
      }
      throw new Error(`Could not enter mini mode: ${message(error)}`);
    }
  } else {
    const state = fullState!;
    try {
      await restoreFull(window, state);
      await api.windowCompositor("clear");
      fullState = null;
      miniActive = false;
    } catch (error) {
      try {
        await enterMini(window, state);
        miniActive = true;
      } catch (rollback) {
        throw new Error(`Could not restore the full window: ${message(error)}. Restoring mini mode also failed: ${message(rollback)}`);
      }
      throw new Error(`Could not restore the full window: ${message(error)}`);
    }
  }
}

/** Serialize even failed/rapid requests so the original full bounds are never overwritten. */
export function setMiniMode(mini: boolean): Promise<void> {
  const next = transitions.then(() => transition(mini));
  transitions = next.catch(() => {});
  return next;
}

/** Call directly from pointer-down; a compositor may require the input gesture's serial. */
export async function startWindowDrag(): Promise<void> {
  if (!isTauri()) return;
  await getCurrentWindow().startDragging();
}
