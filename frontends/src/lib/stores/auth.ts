import { writable } from "svelte/store";

export const showAuthDialog = writable(false);

let resolveFn: ((token: string) => void) | null = null;

export function requestAuthToken(): Promise<string> {
  showAuthDialog.set(true);
  return new Promise((resolve) => {
    resolveFn = resolve;
  });
}

export function submitAuthToken(token: string) {
  showAuthDialog.set(false);
  resolveFn?.(token);
  resolveFn = null;
}
