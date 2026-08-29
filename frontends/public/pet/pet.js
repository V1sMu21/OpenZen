/* 阿青 · 桌面小猫咪（插件式）— pet.js
   独立 webview：逐帧 canvas 播放、sse_event 状态映射、交叉淡化切换、localStorage 持久化 */
(function () {
  "use strict";

  const FPS = 10, FRAMES = 32, LOOP_SEC = FRAMES / FPS, FADE_MS = 300;
  // 无事件回落睡觉的时长：工具调用间隙常有 4-8s 静默，太短会让猫在
  // 任务中途睡着又惊醒（"转换混乱"观感的一部分），放宽到 9s；
  // done 态单独更短（DONE_LINGER_MS），庆祝完就休息。
  const IDLE_TIMEOUT_MS = 9000, DONE_LINGER_MS = 4000;
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
  //
  // 幽灵残影修复：旧实现淡出结束后把两层 opacity 重置为 ""（回落到 CSS
  // 默认值 1），旧画布的冻结帧重新变回完全不透明，且 DOM 靠后的 catB 永远
  // 压在上层 —— 表现为"冻住的旧状态猫贴在活动猫背后"。现在非显示层永远
  // 钉在 opacity:0 且淡出后被 clearRect 清空；清理定时器可被下一次切换
  // 取消收敛，快速连续切换不残留半途淡化状态。
  let fadeTimer = null;
  let lastSwitchAt = 0;

  function doSwitch(state) {
    if (state === active) return;
    const phase = frameIdx / FRAMES;
    active = state;
    frameIdx = Math.round(phase * FRAMES) % FRAMES;

    if (fadeTimer) { clearTimeout(fadeTimer); fadeTimer = null; }

    const oldCv = displayCv;
    const toCv = displayCv === cvA ? cvB : cvA;
    const toCtx = toCv.getContext("2d");

    // 钉到确定起点，抹掉被打断淡化留下的中间态。
    oldCv.style.transition = "none"; oldCv.style.opacity = "1";
    toCv.style.transition = "none"; toCv.style.opacity = "0";
    toCtx.clearRect(0, 0, 768, 768);
    void toCv.offsetWidth;

    drawImage(toCtx, frames[active][frameIdx]);
    toCv.style.zIndex = "2"; oldCv.style.zIndex = "1";
    displayCv = toCv; displayCtx = toCtx;

    toCv.style.transition = "opacity " + FADE_MS + "ms ease";
    oldCv.style.transition = "opacity " + FADE_MS + "ms ease";
    requestAnimationFrame(function () {
      toCv.style.opacity = "1";
      oldCv.style.opacity = "0";
    });
    // 淡出完成：旧层保持透明并清空像素 —— 绝不让旧层回到可见态。
    fadeTimer = setTimeout(function () {
      fadeTimer = null;
      oldCv.style.transition = "none";
      oldCv.style.opacity = "0";
      oldCv.getContext("2d").clearRect(0, 0, 768, 768);
    }, FADE_MS + 30);
    updateText(); updateCard();
  }

  // 节流 + 驻留（防状态抖动）：
  // - SWITCH_MIN_MS：两次真实切换的最小间隔（>FADE_MS，淡化完整走完）；
  // - SOFT_DWELL_MS：working/thinking 这类软状态必须"连续被请求"超过该时长
  //   才真正切换 —— 流式输出中 reasoning/text 事件交替到达，直接切换会让猫
  //   在思考/执行之间疯狂闪跳（"状态转换混乱"的主因）；
  // - error/waiting/done/idle 属强信号：立即排队切换。
  const SWITCH_MIN_MS = 420;
  const SOFT_DWELL_MS = 700;
  let pendingTimer = null;   // 定时切换句柄
  let pendingState = null;   // 定时切换目标
  let pendingAt = 0;         // 该定时器应触发的时刻
  let candState = null;      // 当前驻留候选（软状态）
  let candSince = 0;         // 候选开始被连续请求的时刻

  function isSoft(state) { return state === "working" || state === "thinking"; }

  function requestState(state, immediate) {
    if (state === active) { candState = null; return; }
    const now = Date.now();
    // 软状态要过驻留门；强信号（immediate 或非软状态）即刻排队
    let dueAt = now;
    if (!immediate && isSoft(state)) {
      if (candState !== state) { candState = state; candSince = now; }
      dueAt = Math.max(now, candSince + SOFT_DWELL_MS);
    }
    // 节流：与上一次切换至少间隔 SWITCH_MIN_MS
    dueAt = Math.max(dueAt, lastSwitchAt + SWITCH_MIN_MS);
    // 已有更早的排程则不动（后到的请求改写 pendingState 在触发时校验）
    if (pendingTimer && dueAt >= pendingAt) { pendingState = state; return; }
    if (pendingTimer) clearTimeout(pendingTimer);
    pendingAt = dueAt;
    pendingState = state;
    pendingTimer = setTimeout(function () {
      pendingTimer = null;
      const s = pendingState; pendingState = null;
      if (!s || s === active) return;
      // 触发时二次校验：软状态若已被新候选取代，放弃过期切换；
      // 强状态不受候选约束。
      if (isSoft(s) && candState !== s) return;
      candState = null;
      lastSwitchAt = Date.now();
      doSwitch(s);
    }, Math.max(0, dueAt - now));
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

    if (target) {
      lastEventAt = Date.now();
      requestState(target, target === "error" || target === "waiting");
      updateCard();
    }
  }
  function idleCheck() {
    if (active === "idle_sleep") return;
    const limit = active === "done" ? DONE_LINGER_MS : IDLE_TIMEOUT_MS;
    if (Date.now() - lastEventAt > limit) requestState("idle_sleep");
  }

  // ---------- 文字 / 卡片 ----------
  function updateText() {
    const p = STATE_TEXTS[active] || STATE_TEXTS.idle_sleep;
    zhText.textContent = p[0]; enText.textContent = p[1];
    // 窗口标题同步状态：对用户是信息，对自动化验证是可读通道
    document.title = "阿青 · " + p[0];
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
      tooltip.textContent = (STATE_TEXTS[active][0]) + (todo.total ? " · 步骤 " + todo.current + "/" + todo.total : "") + " · 双击回主窗 / 右键菜单";
      tooltipTimer = setTimeout(tickTip, 400);
    }

    // 单击 → 状态卡；双击 → 回主窗（用延迟区分）
    let cTimer = null;
    document.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      cTimer = setTimeout(function () { if (!menu.hidden || !renameBox.hidden) return; card.hidden = !card.hidden; }, 220);
    });
    document.addEventListener("mouseup", function () { clearTimeout(cTimer); });
    document.addEventListener("dblclick", function () { clearTimeout(cTimer); card.hidden = true; restoreMain(); });

    // 拖拽：按住并移动 >4px → 调用原生 start_dragging（OS 级跟手）。
    // 不能用 data-tauri-drag-region：Tauri 的 drag 脚本对 mousedown 做
    // stopImmediatePropagation + 立即 start_dragging，会把单击状态卡、
    // 双击回主窗全部吃掉。阈值方案保留全部点击语义，移动即转为拖窗。
    // 菜单/状态卡等可交互元素不触发拖拽。
    let dsX = 0, dsY = 0, dragArmed = false, dragStarted = false;
    document.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      if (e.target.closest("#menu, #card, #renameBox")) { dragArmed = false; return; }
      dsX = e.clientX; dsY = e.clientY; dragArmed = true; dragStarted = false;
    });
    document.addEventListener("mousemove", function (e) {
      if (!dragArmed || dragStarted) return;
      if (Math.hypot(e.clientX - dsX, e.clientY - dsY) > 4) {
        dragStarted = true; dragArmed = false;
        clearTimeout(cTimer);   // 已判定为拖拽，别再开状态卡
        document.body.classList.add("dragging");
        const s = getSelf();
        if (s && typeof s.startDragging === "function") s.startDragging().catch(function () {});
        else invokeTauri("plugin:window|start_dragging", {}).catch(function () {});
      }
    });
    document.addEventListener("mouseup", function () {
      dragArmed = false;
      if (dragStarted) {
        dragStarted = false;
        document.body.classList.remove("dragging");
        savePosition();
      }
    });

    // 抚摸：指针悬停在猫身上快速左右划 → 呼噜 + affinity。
    // 按住划动已被原生拖拽接管（OS 吞掉后续 mousemove），因此抚摸
    // 改为"未按下"的悬停划动；500ms 无动作自动复位计数。
    let wig = 0, wigAt = 0;
    document.addEventListener("mousemove", function (e) {
      if (e.buttons) { wig = 0; return; }   // 按下 = 拖拽/点击，不算抚摸
      const dx = e.movementX || 0;
      if (Math.abs(dx) > 5) { wig++; wigAt = Date.now(); if (wig > 3) petIt(); }
    });
    setInterval(function () { if (Date.now() - wigAt > 500) wig = 0; }, 500);

    // 右键菜单
    document.addEventListener("contextmenu", function (e) { e.preventDefault(); menu.hidden = !menu.hidden; });
    document.addEventListener("click", function (e) {
      if (!menu.hidden && !e.target.closest("#menu")) menu.hidden = true;
      if (!renameBox.hidden && !e.target.closest("#renameBox")) closeRename(false);
    });
    menu.addEventListener("click", function (e) {
      const act = e.target.dataset && e.target.dataset.act; if (!act) return; menu.hidden = true;
      if (act === "back") restoreMain();
      else if (act === "rename") openRename();
      else if (act === "quit") quit();
    });

    // 改名：WKWebView 没有原生 prompt()（静默返回 undefined，点击无反应的
    // 根因），用页面内对话框替代。打开时把宠物窗设为 key window——
    // 否则输入框拿不到键盘事件；成功后弹状态卡展示新名字。
    const renameBox = document.getElementById("renameBox");
    const renameInput = document.getElementById("renameInput");
    function openRename() {
      renameBox.hidden = false;
      renameInput.value = pet.name || "";
      const s = getSelf();
      if (s && s.setFocus) s.setFocus().catch(function () {});
      setTimeout(function () { renameInput.focus(); renameInput.select(); }, 80);
    }
    function closeRename(saveIt) {
      if (saveIt) {
        const n = renameInput.value.trim();
        if (n && n !== pet.name) {
          pet.name = n; save(); updateCard();
          card.hidden = false;   // 弹卡展示新名字作为成功反馈
        }
      }
      renameBox.hidden = true;
    }
    document.getElementById("renameOk").addEventListener("click", function () { closeRename(true); });
    document.getElementById("renameCancel").addEventListener("click", function () { closeRename(false); });
    renameInput.addEventListener("keydown", function (e) {
      if (e.key === "Enter") closeRename(true);
      else if (e.key === "Escape") closeRename(false);
    });
  }
  function petIt() {
    const now = Date.now();
    if (now - pet.affinity.lastPetAt < 10000) return;
    pet.affinity.points++; pet.affinity.pets++; pet.affinity.lastPetAt = now;
    pet.soul.mood = "开心"; save(); updateCard();
    const prev = active;
    if (prev !== "done") {
      requestState("done", true);
      setTimeout(function () { if (active === "done" && Date.now() - lastEventAt > 1500) requestState(prev); }, 800);
    }
  }

  // ---------- 窗口操作 ----------
  // 注意：Tauri v2 的 WebviewWindow 类在 __TAURI__.webviewWindow 下，
  // v1 时代的 __TAURI__.window.WebviewWindow 在 v2 全局包里不存在
  // （此前 getMain() 永远返回 null 的根因）。两个命名空间都试以兼容。
  function getWebviewWindowClass() {
    const T = globalThis.__TAURI__;
    if (T && T.webviewWindow && T.webviewWindow.WebviewWindow) return T.webviewWindow.WebviewWindow;
    if (T && T.window && T.window.WebviewWindow) return T.window.WebviewWindow; // v1 fallback
    return null;
  }
  function getMain() {
    const WW = getWebviewWindowClass();
    if (WW && WW.getByLabel) return WW.getByLabel("main");
    return null;
  }
  function getSelf() {
    const T = globalThis.__TAURI__;
    if (T && T.window && T.window.getCurrentWindow) return T.window.getCurrentWindow();
    // v2 另一入口：webviewWindow 命名空间同样持有当前窗口句柄
    if (T && T.webviewWindow && T.webviewWindow.getCurrentWebviewWindow) return T.webviewWindow.getCurrentWebviewWindow();
    return null;
  }
  function invokeTauri(cmd, args) {
    const T = globalThis;
    if (T.__TAURI_INTERNALS__ && T.__TAURI_INTERNALS__.invoke) {
      return T.__TAURI_INTERNALS__.invoke(cmd, args);
    }
    if (T.__TAURI__ && T.__TAURI__.core && T.__TAURI__.core.invoke) {
      return T.__TAURI__.core.invoke(cmd, args);
    }
    return Promise.reject(new Error("Tauri IPC unavailable"));
  }
  function hideWindowAnyWay() {
    // 三级降级：API 对象方法 → 兜底 IPC hide → IPC destroy。
    // 前两级都失败（API 形状漂移/权限缺失）时宁可销毁窗口也不留一个
    // 关不掉的宠物 —— 主窗 seal 会走动态重建路径把它找回来。
    return new Promise(function (resolve) {
      const s = getSelf();
      const viaIpc = function (cmd) {
        return invokeTauri(cmd, { label: "pet" })
          .then(function () { resolve(true); })
          .catch(function () {
            if (cmd === "plugin:window|hide") return viaIpc("plugin:window|destroy");
            resolve(false);
          });
      };
      if (s && typeof s.hide === "function") {
        s.hide().then(function () { resolve(true); }, function () { viaIpc("plugin:window|hide"); });
      } else {
        viaIpc("plugin:window|hide");
      }
    });
  }
  function savePosition() {
    try {
      const s = getSelf();
      if (s && s.outerPosition) {
        s.outerPosition().then(function (p) {
          if (p && typeof p.x === "number") { pet.display.x = p.x; pet.display.y = p.y; save(); }
        }).catch(function () {});
      }
    } catch (e) {}
  }
  function restoreMain() {
    const m = getMain();
    if (m) { try { m.show(); m.setFocus(); m.unminimize(); } catch (e) {} }
    hideSelf();
  }
  function hideSelf() {
    savePosition();
    pet.display.visible = false; save();
    hideWindowAnyWay();
  }
  // "退出猫咪"= 隐藏而非销毁：静态声明的宠物窗走主窗同款加载路径，
  // show() 秒回；close() 销毁后只能走动态重建（历史上不可靠）。
  function quit() {
    hideSelf();
  }

  // ---------- 启动 ----------
  function boot() {
    // 仅当 localStorage 里存过真实位置才恢复；否则保持系统默认，
    // 不再每次启动都被默认值拽回 (100,100)。
    let hadSavedPos = false;
    try {
      const raw = JSON.parse(localStorage.getItem(PET_KEY) || "{}");
      hadSavedPos = !!(raw && raw.display && raw.display.x != null);
    } catch (e) {}
    const s = getSelf();
    if (s && hadSavedPos && pet.display.x != null) { try { s.setPosition(pet.display.x, pet.display.y); } catch (e) {} }
    // 拖动结束后落盘位置（节流 2s），下次启动原位恢复。
    if (s && s.listen) {
      let lastMoveSave = 0;
      s.listen("tauri://moved", function () {
        const now = Date.now();
        if (now - lastMoveSave < 2000) return;
        lastMoveSave = now;
        savePosition();
      }).catch(function () {});
    }
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
    bindInteraction();
    loadFrames();
  });
})();
