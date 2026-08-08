// scripts/verify_diffwords.mjs
// Verify EditCard.svelte diffWords() — word-level inline diff.
// Run: node scripts/verify_diffwords.mjs
// Must be kept in sync with the function in EditCard.svelte.

/**
 * diffWords(oldStr, newStr) → WordToken[]
 * Mirrors EditCard.svelte implementation.
 */
function diffWords(oldStr, newStr) {
  const oldTokens = oldStr.length > 0 ? oldStr.split(/(\s+)/) : [];
  const newTokens = newStr.length > 0 ? newStr.split(/(\s+)/) : [];

  const m = oldTokens.length;
  const n = newTokens.length;
  if (m === 0 && n === 0) return [];

  const dp = Array.from({ length: m + 1 }, () => Array(n + 1).fill(0));
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (oldTokens[i - 1] === newTokens[j - 1]) {
        dp[i][j] = dp[i - 1][j - 1] + 1;
      } else {
        dp[i][j] = Math.max(dp[i - 1][j], dp[i][j - 1]);
      }
    }
  }

  const buf = [];
  let i = m;
  let j = n;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldTokens[i - 1] === newTokens[j - 1]) {
      buf.push({ text: oldTokens[i - 1], type: "context" });
      i--; j--;
    } else if (j > 0 && (i === 0 || dp[i][j - 1] >= dp[i - 1][j])) {
      buf.push({ text: newTokens[j - 1], type: "added" });
      j--;
    } else {
      buf.push({ text: oldTokens[i - 1], type: "removed" });
      i--;
    }
  }

  const tokens = [];
  for (let k = buf.length - 1; k >= 0; k--) tokens.push(buf[k]);
  return tokens;
}

// ── Test cases ──
const TESTS = [
  {
    old: "hello",
    new: "hello",
    desc: "完全相同的单词",
    wantTypes: ["context"],
    wantRemTexts: [],
    wantAddTexts: [],
  },
  {
    old: "foo",
    new: "bar",
    desc: "单单词替换",
    wantRemTexts: ["foo"],
    wantAddTexts: ["bar"],
  },
  {
    old: "hello world",
    new: "hello universe",
    desc: "多单词，尾单词变",
    wantRemTexts: ["world"],
    wantAddTexts: ["universe"],
  },
  {
    old: "",
    new: "new",
    desc: "空→有",
    wantRemTexts: [],
    wantAddTexts: ["new"],
  },
  {
    old: "old",
    new: "",
    desc: "有→空",
    wantRemTexts: ["old"],
    wantAddTexts: [],
  },
  {
    old: "a b c",
    new: "a x c",
    desc: "中间单词变",
    wantRemTexts: ["b"],
    wantAddTexts: ["x"],
  },
  {
    old: "abc def",
    new: "abc def ghi",
    desc: "追加单词",
    // split(/(\s+)/) keeps spaces as tokens → space + "ghi" are added
    wantRemTexts: [],
    wantAddTexts: [" ", "ghi"],
  },
  {
    old: "abc def ghi",
    new: "abc def",
    desc: "删除末尾单词",
    wantRemTexts: [" ", "ghi"],
    wantAddTexts: [],
  },
  {
    old: "function foo() {",
    new: "function foo(bar: string) {",
    desc: "代码行——参数新增（按空格分词）",
    // "foo()" is one token; "foo(bar:" + " " + "string)" are 3 tokens
    wantRemTexts: ["foo()"],
    wantAddTexts: ["foo(bar:", " ", "string)"],
  },
  {
    old: "  return x + y;",
    new: "  return x - y;",
    desc: "运算符变",
    wantRemTexts: ["+"],
    wantAddTexts: ["-"],
  },
];

let failed = 0;
for (const t of TESTS) {
  const tokens = diffWords(t.old, t.new);
  const remTexts = tokens.filter((tk) => tk.type === "removed").map((tk) => tk.text);
  const addTexts = tokens.filter((tk) => tk.type === "added").map((tk) => tk.text);

  // Check that all wanted removed texts are present (ignoring order/whitespace)
  const remMissing = t.wantRemTexts.filter((w) => !remTexts.includes(w));
  const remExtra = remTexts.filter((w) => !t.wantRemTexts.includes(w));
  const addMissing = t.wantAddTexts.filter((w) => !addTexts.includes(w));
  const addExtra = addTexts.filter((w) => !t.wantAddTexts.includes(w));

  const pass = remMissing.length === 0 && remExtra.length === 0 &&
               addMissing.length === 0 && addExtra.length === 0;

  if (!pass) {
    failed++;
    console.error(`\n❌ FAIL: ${t.desc}`);
    console.error(`  old:       "${t.old}"`);
    console.error(`  new:       "${t.new}"`);
    console.error(`  all tokens: ${JSON.stringify(tokens.map(tk => `${tk.type}:${tk.text}`))}`);
    if (remMissing.length) console.error(`  rem missing: ${JSON.stringify(remMissing)}`);
    if (remExtra.length) console.error(`  rem extra:   ${JSON.stringify(remExtra)}`);
    if (addMissing.length) console.error(`  add missing: ${JSON.stringify(addMissing)}`);
    if (addExtra.length) console.error(`  add extra:   ${JSON.stringify(addExtra)}`);
  } else {
    console.log(`  ✅ PASS: ${t.desc}`);
    const tokensDisplay = tokens.map(tk => {
      if (tk.type === "removed") return `-${tk.text}-`;
      if (tk.type === "added") return `**${tk.text}**`;
      return tk.text;
    }).join("");
    console.log(`     "${t.old}" → "${t.new}"  →  "${tokensDisplay}"`);
  }
}

console.log(`\n${failed === 0 ? '✅' : '❌'} ${TESTS.length - failed}/${TESTS.length} tests passed.`);
process.exit(failed === 0 ? 0 : 1);
