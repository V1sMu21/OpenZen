/**
 * `detectArtifactType` 判定（round5 P1-13④）。
 *
 * 守的是什么：侧边栏用哪个 viewer 打开文件，全靠这个映射。它有几条**已经
 * 踩过坑的正确口径**（注释里写明"目录名带点不能冒充扩展名"、"点文件没有
 * 扩展名"），一旦被"顺手简化"就会回退成 `path.split('.')`：
 *   - `/a.b/file` 会被误判成扩展名 `b/file`
 *   - `.gitignore` 会被误判成扩展名 `gitignore`
 * 两者都会让侧边栏挑错渲染器（例如把代码当图片打开）。
 */
import { describe, expect, it } from "vitest";
import { detectArtifactType } from "./artifactType";

describe("扩展名 → 渲染器映射", () => {
  it.each([
    ["/w/report.html", "html"],
    ["/w/report.htm", "html"],
    ["/w/doc.pdf", "pdf"],
    ["/w/sheet.xlsx", "spreadsheet"],
    ["/w/sheet.csv", "spreadsheet"],
    ["/w/main.rs", "code"],
    ["/w/App.svelte", "code"],
    ["/w/data.json", "code"],
    ["/w/notes.md", "markdown"],
    ["/w/paper.tex", "latex"],
    ["/w/shot.png", "image"],
    ["/w/shot.webp", "image"],
    ["/w/deck.pptx", "office"],
  ])("%s → %s", (path, expected) => {
    expect(detectArtifactType(path)).toBe(expected);
  });

  it("大小写不敏感（A.PNG 仍是图片）", () => {
    expect(detectArtifactType("/w/A.PNG")).toBe("image");
    expect(detectArtifactType("/w/REPORT.HTML")).toBe("html");
  });
});

describe("已踩过的坑：扩展名必须取自最后一个路径段", () => {
  it("目录名带点不能冒充扩展名", () => {
    // 旧口径用整条路径 split('.') → 会得到 "b/file" 这种假扩展名
    expect(detectArtifactType("/a.b/file")).toBe("code");
    expect(detectArtifactType("/tmp/v1.2/README")).toBe("code");
  });

  it("点文件没有扩展名（点在第 0 位）", () => {
    expect(detectArtifactType("/w/.gitignore")).toBe("code");
    expect(detectArtifactType("/w/.env")).toBe("code");
  });

  it("以点结尾的文件名没有扩展名", () => {
    expect(detectArtifactType("/w/file.")).toBe("code");
  });

  it("未知扩展名 / 无扩展名 → code（兜底而不是 undefined）", () => {
    expect(detectArtifactType("/w/archive.tar.gz")).toBe("code");
    expect(detectArtifactType("/w/LICENSE")).toBe("code");
    expect(detectArtifactType("")).toBe("code");
  });

  it("嵌套目录只认最后一段的扩展名", () => {
    expect(detectArtifactType("/w/assets/img/logo.svg")).toBe("image");
    expect(detectArtifactType("/w/src/deep/nested/mod.ts")).toBe("code");
  });
});
