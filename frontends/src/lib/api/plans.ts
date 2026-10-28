// Plan review API — thin typed wrappers over the Rust commands in
// src-tauri/src/plans/mod.rs.
//
// A plan is a dated artifact on disk: `<working_dir>/.openzen/plans/<id>.json`
// plus `<id>.decision.json`. Everything here is IPC, no HTTP.

import { tauriInvoke } from "./tauri";

export interface PlanOption {
  label: string;
  detail: string;
}

export interface PlanDecision {
  decision: "approved" | "rejected";
  option: string;
  comment: string;
  decided_at: string;
}

export interface Plan {
  id: string;
  title: string;
  summary: string;
  markdown: string;
  options: PlanOption[];
  est_cost: string;
  status: "pending" | "approved" | "rejected";
  created_at: string;
  decision?: PlanDecision | null;
  /** Absolute path of `<id>.json`; used as the side-panel artifact path. */
  path: string;
}

export function listPlans(): Promise<Plan[]> {
  return tauriInvoke("plan_list");
}

export function getPlan(id: string): Promise<Plan> {
  return tauriInvoke("plan_get", { id });
}

export function submitPlan(input: {
  title: string;
  markdown: string;
  options?: PlanOption[];
  est_cost?: string;
  summary?: string;
}): Promise<Plan> {
  return tauriInvoke("plan_submit", {
    title: input.title,
    markdown: input.markdown,
    options: input.options ?? [],
    estCost: input.est_cost ?? "",
    summary: input.summary ?? "",
  });
}

export function decidePlan(
  id: string,
  decision: "approved" | "rejected",
  option = "",
  comment = "",
): Promise<Plan> {
  return tauriInvoke("plan_decide", { id, decision, option, comment });
}

/** Import plans an out-of-process agent dropped into the inbox. */
export function scanPlanInbox(): Promise<number> {
  return tauriInvoke("plan_scan_inbox");
}

/** Focus the side-panel tab for an existing plan. */
export function openPlan(id: string): Promise<Plan> {
  return tauriInvoke("plan_open", { id });
}
