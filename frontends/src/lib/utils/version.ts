/** Compare release tags (`v0.3.0`) with app versions (`0.2.6`). */
export function parseVersion(value: string): number[] {
  const parts = value
    .trim()
    .replace(/^v/i, "")
    .split(/[.+-]/)
    .slice(0, 3)
    .map((part) => {
      const n = Number.parseInt(part, 10);
      return Number.isFinite(n) ? n : 0;
    });
  while (parts.length < 3) parts.push(0);
  return parts;
}

/** True when `candidate` is a newer semantic version than `current`. */
export function isNewerVersion(candidate: string, current: string): boolean {
  const a = parseVersion(candidate);
  const b = parseVersion(current);
  for (let i = 0; i < 3; i += 1) {
    const left = a[i] ?? 0;
    const right = b[i] ?? 0;
    if (left !== right) return left > right;
  }
  return false;
}
