# OpenZen 桌面端设计规范 — 器物语法 (Artifact Grammar)

> 状态: Draft v1.0 · 2026-08-01
> 配套: [roadmap.md](roadmap.md) Phase 7 · 原型参考 `/Users/macstu/Desktop/openzen-ui-preview-v2.html`
> 前身: `frontends/DESIGN.md`(暖棕珊瑚主题,**本规范正式取代其视觉方向**)

---

## 1. 设计身份 (Identity)

OpenZen 桌面端的视觉语言是**器物语法** — 一件宋韵天青釉器,而非一块软件面板。

**一句话定位**:把"雨过天青云破处"的汝窑美学,翻译成开发者工具的界面语言。

**设计原则 (五条,任何改动不得违反)**:

1. **三色克制** — 全界面只有三种色:釉白(底)、天青(唯一功能色)、墨(文字)。例外仅一个:朱砂(印章与错误,器物上的印泥)。
2. **釉面即界面** — 消息不是卡片,是釉面上的刻痕;工具调用不是网格,是釉下暗纹。视觉复杂度全部来自质感层,不来自结构层。
3. **单色语义** — 状态不用红黄绿圆点,用釉色深浅 + 开片密度 + 朱砂点缀表达。天青=活动/进行,墨=完成/常态,朱砂=错误/危险。
4. **中国谱系** — 天青(北宋汝窑)、开片纹(汝窑/哥窑冰裂纹)、朱砂钤印(书画传统)、宋体铭文(宋代刻本)、楷体手迹(书法)、干支纪年、卷轴天头、书画落款("某某识")。**不是日式侘寂** — 侘寂讲枯寂粗粝,本语言讲精雅克制。
5. **复杂度契约** — 一切"好看"都必须 O(1) 恒定成本(见 §8)。长程任务下 DOM 恒定、纹理平铺、动画走合成层。

---

## 2. 色彩系统 (Color)

### 2.1 Token(暗色主 / 亮色釉下彩)

```css
:root {                        /* 墨夜 (dark) */
  --bg: #14120e;               /* 墨底 */
  --paper: #1a1712;            /* 纸底 */
  --ink: #e4ddca;              /* 暖墨文字 */
  --ink-dim: rgba(228,221,202,.62);
  --ink-faint: rgba(228,221,202,.34);
  --qing: #93c3d6;             /* 天青 — 唯一功能色 */
  --qing-bright: #b6dbe8;
  --qing-dim: rgba(147,195,214,.4);
  --qing-faint: rgba(147,195,214,.14);
  --qing-bg: rgba(147,195,214,.07);   /* 釉下暗纹底 */
  --cinnabar: #c05a3e;         /* 朱砂 — 印章/错误 */
  --hairline: rgba(147,195,214,.12);  /* 青调发丝线 */
  --crackle-op: .03;           /* 开片纹静态透明度 */
  --atmo-op: .5;               /* 大气层径向光 */
  --glaze: inset 0 1px 0 rgba(147,195,214,.10), 0 12px 40px rgba(0,0,0,.38);
  --ease: cubic-bezier(.22,1,.36,1);  /* ease-out-quint 入釉 */
}
[data-theme="light"] {         /* 釉下彩 (light) */
  --bg: #eee9db;               /* 釉白 */
  --paper: #f5f1e5;            /* 宣纸 */
  --ink: #2c2820;
  --qing: #4f8ea8;             /* 釉下青花 */
  --qing-bright: #36748f;
  --cinnabar: #a8452c;
  --hairline: rgba(79,142,168,.16);
  --glaze: inset 0 1px 0 rgba(255,255,255,.5), 0 12px 40px rgba(60,50,30,.10);
}
```

### 2.2 色彩角色

| Token | 角色 | 禁止用途 |
|---|---|---|
| `--qing` | 活动指示、光标、当前项、hover 高亮、折叠头、印章 | 大面积极填充(>10% 视口) |
| `--cinnabar` | 错误、失败工具、未决意见、超限警示 | 普通强调、品牌 |
| `--ink` 系 | 正文、层级递减(dim→faint) | 代替青色作强调 |
| `--qing-bg` | 用户消息釉色条、暗纹底、hover 底 | 卡片背景(禁止卡片!) |

### 2.3 状态语义(替代红黄绿)

| 状态 | 表达 |
|---|---|
| 运行中 | 天青呼吸点(`animation: breath 1.4s`,glow 8px) |
| 已完成 | 墨色「已竟」/ ✓ 等宽小字 |
| 错误 | 朱砂 ✕ + 开片纹加深 + 「朱砂」字样 |
| 超限 (ctx>78%) | 用量条变朱砂 |

---

## 3. 字形系统 (Typography)

| 用途 | 字体 | 规则 |
|---|---|---|
| 铭文 (标题/角色/工具名/按钮) | `--serif: "Songti SC","Noto Serif SC",serif` | 12-13px,letter-spacing .06-.3em |
| 手迹 (思考块) | `--kai: "Kaiti SC","STKaiti","KaiTi",serif` | 13.5px,line-height 2 |
| 正文 (助手输出) | 无衬线 body | 14.5px,line-height 1.75 |
| 等宽 (代码/时间/元数据) | `--mono: ui-monospace,"SF Mono",Menlo` | 10-12.5px |

**禁止**:Inter 作为展示字、纯英文大写标签、JetBrains Mono 作为唯一等宽(系统栈优先,避免额外加载)。

---

## 4. 釉面三层 (Glaze Layers)

全部**固定尺寸平铺 background**,零 JS、零 DOM、GPU 解码一次 → O(1):

1. **大气层** `body::before` — 顶部/右下角天青 radial-gradient,`--atmo-op` 控制
2. **噪点** `body::after` 第二层 — 160px feTurbulence 平铺,opacity .05
3. **开片纹** `body::after` 第一层 — 240px SVG 裂纹线平铺,`--crackle-op`(暗色 .03 / 亮色 .05)

**交互唤醒**:`body:hover::after { opacity: calc(var(--crackle-op) * 1.9) }`,transition 1.2s ease — 釉面"活"的触感。

**禁止**:整页 Canvas 粒子、DOM 节点做噪点、backdrop-filter 玻璃拟态(每帧重采样,最贵)。

---

## 5. 动效语法 (Motion — 入釉)

一切动画统一为**入釉**节奏:`--ease: cubic-bezier(.22,1,.36,1)`,时长 350-600ms。

| 动效 | 实现 | 成本 |
|---|---|---|
| 浸润出现 (soak) | `opacity + translateY(10px) + blur(3px)` → none,.6s | 合成层 |
| token 流式 | 每 token `opacity+translateY(3px)`,90ms | 合成层 |
| 展开/折叠 | `grid-template-rows: 0fr→1fr`,.5s | 布局一次 |
| 釉光扫过 (hover) | `::after` 渐变层 `translateX(-110%)→110%`,.7s | 合成层 |
| 钤印按下 | `scale(.92)` active,.2s | 合成层 |
| 呼吸点 | `opacity` keyframes 1.4s | 合成层 |

**约束**:只动 transform/opacity/filter/grid-rows;禁止动画 layout 属性(width/height/top)。`prefers-reduced-motion: reduce` 时全部动画/过渡关闭。

---

## 6. 布局 (Layout)

```
┌────────────────────────────────────────────────────┐
│ 标题栏 38px [禅] 修砚 · 丙午制  path      ctx用量 复杂度 主题 ⌘K │
├──────────┬─────────────────────────────┬──────────┤
│ 左侧栏    │ 单列叙事流 (max 660px)        │ 右侧栏     │
│ 172px     │  竖排天头「卷一·修砚之录」     │ 224px     │
│ ＋新会话   │  消息…                      │ 物/审/迹  │
│ 项目/会话  │  ───────────────────        │ (tab)     │
│ 底部工具   │  落款「丙午·修砚 识」         │           │
├──────────┴─────────────────────────────┴──────────┤
│ Composer [附 件][输入框]              [言](钤印)      │
└────────────────────────────────────────────────────┘
```

- Grid: `grid-template-columns: 172px 1fr 224px; rows: 38px 1fr auto`
- 叙事流居中窄栏(660px),左右留白 — 像展开的手卷
- 竖排天头: `writing-mode: vertical-rl; text-orientation: upright`,左缘青线
- 落款行: 消息流底部 `[禅小印] 丙午 · 修砚 识于杭州`

---

## 7. 组件规范 (Components)

### 7.1 标题栏 (Titlebar)

- 左: `[禅]` 天青印章 22px(宋体 700)+ 名「修砚」+ 干支款「丙午 制」(青线分隔)
- 右: ctx 用量条(72×3px 青线,>78% 变朱砂)、复杂度 toggle、主题 toggle、⌘K 搜索
- 底部 1px `--hairline`

### 7.2 左侧栏 (Sidebar)

- 顶: 「＋ 新 会 话」印章式按钮(天青描边,釉光扫过 hover)
- 「项 目」/「会 话」竖排间距标题(`letter-spacing:.3em`),列表项 12.5px
- 当前项: 左侧 2px 天青线 + `--qing-faint` 底
- 底: 设置/色/端 三枚小字按钮
- **不可折叠**(对比 v1 的 icon-only 方案 — 用户明确保留)

### 7.3 消息 (Messages)

- **用户消息** = 釉色条: 整行 `--qing-bg` 底 + 左缘 2px `--qing-dim` 线,右上「砚 主」宋体,右缘 max-width 84%
- **助手消息** = 纸上墨: 无容器、无底色、纯墨字,左侧无装饰
- **时间戳** = 落款格式「砚主 识 · 10:24:01」/「修砚 识 · 10:24:52」,宋体 10.5px `--ink-faint`
- 代码块: `--paper` 底 + hairline 边框 + `--glaze`,圆角 4px,mono 12.5px

### 7.4 思考块 (ThinkingBlock) — 楷体手迹

- 折叠行: `⚘ 静思 · 推演方案` 楷体 13px + 右侧 mono 耗时,点击展开
- 展开体: `grid-template-rows 0fr→1fr` 动画,内文楷体 13.5px、行高 2、左缘 2px 青线
- 流式思考: 楷体逐 token 入釉

### 7.5 工具调用 (Tool Call) — 釉下暗纹

- **折叠态**: 一行 `◈ 工具名` 宋体 + 右状态(呼吸点「运行」/「已竟」/「朱砂 ✕」+ mono 耗时),下缘 hairline,**无卡片**
- **展开态**: `--qing-bg` 底 + hairline 边框 + `--glaze`,mono 参数与结果;错误结果朱砂
- 展开动画 0.5s 入釉

### 7.6 时间线折叠 (Timeline Folding) — 长程任务核心

```
▸ 时 间 线 · 折叠 8 事 · 8 工具 · 12.4s      [1 ⚠]
```

- 虚线天青框 + `--qing-faint` 底 + 聚合统计(计数/总耗时/错误数)
- **折叠区事件不挂入 DOM**: 数据留内存,展开时按需 appendChild → 渲染层 O(1)
- 折叠阈值 `FOLD_THRESHOLD = 5`(与 ChatMessage.svelte 现有实现一致)
- 错误标记: 折叠头右侧朱砂「⚠ 1」

### 7.7 Composer — 题字落款

- 「附 件」按钮(细线青框,釉光扫过)+ 输入框(`--paper` 底,聚焦时天青 glow 3px)+ 快捷键提示 `<kbd>⌘</kbd> 发送 · <kbd>⇧⏎</kbd> 换行`
- 发送钮 = **钤印**: 34px 天青方印,宋体「言」,按下 `scale(.92)`;busy 态变纸底朱砂「止」

### 7.8 右侧栏 (百宝格) — 物 / 审 / 迹

- 三 tab 单字宋体: 物(产物)、审(复核)、迹(活动时间线),当前 tab 下缘 2px 天青线
- 产物/复核项: 单行 `[卷/文/图/表] 名 状态 mono 大小`,hover 釉光
- **迹 tab**: 活动时间线,>5 条时折叠成虚线头(同 7.6 策略),展开才挂 DOM
- ctx 用量: `qwen3-coder · ▬▬▬ 12.4k/128k`,发送时实时爬升,>78% 朱砂

### 7.9 文化锚点 (身份强化)

- 干支款识: 标题栏「丙午 制」;落款「丙午 · 修砚 识于杭州」
- 竖排天头: 消息流左侧「卷一 · 修砚之录」vertical-rl
- 落款格式: 所有时间戳「谁 识 · 时刻」
- 印章系统: 主章「禅」/ 发送章「言」/ 小章「禅」(落款处)

---

## 8. 复杂度契约 (Performance Contract)

> 视觉复杂 ≠ 运行复杂。长程任务(10h+ soak)必须满足以下约束,否则视为设计违规。

| 层 | 约束 | 复杂度 |
|---|---|---|
| 消息 DOM | 时间线折叠 + 虚拟滚动,只渲染 FOLD 条 + 视口 | **O(1)** |
| 事件数据 | 保留于内存(可恢复),渲染层恒定 | O(n) 内存 |
| 纹理三层 | 固定 ≤512px 平铺,GPU 解码一次,零 JS | **O(1)** |
| 动画 | 仅 transform/opacity/filter/grid-rows,合成层 | **O(1)/帧** |
| 流式渲染 | rAF 合并,每帧 ≤1 flush | **O(1)/帧** |
| 折叠态更新 | `$derived` 仅在折叠集变化时重算 | O(折叠数) |

**禁止清单**:
- `backdrop-filter` 玻璃拟态(每帧重采样)
- 整页大 Canvas / DOM 粒子
- 动画 layout 属性(width/height/top/left)
- 无上限的事件列表渲染(必须折叠或虚拟化)

**验收工具**: 原型「复杂度」toggle 显示的徽章标注每个组件的复杂度;实现后以 10h soak 验证 RSS 稳定(目标 ≤350MB)。

---

## 9. 与旧设计的关系 (Migration)

| 旧 (`frontends/DESIGN.md`) | 新 (本规范) | 处置 |
|---|---|---|
| 暖棕 `#181715` + 珊瑚 `#cc785c` | 墨夜 `#14120e` + 天青 `#93c3d6` | 替换所有 token |
| 卡片/阴影层级 | 釉面/发丝线/刻痕 | 组件重构 |
| Inter 展示字 | 宋体铭文 + 楷体手迹 | 替换 |
| 红黄绿语义 | 天青/墨/朱砂 | 替换 |

迁移顺序: token 层 → 布局骨架 → 消息组件 → 工具/思考 → 侧栏 → composer → 锚点。每步保功能,最后统一过复杂度审计。
