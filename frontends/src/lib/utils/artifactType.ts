// File extension → side-panel renderer type.
//
// Mirrors the `detectType` map that used to live inline in SidePanel.svelte
// (and, on the Rust side, `detect_artifact_type` in
// src-tauri/src/sidepanel/commands.rs). Shared so a card can ask the panel to
// open a file with the same viewer the tab bar would pick.

import { basename } from "./paths";

const EXT_TYPE: Record<string, string> = {
  html: "html", htm: "html",
  pdf: "pdf",
  xlsx: "spreadsheet", xls: "spreadsheet", csv: "spreadsheet", tsv: "spreadsheet",
  py: "code", rs: "code", ts: "code", js: "code", go: "code",
  svelte: "code", json: "code", yaml: "code", yml: "code", toml: "code",
  sql: "code", sh: "code", css: "code", scss: "code", txt: "code",
  md: "markdown", rtf: "markdown",
  tex: "latex", lt: "latex", sty: "code", cls: "code", bib: "code",
  png: "image", jpg: "image", jpeg: "image", gif: "image", svg: "image", webp: "image",
  doc: "office", docx: "office", ppt: "office", pptx: "office",
};

/** Renderer type for `path`; unknown/extension-less files render as code.
 *
 *  The extension is taken from the LAST path segment only, so a directory
 *  containing a dot (`/a.b/file`) cannot masquerade as an extension, and a
 *  dotfile (`.gitignore`) — whose dot is at index 0 — has no extension. */
export function detectArtifactType(path: string): string {
  const name = basename(path);
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot === name.length - 1) return "code";
  return EXT_TYPE[name.slice(dot + 1).toLowerCase()] ?? "code";
}
