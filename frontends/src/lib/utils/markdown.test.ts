/**
 * markdown 渲染的转义纪律（round5 P1-13④）。
 *
 * 守的是什么：`renderMarkdown` 的输出直接进 `{@html}`，所以它是 XSS 的
 * 唯一闸门。两条不变量：
 *   1. 原始 HTML 必须被转义（不能穿透成标签）；
 *   2. 链接/图片 URL 走允许列表 —— `javascript:` / `data:` / `vbscript:`
 *      必须退化成纯文本，不能变成可点的 href/src。
 * 这两条一旦回退，聊天内容（含工具输出、网页抓取结果）就能在 webview 里
 * 执行脚本。
 */
import { describe, expect, it } from "vitest";
import { renderMarkdown, renderStreamingFragment } from "./markdown";

describe("原始 HTML 必须被转义", () => {
  it("<script> 不穿透", () => {
    const html = renderMarkdown("<script>alert(1)</script>");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });

  it("带属性的注入（img onerror）不穿透", () => {
    const html = renderMarkdown('<img src=x onerror="alert(1)">');
    expect(html).not.toMatch(/<img[^>]*onerror/i);
    expect(html).toContain("&lt;img");
  });

  it("代码块里的 HTML 也保持转义", () => {
    const html = renderMarkdown("```\n<script>alert(1)</script>\n```");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });

  it("renderStreamingFragment（流式半成品）同样转义", () => {
    const html = renderStreamingFragment("<script>alert(1)</script>");
    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });

  it("& 先于 < > 转义（不产生二次实体）", () => {
    const html = renderMarkdown("a & b < c");
    expect(html).toContain("&amp;");
    expect(html).not.toContain("&amp;lt;");
  });
});

describe("URL 允许列表", () => {
  it("javascript: 链接不得变成可点 href", () => {
    const html = renderMarkdown("[click](javascript:alert(1))");
    expect(html).not.toMatch(/href\s*=\s*["']?javascript:/i);
  });

  it("data: 链接不得变成可点 href", () => {
    const html = renderMarkdown("[x](data:text/html;base64,PHNjcmlwdD4=)");
    expect(html).not.toMatch(/href\s*=\s*["']?data:/i);
  });

  it("vbscript: 链接不得变成可点 href", () => {
    const html = renderMarkdown("[x](vbscript:msgbox(1))");
    expect(html).not.toMatch(/href\s*=\s*["']?vbscript:/i);
  });

  it("https 链接正常渲染成 href", () => {
    const html = renderMarkdown("[docs](https://example.com/a?b=1)");
    expect(html).toContain('href="https://example.com/a?b=1"');
  });

  it("javascript: 图片不得变成 src", () => {
    const html = renderMarkdown("![x](javascript:alert(1))");
    expect(html).not.toMatch(/src\s*=\s*["']?javascript:/i);
  });
});
