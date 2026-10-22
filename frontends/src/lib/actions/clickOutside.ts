/**
 * `use:clickOutside` — call `onOutside(event)` when a pointer press lands
 * outside the element (and outside any node listed in `except`).
 *
 * The composer's two popovers (model switcher pill, thinking-level pill) had
 * no way to dismiss them except re-clicking the pill or pressing Escape, and
 * the rest of the app's popovers rely on a full-screen backdrop instead. A
 * backdrop steals the click from whatever the user actually aimed at, so the
 * popovers get this small action rather than another overlay.
 *
 * Listens on `pointerdown` (not `click`): a press that starts outside should
 * close the popover even if the pointer is released somewhere else, and it
 * must run before the target's own click handler.
 *
 * ```svelte
 * <div use:clickOutside={{ handler: () => (open = false), except: [btn] }}>
 * ```
 */
export interface ClickOutsideOptions {
  handler: (event: PointerEvent) => void;
  /** Nodes that must NOT count as "outside" (usually the trigger button, so
   *  its own click can toggle the popover instead of close-then-reopen). */
  except?: Array<HTMLElement | undefined | null>;
  /** Set false to suspend the listener without removing the action. */
  enabled?: boolean;
}

export function clickOutside(
  node: HTMLElement,
  options: ClickOutsideOptions | ((event: PointerEvent) => void),
) {
  let opts: ClickOutsideOptions =
    typeof options === "function" ? { handler: options } : options;

  function onPointerDown(event: PointerEvent) {
    if (opts.enabled === false) return;
    const target = event.target as Node | null;
    if (!target) return;
    if (node.contains(target)) return;
    for (const el of opts.except ?? []) {
      if (el && el.contains(target)) return;
    }
    opts.handler(event);
  }

  document.addEventListener("pointerdown", onPointerDown, true);

  return {
    update(next: ClickOutsideOptions | ((event: PointerEvent) => void)) {
      opts = typeof next === "function" ? { handler: next } : next;
    },
    destroy() {
      document.removeEventListener("pointerdown", onPointerDown, true);
    },
  };
}
