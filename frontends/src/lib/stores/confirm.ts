/**
 * Promise-based confirmation dialog.
 *
 * `window.confirm` does NOT work in this app: Tauri v2 patches it into
 * `plugin:dialog|confirm`, and that command is rejected by the ACL
 * ("Command plugin:dialog|confirm not allowed by ACL" — observed on a real
 * build while testing the full-access switch). Every confirm in the UI was
 * therefore a silent no-op: the destructive action simply never ran, with the
 * only trace being an unhandled rejection in the log.
 *
 * This store backs a small in-app modal instead: no plugin dependency, works
 * identically in the desktop app and in a plain browser tab, and it can be
 * styled/translated like the rest of the UI.
 */
import { writable } from "svelte/store";

export interface ConfirmRequest {
  message: string;
  /** Label of the confirming (destructive) action. */
  confirmLabel?: string;
  /** Renders the confirm button in the danger colour. */
  danger?: boolean;
}

interface ConfirmState extends ConfirmRequest {
  open: boolean;
}

const initial: ConfirmState = { open: false, message: "" };

export const confirmState = writable<ConfirmState>(initial);

let resolver: ((ok: boolean) => void) | null = null;

/** Ask the user to confirm `message`; resolves false on cancel/Esc/backdrop. */
export function askConfirm(
  message: string,
  opts: { confirmLabel?: string; danger?: boolean } = {},
): Promise<boolean> {
  // A second request while one is open resolves the first as "cancelled"
  // rather than leaving its promise pending forever. The dialog re-samples the
  // element to restore focus on every open, so the second request restores
  // focus to ITS trigger (review C11).
  if (resolver) {
    resolver(false);
    resolver = null;
  }
  return new Promise<boolean>((resolve) => {
    resolver = resolve;
    confirmState.set({
      open: true,
      message,
      confirmLabel: opts.confirmLabel,
      danger: opts.danger ?? true,
    });
  });
}

export function resolveConfirm(ok: boolean) {
  const r = resolver;
  resolver = null;
  confirmState.set(initial);
  r?.(ok);
}
