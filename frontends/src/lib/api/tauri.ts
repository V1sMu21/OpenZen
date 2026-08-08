/** Detect if we're running inside Tauri */
export function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as any).__TAURI_INTERNALS__ !== undefined
  );
}

/** Invoke a Tauri command */
export async function tauriInvoke(
  cmd: string,
  args: Record<string, unknown> = {},
): Promise<any> {
  const w = window as any;
  if (w.__TAURI_INTERNALS__) {
    return w.__TAURI_INTERNALS__.invoke(cmd, args);
  }
  if (w.__TAURI?.invoke) {
    return w.__TAURI__.invoke(cmd, args);
  }
  throw new Error("Tauri not available");
}
