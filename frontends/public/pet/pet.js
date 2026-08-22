/* 阿青 · 桌面小猫咪（插件式）— pet.js
   独立 webview：逐帧 canvas 播放、sse_event 状态映射、交叉淡化切换、localStorage 持久化 */
(function () {
  "use strict";

  const FPS = 10, FRAMES = 32, LOOP_SEC = FRAMES / FPS, FADE_MS = 300, IDLE_TIMEOUT_MS = 4000;
  const STATES = ["idle_sleep", "working", "thinking", "waiting", "error", "done"];
  const STATE_TEXTS = {
    idle_sleep: ["休息中", "Idle"], working: ["执行中", "Working"],
    thinking: ["推理中", "Thinking"], waiting: ["等待中", "Waiting"],
    error: ["报错啦", "Error"], done: ["完成啦", "Done"],
  };
  const PET_KEY = "openzen.pet";
  const DEFAULT_STATE = {
    name: "阿青",
    affinity: { points: 0, pets: 0, lastPetAt: 0, treats: 0, lastTreatAt: 0 },
    soul: { mood: "宁静", tasksDone: 0, tokensConsumed: 0, lastTaskAt: 0 },
    display: { visible: true, size: 160, x: 100, y: 100 },
  };

  let active = "idle_sleep";
  let frameIdx = 0;
  let lastEventAt = 0;
  let todo = { current: 0, total: 0 };
  let contextPct = 0;
  let pet = loadState();
  let frames = {};           // state -> [Image]
  let loaded = 0, started = false;

  const cvA = document.getElementById("catA"), cvB = document.getElementById("catB");
  const cA = cvA.getContext("2d"), cB = cvB.getContext("2d");
  const zhText = document.getElementById("zhText"), enText = document.getElementById("enText");
  const tooltip = document.getElementById("tooltip"), card = document.getElementById("card"), menu = document.getElementById("menu");
  let displayCv = cvA, displayCtx = cA;   // 当前显示层

  // ---------- 持久化 ----------
  function loadState() {
    let raw = {};
    try { raw = JSON.parse(localStorage.getItem(PET_KEY) || "{}"); } catch (e) {}
    const st = JSON.parse(JSON.stringify(DEFAULT_STATE));
    Object.assign(st, raw);
    Object.assign(st.affinity, DEFAULT_STATE.affinity, raw.affinity || {});
    Object.assign(st.soul, DEFAULT_STATE.soul, raw.soul || {});
    Object.assign(st.display, DEFAULT_STATE.display, raw.display || {});
    return st;
  }
  function save() { localStorage.setItem(PET_KEY, JSON.stringify(pet)); }

  // ---------- 帧加载 ----------
  // 不等待全部 192 帧：idle 任一帧就绪即起播，其余帧到达后被 loop 直接取用；
  // onerror 也计数，个别帧加载失败绝不卡死整体动画。
  const TOTAL_FRAMES = STATES.length * FRAMES;
  function markProgress() { loaded++; if (!started) tryStart(); }
  function loadFrames() {
    STATES.forEach(function (st) {
      frames[st] = [];
      for (let i = 0; i < FRAMES; i++) {
        const img = new Image();
        img.src = "frames_webp/" + st + "/f_" + i.toString().padStart(2, "0") + ".webp";
        img.onload = markProgress;
        img.onerror = markProgress;
        frames[st].push(img);
      }
    });
    tryStart();
  }
  function drawImage(ctx, img) {
    if (!img || !img.complete) return;
    ctx.clearRect(0, 0, 768, 768);
    // 关键帧 WebP 源为 256px，放大绘制到 768 画布（显示端 CSS 缩小，视觉无损）
    ctx.drawImage(img, 0, 0, 768, 768);
  }

  let raf = 0, lastTs = 0, acc = 0;
  function tryStart() {
    if (started) return;
    const idle = frames.idle_sleep || [];
    const first = idle.find(function (im) { return im && im.complete; }) || idle[0];
    if (!first) return;
    started = true;
    drawImage(displayCtx, first);
    lastTs = performance.now();
    raf = requestAnimationFrame(loop);
    updateText();
  }
  // 兜底：极个别帧迟迟不回时 2.5s 后强行起播（未就绪帧由 drawImage 跳过）
  setTimeout(function () { tryStart(); }, 2500);

  function loop(ts) {
    const dt = Math.min(100, ts - lastTs); lastTs = ts;
    // 累计小数步进，按 FPS 精确走帧（Math.round 逐帧取整在 60Hz 下恒为 0，动画会冻结）
    acc += dt / 1000 * FPS;
    if (acc >= 1) {
      const step = Math.floor(acc);
      acc -= step;
      frameIdx = (frameIdx + step) % FRAMES;
    }
    const img = frames[active][frameIdx];
    drawImage(displayCtx, img);   // drawImage 内部已判空/判未就绪
    document.body.dataset.petFrame = active + ":" + frameIdx;  // 每帧标志，供外部观测/调试
    raf = requestAnimationFrame(loop);
  }

  // ---------- 切换（交叉淡化 + 相位对齐） ----------
  // 立即切换显示层：loop 全程把"新状态"画到新层，旧层定格旧状态淡出，
  // 文字/卡片同步更新 —— 状态与画面永不失配。
  function switchTo(state) {
    if (state === active) return;
    const phase = frameIdx / FRAMES;
    active = state;
    frameIdx = Math.round(phase * FRAMES) % FRAMES;

    const oldCv = displayCv;
    const toCv = displayCv === cvA ? cvB : cvA;
    const toCtx = toCv.getContext("2d");
    const img = frames[active][frameIdx];
    drawImage(toCtx, img);
    displayCv = toCv; displayCtx = toCtx;

    // 交叉淡化：新层 0→1，旧层（定格旧状态）1→0
    toCv.style.transition = "none"; toCv.style.opacity = "0";
    void toCv.offsetWidth;                      // 强制重排让 opacity:0 落地
    toCv.style.transition = "opacity " + FADE_MS + "ms ease";
    oldCv.style.transition = "opacity " + FADE_MS + "ms ease";
    requestAnimationFrame(function () {
      toCv.style.opacity = "1";
      oldCv.style.opacity = "0";
    });
    setTimeout(function () { toCv.style.opacity = ""; oldCv.style.opacity = ""; }, FADE_MS + 30);
    updateText(); updateCard();
  }

  // ---------- 事件 → 状态 ----------
  function onEvent(evt) {
    const env = (evt && evt.payload) ? evt.payload : evt;
    if (!env || !env.event_type) return;
    const type = env.event_type;
    let inner = null;
    try { inner = env.data ? JSON.parse(env.data) : null; } catch (e) {}
    const itype = inner && inner.type ? inner.type : "";
    let target = null;

    if (type === "ask_user_pending" || itype === "ask_user_pending" || type === "approval_needed") target = "waiting";
    else if (type === "done") { target = "done"; pet.soul.tasksDone++; pet.affinity.treats++; pet.soul.lastTaskAt = Date.now(); pet.soul.mood = "开心"; save(); }
    else if (type === "error" || itype === "error") target = "error";
    else if (itype.indexOf("reasoning") === 0) target = "thinking";
    else if (itype === "data_todo_update" && inner) { todo = { current: inner.current || 0, total: inner.total || 0 }; target = "working"; }
    else if (itype === "data_context_usage" && inner) { contextPct = Math.min(100, Math.round((inner.current_tokens || 0) / (inner.context_window || 1) * 100)); target = "working"; }
    else if (itype.indexOf("tool_") === 0 || itype.indexOf("text_") === 0 || itype.indexOf("on_artifact") === 0) target = "working";

    if (target) { lastEventAt = Date.now(); if (target !== active) switchTo(target); updateCard(); }
  }
  function idleCheck() {
    if (active !== "idle_sleep" && Date.now() - lastEventAt > IDLE_TIMEOUT_MS) switchTo("idle_sleep");
  }

  // ---------- 文字 / 卡片 ----------
  function updateText() {
    const p = STATE_TEXTS[active] || STATE_TEXTS.idle_sleep;
    zhText.textContent = p[0]; enText.textContent = p[1];
  }
  function updateCard() {
    document.getElementById("cardTitle").textContent = pet.name + " · 修砚";
    document.getElementById("cardState").textContent = "状态：" + (STATE_TEXTS[active][0]);
    const has = todo.total > 0;
    document.getElementById("progressText").textContent = has ? todo.current + "/" + todo.total : "--";
    document.getElementById("progressFill").style.width = has ? (todo.current / todo.total * 100) + "%" : "0%";
    document.getElementById("contextText").textContent = contextPct + "%";
    document.getElementById("moodText").textContent = pet.soul.mood;
    document.getElementById("affText").textContent = pet.affinity.points;
  }

  // ---------- 交互 ----------
  let tooltipTimer = null;
  function bindInteraction() {
    // 悬停 → 浮签
    document.body.addEventListener("mouseenter", function () { tooltip.hidden = false; tickTip(); });
    document.body.addEventListener("mouseleave", function () { tooltip.hidden = true; });
    function tickTip() {
      if (tooltip.hidden) return;
      tooltip.textContent = (STATE_TEXTS[active][0]) + (todo.total ? " · 步骤 " + todo.current + "/" + todo.total : "");
      tooltipTimer = setTimeout(tickTip, 400);
    }

    // 单击 → 状态卡；双击 → 回主窗（用延迟区分）
    let cTimer = null;
    document.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      cTimer = setTimeout(function () { if (!menu.hidden) return; card.hidden = !card.hidden; }, 220);
    });
    document.addEventListener("mouseup", function () { clearTimeout(cTimer); });
    document.addEventListener("dblclick", function () { clearTimeout(cTimer); card.hidden = true; restoreMain(); });

    // 拖拽由 data-tauri-drag-region 在 OS 层原生处理（整个窗口可拖）

    // 抚摸：按住不动快速左右划 → 呼噜 + affinity
    let petX = 0, wig = 0, wStart = 0;
    document.addEventListener("mousedown", function () { petX = 0; wig = 0; wStart = Date.now(); });
    document.addEventListener("mousemove", function (e) {
      const dx = e.movementX || 0;
      if (Math.abs(dx) > 4) { wig++; petX += dx; if (wig > 2 && Date.now() - wStart > 150) petIt(); }
    });

    // 右键菜单
    document.addEventListener("contextmenu", function (e) { e.preventDefault(); menu.hidden = !menu.hidden; });
    document.addEventListener("click", function (e) { if (!menu.hidden && !e.target.closest("#menu")) menu.hidden = true; });
    menu.addEventListener("click", function (e) {
      const act = e.target.dataset && e.target.dataset.act; if (!act) return; menu.hidden = true;
      if (act === "back") restoreMain();
      else if (act === "rename") { const n = prompt("给小猫起个名字：", pet.name); if (n) { pet.name = n; save(); updateCard(); } }
      else if (act === "quit") quit();
    });
  }
  function petIt() {
    const now = Date.now();
    if (now - pet.affinity.lastPetAt < 10000) return;
    pet.affinity.points++; pet.affinity.pets++; pet.affinity.lastPetAt = now;
    pet.soul.mood = "开心"; save(); updateCard();
    const prev = active;
    if (prev !== "done") {
      switchTo("done");
      setTimeout(function () { if (active === "done" && Date.now() - lastEventAt > 1500) switchTo(prev); }, 800);
    }
  }

  // ---------- 窗口操作 ----------
  function getMain() {
    const T = globalThis.__TAURI__;
    if (T && T.window && T.window.WebviewWindow) return T.window.WebviewWindow.getByLabel("main");
    return null;
  }
  function getSelf() {
    const T = globalThis.__TAURI__;
    if (T && T.window && T.window.getCurrentWindow) return T.window.getCurrentWindow();
    return null;
  }
  function restoreMain() {
    const m = getMain();
    if (m) { try { m.show(); m.setFocus(); m.unminimize(); } catch (e) {} }
    hideSelf();
  }
  function hideSelf() {
    pet.display.visible = false; save();
    const s = getSelf(); if (s) s.hide().catch(function () {});
  }
  function quit() {
    pet.display.visible = false; save();
    const s = getSelf(); if (s) s.close().catch(function () {});
  }

  // ---------- 启动 ----------
  function boot() {
    // 恢复位置（若之前被拖走）
    const s = getSelf();
    if (s && pet.display.x != null) { try { s.setPosition(pet.display.x, pet.display.y); } catch (e) {} }
    updateText(); updateCard();

    if (globalThis.__TAURI__ && globalThis.__TAURI__.event) {
      globalThis.__TAURI__.event.listen("sse_event", onEvent).catch(function () {});
    } else {
      // 浏览器演示模式：周期模拟事件
      const demo = ["done", "error", "ask_user_pending"];
      let d = 0;
      setInterval(function () {
        onEvent({ event_type: demo[d % 3], data: "" }); d++;
      }, 5000);
    }
    setInterval(idleCheck, 2000);
  }

  
  document.addEventListener("DOMContentLoaded", function () {
    boot();
    loadFrames();
  });
})();
