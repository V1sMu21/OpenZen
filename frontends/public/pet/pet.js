/* 阿青 · 桌面小猫咪（插件式）— pet.js
   独立 webview：逐帧 canvas 播放、sse_event 状态映射、交叉淡化切换、localStorage 持久化 */
(function () {
  "use strict";

  const FPS = 10, FRAMES = 32, LOOP_SEC = FRAMES / FPS, FADE_MS = 300;
  // 无事件回落睡觉的时长：工具调用间隙常有 4-8s 静默，太短会让猫在
  // 任务中途睡着又惊醒（"转换混乱"观感的一部分），放宽到 9s；
  // done 态单独更短（DONE_LINGER_MS），庆祝完就休息。
  const IDLE_TIMEOUT_MS = 9000, DONE_LINGER_MS = 4000;
  const STATES = ["idle_sleep", "working", "thinking", "waiting", "error", "done", "petted", "walking", "dangle"];
  const STATE_TEXTS = {
    idle_sleep: ["休息中", "Idle"], working: ["执行中", "Working"],
    thinking: ["推理中", "Thinking"], waiting: ["等待中", "Waiting"],
    error: ["报错啦", "Error"], done: ["完成啦", "Done"],
    petted: ["呼噜呼噜", "Purring"], walking: ["散步中", "Walking"],
    dangle: ["喵呜…", "Carried"],
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

  // 帧兜底：某状态 webp 缺失（生成失败/被裁剪）时回落 idle_sleep，
  // 避免切换到空画布导致猫凭空消失。
  function hasFrames(st) {
    const a = frames[st] || [];
    return a.some(function (im) { return im && im.complete; });
  }

  function doSwitch(state) {
    if (state === active) return;
    if (!hasFrames(state)) state = "idle_sleep";
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
  // 强信号状态的最短展示时长：避免 error/waiting 等症状被下一个事件瞬间覆盖
  const MIN_HOLD_MS = { error: 2600, waiting: 1800, done: 1500 };
  let stateEnteredAt = 0;
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
    // 最短展示：当前状态的 MIN_HOLD 未走完前不允许被替换
    const hold = MIN_HOLD_MS[active] || 0;
    dueAt = Math.max(dueAt, stateEnteredAt + hold);
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
      stateEnteredAt = Date.now();
      doSwitch(s);
    }, Math.max(0, dueAt - now));
  }

  // ---------- 事件 → 状态 ----------
  function onEvent(evt) {
    const env = (evt && evt.payload) ? evt.payload : evt;
    if (!env || !env.event_type) return;
    if (previewing) { lastEventAt = Date.now(); return; }   // 预览中不被事件打断
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
    // 手势/预览进行中不回落（dangle 由鼠标松开收尾、petted 由松手收尾、
    // walking 由演出定时器收尾）——空闲计时绝不打断这些瞬态
    if (dragStarted || petHeld || previewing) return;
    if (active === "dangle" || active === "petted" || active === "walking") return;
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

  // ---------- 动画预览 ----------
  let previewing = false, previewTimer = null;
  function startPreview() {
    if (previewing) return;
    previewing = true;
    const seq = STATES.slice();
    let i = 0;
    const step = function () {
      if (i >= seq.length) {
        previewing = false;
        previewTimer = null;
        requestState("idle_sleep", true);
        return;
      }
      const st = seq[i++];
      const p = STATE_TEXTS[st] || STATE_TEXTS.idle_sleep;
      zhText.textContent = p[0]; enText.textContent = p[1];
      requestState(st, true);
      previewTimer = setTimeout(step, 1900);
    };
    step();
  }

  // ---------- 交互 ----------
  let tooltipTimer = null;
  // 拎起晃动（dangle）共享态：bindInteraction 设定/清除，boot 的 moved 监听刷新时间戳
  let danglePrev = null, lastMoveAt = 0;
  // 按住抚摸共享态：按住期间 petted 持续，mouseup 统一收尾
  let petHeld = false, petPrev = null;
  // 空闲散步：45s 无任何交互且处于休息态 → 走两步
  let lastInteractAt = Date.now(), lastStrollAt = 0;
  let justShown = true;   // 启动/隐藏后首次可见 → 走进场
  function bindInteraction() {
    // 悬停 → 浮签
    document.body.addEventListener("mouseenter", function () { tooltip.hidden = false; tickTip(); });
    document.body.addEventListener("mouseleave", function () { tooltip.hidden = true; });
    function tickTip() {
      if (tooltip.hidden) return;
      tooltip.textContent = (STATE_TEXTS[active][0]) + (todo.total ? " · 步骤 " + todo.current + "/" + todo.total : "") + " · 按住撸一撸 · 拖动搬家 · 双击回主窗 · 右键菜单";
      tooltipTimer = setTimeout(tickTip, 400);
    }

    // 改名对话框元素（被卡片/拖拽/点击等多处引用，必须先于事件注册定义）
    const renameBox = document.getElementById("renameBox");
    const renameInput = document.getElementById("renameInput");

    // 手势模型（互斥）：
    //   单击(<500ms 未移动)   → 状态卡开合
    //   按住(≥500ms 未移动)   → 抚摸（按住撸猫，亲密度+1）
    //   按住移动 >4px         → 拖拽（dangle 晃动）
    //   双击                  → 回主窗
    // mousemove 依赖 key window（非 key 时 macOS 不投递），按住抚摸只用
    // mousedown/up 即可触发，交互在任意焦点状态下都可靠。
    let holdTimer = null, heldForPet = false, downAt = 0;
    document.addEventListener("mousedown", function (e) {
      lastInteractAt = Date.now();
      if (e.button !== 0) return;
      heldForPet = false; downAt = Date.now();
      clearTimeout(holdTimer);
      if (e.target.closest("#menu, #card, #renameBox")) return;
      holdTimer = setTimeout(function () {
        if (dragStarted) return;
        heldForPet = true;
        petIt(true);      // 按住 0.6s = 撸猫，松手才结束
      }, 600);
    });
    document.addEventListener("mouseup", function (e) {
      clearTimeout(holdTimer);
      // 按住抚摸以"松开"为边界：松手才结束动画
      if (petHeld) {
        petHeld = false;
        if (active === "petted" && petPrev) requestState(petPrev);
        petPrev = null;
      }
      // 快速点击（未抚摸/未拖拽）= 状态卡开合
      if (!heldForPet && !dragStarted && Date.now() - downAt < 600 &&
          !(e.target.closest && e.target.closest("#menu, #card, #renameBox"))) {
        if (menu.hidden && renameBox.hidden) card.hidden = !card.hidden;
      }
    });
    document.addEventListener("dblclick", function () { card.hidden = true; restoreMain(); });

    // 拖拽：按住并移动 >4px → 调用原生 start_dragging（OS 级跟手）。
    // 不能用 data-tauri-drag-region：Tauri 的 drag 脚本对 mousedown 做
    // stopImmediatePropagation + 立即 start_dragging，会把单击状态卡、
    // 双击回主窗全部吃掉。阈值方案保留全部点击语义，移动即转为拖窗。
    // 菜单/状态卡等可交互元素不触发拖拽。
    let dsX = 0, dsY = 0, dsScreenX = 0, dsScreenY = 0;
    let dragArmed = false, dragStarted = false;
    let dragBase = null;   // { wx, wy } 窗口物理坐标基准（拖拽起点异步获取）
    document.addEventListener("mousedown", function (e) {
      if (e.button !== 0) return;
      if (e.target.closest("#menu, #card, #renameBox")) { dragArmed = false; return; }
      dsX = e.clientX; dsY = e.clientY;
      // 关键：位移必须基于"鼠标屏幕坐标"。clientX 是相对窗口的坐标，
      // 窗口一旦跟随移动，clientX 就恒定不变，位移会自我抵消（窗口纹丝不动）。
      dsScreenX = e.screenX; dsScreenY = e.screenY;
      dragArmed = true; dragStarted = false; dragBase = null;
    });
    document.addEventListener("mousemove", function (e) {
      if (!dragArmed || dragStarted) return;
      if (Math.hypot(e.screenX - dsScreenX, e.screenY - dsScreenY) > 4) {
        dragStarted = true; dragArmed = false;
        clearTimeout(holdTimer);   // 已判定为拖拽：不开卡、不抚摸
        document.body.classList.add("dragging");
        danglePrev = (active === "dangle" || active === "petted") ? "idle_sleep" : active;
        petHeld = false; petPrev = null;   // 转拖拽后抚摸不再持有
        lastMoveAt = Date.now();
        requestState("dangle", true);
        // 手动跟随：基准 = 拖拽起点的窗口物理坐标（异步取，此时窗口尚未移动）。
        const s = getSelf();
        if (s && typeof s.outerPosition === "function") {
          s.outerPosition().then(function (p) {
            dragBase = { wx: p.x, wy: p.y, dpr: window.devicePixelRatio || 1 };
          }).catch(function () {});
        } else {
          invokeTauri("plugin:window|outer_position", { label: "pet" }).then(function (p) {
            dragBase = { wx: p.x, wy: p.y, dpr: window.devicePixelRatio || 1 };
          }).catch(function () {});
        }
      }
    });
    function followDrag(e) {
      if (!dragBase) return;
      lastMoveAt = Date.now();
      invokeTauri("plugin:window|set_position", {
        label: "pet",
        value: { Physical: {
          x: Math.round(dragBase.wx + (e.screenX - dsScreenX) * dragBase.dpr),
          y: Math.round(dragBase.wy + (e.screenY - dsScreenY) * dragBase.dpr),
        } },
      }).catch(function () {});
    }
    document.addEventListener("mousemove", function (e) {
      if (!dragStarted) return;
      // 丢失 mouseup 的检测：按键已在窗口外松开（后续移动事件 buttons=0）
      if (e.buttons === 0) { endDrag(); return; }
      followDrag(e);
    });
    function endDrag() {
      if (!dragStarted) return;
      dragStarted = false; dragArmed = false;
      document.body.classList.remove("dragging");
      savePosition();
      if (danglePrev !== null) { requestState(danglePrev); danglePrev = null; }
    }
    document.addEventListener("mouseup", function () { endDrag(); });
    // 兜底安全网：mouseup 与 buttons 检测都失效时（极罕见），拖拽态超过 6s
    // 无任何事件才回收——不影响"按住不动"的正常拖拽停顿。
    setInterval(function () {
      if (active === "dangle" && danglePrev !== null && Date.now() - lastMoveAt > 6000) {
        endDrag();
      }
    }, 1000);

    // 抚摸：指针悬停在猫身上快速左右划 → 呼噜 + affinity。
    // 按住划动已被原生拖拽接管（OS 吞掉后续 mousemove），因此抚摸
    // 改为"未按下"的悬停划动；500ms 无动作自动复位计数。
    let wig = 0, wigAt = 0, lastPX = null;
    document.addEventListener("mousemove", function (e) {
      if (e.buttons) { wig = 0; lastPX = null; return; }   // 按下 = 拖拽/点击，不算抚摸
      const dx = lastPX === null ? 0 : e.clientX - lastPX;
      lastPX = e.clientX;
      if (Math.abs(dx) > 5) { wig++; wigAt = Date.now(); if (wig > 3) petIt(false); }
    });
    setInterval(function () { if (Date.now() - wigAt > 500) wig = 0; }, 500);

    // 右键菜单
    document.addEventListener("contextmenu", function (e) { e.preventDefault(); lastInteractAt = Date.now(); menu.hidden = !menu.hidden; });
    document.addEventListener("click", function (e) {
      if (!menu.hidden && !e.target.closest("#menu")) menu.hidden = true;
      if (!renameBox.hidden && !e.target.closest("#renameBox")) closeRename(false);
    });
    menu.addEventListener("click", function (e) {
      const act = e.target.dataset && e.target.dataset.act; if (!act) return; menu.hidden = true;
      if (act === "back") restoreMain();
      else if (act === "preview") startPreview();
      else if (act === "rename") openRename();
      else if (act === "quit") quit();
    });

    // 改名：WKWebView 没有原生 prompt()（静默返回 undefined，点击无反应的
    // 根因），用页面内对话框替代。打开时把宠物窗设为 key window——
    // 否则输入框拿不到键盘事件；成功后弹状态卡展示新名字。
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
  // held=true：按住抚摸——动画持续到松开鼠标（mouseup 统一收尾）；
  // held=false：悬停划动——无"松开"边界，展示 2.2s 后自动回收。
  // 冷却期内也播放动画（只不计数），保证每次抚摸都有反馈。
  function petIt(held) {
    const now = Date.now();
    if (now - pet.affinity.lastPetAt >= 10000) {
      pet.affinity.points++; pet.affinity.pets++; pet.affinity.lastPetAt = now;
      pet.soul.mood = "开心"; save(); updateCard();
    }
    const prev = active === "petted" ? "idle_sleep" : active;
    requestState("petted", true);
    if (held) {
      petHeld = true; petPrev = prev;
    } else {
      setTimeout(function () {
        if (active === "petted" && Date.now() - lastEventAt > 1500) requestState(prev);
      }, 2200);
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
  // 可见性轮询（visibilitychange 在 WKWebView 偶发不派发，用轮询兜底）：
  // ① 变为可见（seal 召唤）→ 走进场；② 45s 无交互且休息中 → 空闲散步
  let wasVisible = !document.hidden;
  setInterval(function () {
    const vis = !document.hidden;
    if (vis !== wasVisible) wasVisible = vis;
    if (!vis) return;
    if (dragStarted || petHeld || previewing) return;
    if (active !== "idle_sleep") return;
    const now = Date.now();
    if (justShown) {                       // 刚被召唤
      justShown = false;
      lastInteractAt = now;
      playWalk(1300);
      return;
    }
    if (now - lastInteractAt > 45000 && now - lastStrollAt > 45000) {
      lastStrollAt = now;
      playWalk(3400);
    }
  }, 1000);

  // 走路演出（入场/散步：仅播放不滑出）
  function playWalk(ms) {
    requestState("walking", true);
    setTimeout(function () {
      if (active === "walking") requestState("idle_sleep");
    }, ms || 3200);
  }
  // 走路退场：开走 + 向右滑出淡出（约 0.95s）。who hides the window:
  // 页内主动隐藏（双击/右键退出）时由本函数收尾；主窗 seal 收起时
  // 主窗发 pet-walkout 事件并自行隐藏，本函数只负责演出。
  function walkOut() {
    requestState("walking", true);
    setTimeout(function () {
      [cvA, cvB].forEach(function (cv) {
        cv.style.transition = "transform 0.95s ease-in, opacity 0.95s ease-in";
        cv.style.transform = "translate(150px, -50%) scale(0.30)";
        cv.style.opacity = "0";
      });
    }, 60);
    setTimeout(function () {
      [cvA, cvB].forEach(function (cv) {
        cv.style.transition = ""; cv.style.transform = ""; cv.style.opacity = "";
      });
      requestState("idle_sleep", true);
    }, 1500);
  }
  function hideSelf() {
    savePosition();
    pet.display.visible = false; save();
    walkOut();
    setTimeout(function () { hideWindowAnyWay(); }, 1010);
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
        lastMoveAt = Date.now();   // dangle 态心跳：OS 拖拽期间窗口持续位移
        const now = Date.now();
        if (now - lastMoveSave < 2000) return;
        lastMoveSave = now;
        savePosition();
      }).catch(function () {});
    }
    updateText(); updateCard();

    if (globalThis.__TAURI__ && globalThis.__TAURI__.event) {
      globalThis.__TAURI__.event.listen("sse_event", onEvent).catch(function () {});
      // 主窗 seal 收起：先让猫走掉再隐藏（主窗在 ~1.15s 后执行 hide）
      globalThis.__TAURI__.event.listen("pet-walkout", function () { walkOut(); }).catch(function () {});
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


  var inited = false;
  function init() {
    if (inited) return;
    inited = true;
    boot();
    bindInteraction();
    loadFrames();
  }
  // 三重兜底初始化：脚本在 body 尾部同步执行，正常路径 readyState 已是
  // "interactive" 直接 init；若 WebKit 把文档停在 loading 态（DCL 永不
  // 触发的观测案例），3s 定时器保证宠物功能最终可用。
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
    setTimeout(init, 3000);
  } else {
    init();
  }
})();
