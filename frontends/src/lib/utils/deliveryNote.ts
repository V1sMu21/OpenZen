/**
 * Split the backend-appended delivery block out of a final reply.
 *
 * The agent loop appends up to two system blocks to `full_response`:
 *
 *   > ⚠️ 独立评审未通过（…）
 *
 *   ---
 *   > **📋 交付说明**
 *   > 验证：…
 *   > 遗留：…
 *
 * They are delivery metadata, not part of the answer, and they used to make
 * the finished bubble read like the agent was talking to itself. The raw
 * message keeps the text (it is still the only place an export/CLI sees the
 * state), but the bubble renders the answer alone and moves these lines
 * behind the footer's "详情" toggle.
 *
 * Detection is deliberately strict: the block must sit at the very end, must
 * start with the exact contract header, and must contain a verification
 * line. A model quoting the same header mid-answer is left untouched.
 */

export interface DeliveryNote {
  /** The `> ⚠️ …` line that precedes the contract, when present. */
  warning?: string;
  /** Legacy `完成：…` line. Newer backends no longer emit it (it duplicated
   *  the reply's own opening), but old transcripts still carry it. */
  done?: string;
  verified?: string;
  left?: string;
}

export interface DeliverySplit {
  /** The reply with the metadata block removed. */
  body: string;
  /** Parsed metadata, or null when the message has no such block. */
  note: DeliveryNote | null;
}

const CONTRACT_HEADERS = [
  "\n\n---\n> **📋 交付说明**",
  "\n\n---\n> **📋 Delivery notes**",
];

const WARNING_TAIL = /\n\n> (?:⚠️|⚠) ([^\n]+)\s*$/;

export function splitDeliveryNote(
  content: string,
  _qualityNote?: string | null,
): DeliverySplit {
  if (!content) return { body: content, note: null };
  const text = content.replace(/\r\n/g, "\n");

  let headIdx = -1;
  for (const header of CONTRACT_HEADERS) {
    const idx = text.lastIndexOf(header);
    if (idx > headIdx) headIdx = idx;
  }
  if (headIdx < 0) return { body: text, note: null };

  // A contract-shaped block inside an unterminated code fence is quoted
  // content, not metadata. Leave it alone.
  const fences = (text.slice(0, headIdx).match(/^```/gm) ?? []).length;
  if (fences % 2 === 1) return { body: text, note: null };

  const tail = text.slice(headIdx);
  const note = parseContract(tail);
  if (!note) return { body: text, note: null };

  let body = text.slice(0, headIdx);
  const warning = body.match(WARNING_TAIL);
  if (warning) {
    note.warning = warning[1].trim();
    body = body.slice(0, warning.index);
  }
  return { body: body.trimEnd(), note };
}

function parseContract(tail: string): DeliveryNote | null {
  const note: DeliveryNote = {};
  let field: keyof DeliveryNote | null = null;
  let sawVerification = false;

  const append = (line: string) => {
    if (!field) return;
    const value = line.startsWith("> ")
      ? line.slice(2)
      : line === ">"
        ? ""
        : line;
    note[field] = note[field] ? `${note[field]}\n${value}` : value;
  };

  for (const raw of tail.split("\n")) {
    const line = raw.replace(/\s+$/, "");
    if (/^>\s*\*\*📋/.test(line)) continue;

    const done = line.match(/^>\s*(?:完成|Done)[：:]\s?(.*)$/);
    if (done) {
      field = "done";
      note.done = done[1];
      continue;
    }
    const verified = line.match(/^>\s*(?:验证|Verified)[：:]\s?(.*)$/);
    if (verified) {
      field = "verified";
      note.verified = verified[1];
      sawVerification = true;
      continue;
    }
    const left = line.match(/^>\s*(?:遗留|Left open)[：:]\s?(.*)$/);
    if (left) {
      field = "left";
      note.left = left[1];
      continue;
    }
    append(line);
  }

  if (!sawVerification) return null;

  for (const key of ["warning", "done", "verified", "left"] as const) {
    const value = note[key];
    if (value != null) {
      const trimmed = value.trim();
      if (trimmed) note[key] = trimmed;
      else delete note[key];
    }
  }
  return note;
}
