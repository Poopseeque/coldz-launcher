// COLDZ launcher UI. All real work (FiveM, network, updates) is done in Rust
// through invoke(); this file only draws the screen.
// Opened in a normal browser (no Tauri), it runs on preview data so the look can be checked.
(function () {
  const T = window.__TAURI__ || previewTauri();
  const invoke = T.core.invoke;
  const appWindow = T.window.getCurrentWindow();
  const $ = (id) => document.getElementById(id);

  const STATUS_EVERY_MS = 20000;
  const FIVEM_POLL_MS = 5000;   // while FiveM is missing: notice the install by itself
  const LANGS = ["ru", "en", "es", "pt-BR"];

  let lang = "ru";
  let cfg = null;
  let fivemInstalled = false;
  let newsData = null;
  let newsSource = "bundled";
  let fivemMessage = null;   // { key, until }
  let busy = false;
  let address = "";          // address in use ("" = not published yet)
  let route = null;          // 0.3.5: "main" | "reserve" | "none" | null (src-tauri/src/route.rs)
  let tab = "news";
  let serverInfo = null;     // 0.5.0: { restartIn, at } (server_info, once a minute); 0.5.3: online graph removed
  const INFO_EVERY_MS = 60000;
  // 0.5.8: the launcher lives in the tray for days; news and launcher updates were asked only at the start, so a tray copy
  // never saw a new release. Asked again by themselves and when the window is opened.
  const NEWS_EVERY_MS = 30 * 60 * 1000;
  const NEWS_ON_FOCUS_MS = 10 * 60 * 1000;
  const UPDATE_EVERY_MS = 3 * 60 * 60 * 1000;
  const RESTART_WARN_S = 120;  // «Играть» asks this long before the restart
  let crashHint = null;      // 0.5.0: { key, detail, gb, at } after a fresh FiveM crash (src-tauri/src/support.rs)

  // ------------------------------------------------------------ language
  function storageGet(k) { try { return localStorage.getItem(k); } catch { return null; } }
  function storageSet(k, v) { try { localStorage.setItem(k, v); } catch { /* ignore */ } }

  function detectLang() {
    const saved = storageGet("lang");
    if (LANGS.includes(saved)) return saved;
    const nav = (navigator.language || "ru").toLowerCase();
    if (/^(ru|uk|be|kk)/.test(nav)) return "ru";
    if (nav.startsWith("es")) return "es";
    if (nav.startsWith("pt")) return "pt-BR";
    return "en";
  }

  function t(key, vars) {
    let s = (I18N[lang] && I18N[lang][key]) || I18N.ru[key] || key;
    if (vars) for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, v);
    return s;
  }

  function applyLang() {
    document.documentElement.lang = lang;
    $("lang").value = lang;
    document.querySelectorAll("[data-i18n]").forEach((el) => { el.textContent = t(el.dataset.i18n); });
    document.querySelectorAll("[data-i18n-title]").forEach((el) => { el.title = t(el.dataset.i18nTitle); });
    document.querySelectorAll("[data-i18n-placeholder]").forEach((el) => { el.placeholder = t(el.dataset.i18nPlaceholder); });
    renderNick();
    renderStatusText();
    renderAddress();
    renderFivem();
    renderNews();
    renderRules();
    renderNet();
    renderReport();
    renderSettings();
    renderRestart();
    renderUpdate();
    if (cfg) renderLinks();   // 20.11: Instagram button follows the window language
  }

  // ------------------------------------------------------------ status + address
  function renderStatusText() {
    const state = $("status").dataset.state;
    const key = { online: "online", offline: "offline", unset: "unset" }[state] || "checking";
    $("status-text").textContent = t(key);
  }

  function renderAddress() {
    const text = $("address-text");
    text.replaceChildren();
    if (address) {
      text.append(t("address") + " ", el("b", null, address));
    } else {
      text.textContent = t("address_unknown");
    }
    $("btn-copy").hidden = !address;
    $("btn-copy").textContent = t("copy");
    // 0.3.5: which route «Играть» uses (main server / reserve route); nothing while not chosen or nothing answered
    const shown = address && (route === "main" || route === "reserve");
    $("route-text").hidden = !shown;
    $("route-text").dataset.route = shown ? route : "";
    $("route-text").textContent = shown ? t("route_" + route) : "";
  }

  async function refreshStatus() {
    try {
      const s = await invoke("server_status");
      address = s.address || "";
      route = s.route || null;
      $("status").dataset.state = !address ? "unset" : s.online ? "online" : "offline";
      $("players").textContent = s.online ? s.players : "—";
      $("max-players").textContent = s.maxPlayers || (cfg && cfg.maxPlayers) || 48;
      $("ping").textContent = s.online && s.pingMs != null ? s.pingMs : "—";
    } catch (e) {
      $("status").dataset.state = "offline";
    }
    renderStatusText();
    renderAddress();
    renderFivem();
  }

  async function copyAddress() {
    if (!address) return;
    let ok = false;
    try { await navigator.clipboard.writeText(address); ok = true; } catch { /* fallback below */ }
    if (!ok) {
      const area = el("textarea");
      area.value = address;
      document.body.append(area);
      area.select();
      try { ok = document.execCommand("copy"); } catch { ok = false; }
      area.remove();
    }
    if (ok) {
      $("btn-copy").textContent = t("copied");
      setTimeout(() => { $("btn-copy").textContent = t("copy"); }, 1500);
    }
  }

  // ------------------------------------------------------------ nickname (13.11)
  // The nick on the server: sent to our server on "Play" (src-tauri/src/nick.rs), empty = the FiveM name.
  // Same rules as the server (winter-server z_menu/shared/nick.lua) and nick.rs: Latin and Cyrillic letters, digits,
  // space, - and _; at least one letter or digit; up to 40 bytes. Staff words ("admin"...) are checked by the server.
  const NICK_MAX_BYTES = 40;
  const NICK_ALLOWED = /^[0-9A-Za-z \-_А-яЁё]+$/;
  const NICK_ALNUM = /[0-9A-Za-zА-яЁё]/;
  const NICK_REASONS = ["chars", "long", "reserved", "often"];
  let nickNote = null;       // the server's answer under the field: { key, vars, state }

  function nickCheck(raw) {
    const nick = String(raw || "").replace(/\s+/g, " ").trim();
    if (!nick) return { nick };
    if (!NICK_ALLOWED.test(nick) || !NICK_ALNUM.test(nick)) return { error: "chars" };
    if (new TextEncoder().encode(nick).length > NICK_MAX_BYTES) return { error: "long" };
    return { nick };
  }

  function nickInvalid() { return !!nickCheck($("nick").value).error; }

  function renderNick() {
    const box = $("nick-box");
    const hint = $("nick-hint");
    const c = nickCheck($("nick").value);
    if (c.error) {
      box.dataset.state = "bad";
      hint.textContent = t("nick_" + c.error);
    } else if (nickNote) {
      box.dataset.state = nickNote.state;
      hint.textContent = t(nickNote.key, nickNote.vars);
    } else {
      box.dataset.state = "";
      hint.textContent = t(c.nick ? "nick_hint" : "nick_hint_empty");
    }
  }

  // Tells the server the nick. false — the server said no (the reason is shown under the field): FiveM is not started.
  // No answer (old server, no internet) — FiveM starts anyway with the nick the player already had.
  async function sendNick(nick) {
    let res;
    try {
      res = await invoke("send_nick", { nick });
    } catch (err) {
      nickNote = { key: err === "long" ? "nick_long" : "nick_chars", state: "bad" };
      return false;
    }
    if (res && res.status === "rejected") {
      nickNote = { key: "nick_" + (NICK_REASONS.includes(res.reason) ? res.reason : "bad"), state: "bad" };
      return false;
    }
    nickNote = res && res.status === "ok"
      ? { key: res.nick ? "nick_sent" : "nick_sent_fivem", vars: { n: res.nick }, state: "ok" }
      : { key: "nick_unreachable", state: "" };
    return true;
  }

  function onNickInput() {
    nickNote = null;
    renderNick();
    renderFivem();
  }

  // ------------------------------------------------------------ FiveM + play
  function renderFivem() {
    const box = $("fivem");
    const text = $("fivem-text");
    const actions = $("fivem-actions");
    $("play").disabled = busy || !fivemInstalled || !address || nickInvalid();
    const showing = !!(fivemMessage && Date.now() < fivemMessage.until);
    // «Закрыть FiveM» — только пока висит сообщение «FiveM завис» (stuck)
    $("fivem-stuck").hidden = !(showing && fivemMessage.stuck);
    $("fivem-net").hidden = true;
    $("fivem-hint").hidden = true;
    $("fivem-storage").hidden = true;
    $("storage-bar").hidden = true;
    delete box.dataset.step;
    // 0.5.7: «Играть» connects through the COLDZ server: one line per step («Windows спросит разрешение» before it asks)
    if (viaStep) {
      box.dataset.state = "via";
      box.dataset.step = viaStep;
      text.textContent = t("via_" + viaStep);
      actions.hidden = true;
      return;
    }
    // 0.5.4: the game file for the server is being downloaded (or stopped): that line and its buttons
    if (storageUi) {
      const s = storageUi;
      box.dataset.state = s.running || s.error === "cancelled" ? "message" : "net";
      text.textContent = storageText();
      actions.hidden = true;
      $("fivem-storage").hidden = false;
      $("btn-storage-cancel").hidden = !s.running;
      $("btn-storage-skip").hidden = !!s.running;
      $("storage-bar").hidden = !s.running;
      $("storage-bar-fill").style.width = (s.total ? Math.min(100, (s.done / s.total) * 100) : 0).toFixed(1) + "%";
      return;
    }
    if (showing) {
      box.dataset.state = "message";
      text.textContent = t(fivemMessage.key, { a: address || "—" }) + (fivemMessage.also ? " " + t(fivemMessage.also) : "");
      actions.hidden = true;
      return;
    }
    fivemMessage = null;
    if (box.dataset.state === "checking" && !cfg) {
      text.textContent = t("fivem_checking");
      return;
    }
    // 0.3.4: the last connection check says FiveM will not start or join: that line instead of «можно играть».
    // «Играть» stays enabled (0.5.7: it fixes what it can by itself, the rest is one card).
    const advice = fivemInstalled ? netAdvice(net.report) : null;
    if (advice) {
      box.dataset.state = net.running ? "netcheck" : net.report.plan === "via" ? "via" : "net";
      text.textContent = t(net.running ? "net_checking_short" : advice);
      actions.hidden = true;
      // 0.5.7: «Играть» takes care of it itself: no links, so the line fits together with the update notice
      $("fivem-net").hidden = net.report.plan === "via";
      $("btn-net-retry").disabled = !!net.running;
      return;
    }
    // 0.5.0: after a fresh FiveM crash, one simple line about the likely cause (until the player hides it)
    const hint = fivemInstalled ? visibleHint() : null;
    if (hint) {
      box.dataset.state = "hint";
      text.textContent = hintText(hint);
      actions.hidden = true;
      $("fivem-hint").hidden = false;
      return;
    }
    box.dataset.state = fivemInstalled ? "ok" : "missing";
    text.textContent = t(fivemInstalled ? "fivem_found" : "fivem_missing");
    actions.hidden = fivemInstalled;
  }

  // ------------------------------------------------------------ game file for the server (0.5.4, storage.rs)
  // FiveM's override update.rpf (1.4 GB for a server on 3258 with a fresh GTA): downloaded before FiveM starts, so FiveM
  // does not download it inside the game. null | { done, total, phase: "download"|"check", running, error }
  let storageUi = null;
  const STORAGE_ERRORS = { cancelled: "storage_paused", network: "storage_network", remote: "storage_remote", bad: "storage_bad", space: "storage_space", disk: "storage_disk" };
  function storageText() {
    const s = storageUi;
    const v = { d: Math.round(s.done / 1e6), t: Math.round(s.total / 1e6), g: Math.max(1, Math.ceil((s.total - s.done) / 1e9 + 0.5)) };
    if (s.running) return t(s.phase === "check" ? "storage_checking" : "storage_progress", v);
    return t(STORAGE_ERRORS[s.error] || "storage_network", v);
  }
  // true: the file is there now (or nothing to do any more): go on to FiveM
  async function downloadStorage(need) {
    storageUi = { done: need.have || 0, total: need.total, phase: "download", running: true, error: null };
    renderFivem();
    try {
      await invoke("storage_download");
      storageUi = null;
      renderFivem();
      return true;
    } catch (err) {
      if (err === "none") {
        storageUi = null;
        return true;
      }
      storageUi = Object.assign({}, storageUi, { running: false, error: String(err) });
      renderFivem();
      return false;
    }
  }
  function onStorageProgress(p) {
    if (!storageUi || !storageUi.running || !p) return;
    Object.assign(storageUi, p);
    renderFivem();
  }

  // ------------------------------------------------------------ crash hint + support report (0.5.0)
  const hintId = (h) => h.key + ":" + h.at;
  function visibleHint() {
    return crashHint && storageGet("hintHidden") !== hintId(crashHint) ? crashHint : null;
  }
  function hintText(h) {
    return t("hint_" + h.key, { f: h.detail ? " (" + h.detail + ")" : "", d: h.detail, g: h.gb, p: h.detail });
  }
  function hideHint() {
    if (crashHint) storageSet("hintHidden", hintId(crashHint));
    renderFivem();
  }
  async function loadHint() {
    try { crashHint = (await invoke("crash_hint")) || null; } catch { crashHint = null; }
    renderFivem();
  }

  function formatSize(n) {
    if (n >= 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + " " + t("unit_mb");
    if (n >= 1024) return Math.round(n / 1024) + " " + t("unit_kb");
    return n + " " + t("unit_b");
  }

  // ------------------------------------------------------------ restart notice (0.5.0; online graph removed in 0.5.3)
  // Seconds left until the announced restart, counted from when the server answered. null: none announced.
  function restartLeft() {
    if (!serverInfo || serverInfo.restartIn == null) return null;
    const left = serverInfo.restartIn - (Date.now() - serverInfo.at) / 1000;
    return left > -60 ? left : null;
  }
  function renderRestart() {
    const left = restartLeft();
    const shown = left != null && left <= 3600;
    const line = $("restart-text");
    line.hidden = !shown;
    if (shown) line.textContent = left < 60 ? t("restart_now") : t("restart_in", { n: Math.ceil(left / 60) });
    if (!$("restart").hidden) {
      $("restart-card-text").textContent = left != null && left < 60 ? t("restart_text_now") : t("restart_text", { n: Math.max(1, Math.ceil((left || 0) / 60)) });
    }
  }
  async function refreshInfo() {
    try {
      const i = await invoke("server_info");
      serverInfo = i ? Object.assign({}, i, { at: Date.now() }) : null;
    } catch { serverInfo = null; }
    renderRestart();
  }
  function openRestart() {
    $("restart").hidden = false;
    renderRestart();
    $("btn-restart-close").focus();
  }
  function formatTime(unix) {
    try { return new Intl.DateTimeFormat(lang, { day: "numeric", month: "long", hour: "2-digit", minute: "2-digit" }).format(new Date(unix * 1000)); }
    catch { return ""; }
  }

  const report = { plan: null, busy: false, result: null };
  async function openReport() {
    $("settings").hidden = true;
    $("report").hidden = false;
    report.plan = null;
    report.result = null;
    $("report-dump").checked = false;
    renderReport();
    $("btn-report-close").focus();
    try { report.plan = await invoke("support_preview"); } catch (e) { console.warn("report:", e); report.plan = { files: [], dump: null, dumpsToday: 0 }; }
    renderReport();
  }
  function renderReport() {
    if ($("report").hidden) return;
    const list = $("report-list");
    list.replaceChildren();
    const p = report.plan;
    if (!p) {
      const li = el("li");
      li.dataset.state = "wait";
      li.append(el("span", null, t("report_loading")), el("span", "netlist__state", ""));
      list.append(li);
    } else {
      for (const f of p.files || []) {
        const li = el("li");
        li.dataset.state = "ok";
        const system = f.name === "system.txt";
        li.append(el("span", null, system ? f.name + ": " + t("report_system") : f.name), el("span", "netlist__state", system ? "" : formatSize(f.size)));
        list.append(li);
      }
    }
    const dump = p && p.dump;
    $("report-dump-box").hidden = !dump;
    if (dump) $("report-dump-text").textContent = t("report_dump", { s: formatSize(dump.size), d: formatTime(dump.at) });
    $("report-dumps").hidden = !(p && p.dumpsToday > 0);
    if (p) $("report-dumps").textContent = t("report_dumps", { n: p.dumpsToday });
    const r = report.result;
    $("report-result").hidden = !r;
    if (r) {
      $("report-result").dataset.state = r.ok ? "ok" : "bad";
      $("report-result").textContent = r.ok ? t("report_done", { n: r.name }) : t("report_failed");
    }
    const make = $("btn-report-make");
    make.disabled = !p || report.busy;
    make.textContent = t(report.busy ? "report_making" : "report_make");
  }
  async function makeReport() {
    if (report.busy || !report.plan) return;
    report.busy = true;
    renderReport();
    try {
      const done = await invoke("support_create", { withDump: $("report-dump").checked });
      report.result = { ok: true, name: done.name };
    } catch (e) {
      console.warn("report:", e);
      report.result = { ok: false };
    }
    report.busy = false;
    renderReport();
  }

  function openSettings() {
    $("settings").hidden = false;
    renderSettings();
    $("btn-settings-close").focus();
  }
  function renderSettings() {
    if ($("settings").hidden) return;
    $("pref-tray").checked = !!prefs.tray;
  }
  // 0.5.0: «Сворачивать в трей»; the tray menu speaks the window's language
  let prefs = { tray: false };
  async function setTray(on) {
    try { prefs = (await invoke("set_tray", { on, open: t("tray_open"), quit: t("tray_quit") })) || prefs; } catch (e) { console.warn("tray:", e); }
    renderSettings();
  }
  function minimizeWindow() {
    invoke("minimize_window").catch(() => appWindow.minimize().catch(() => {}));
  }

  async function refreshFivem() {
    try {
      const info = await invoke("detect_fivem");
      fivemInstalled = !!info.installed;
    } catch {
      fivemInstalled = false;
    }
    renderFivem();
  }

  // «Choose FiveM.exe manually» (0.3.2): the Windows file dialog in Rust (src-tauri/src/fivem.rs). null — closed without
  // a choice; "not_fivem" — the file is not FiveM.exe (nothing saved); otherwise saved, FiveM found
  async function pickFivem() {
    const btn = $("btn-pick-fivem");
    btn.disabled = true;
    try {
      const info = await invoke("pick_fivem", { title: t("pick_fivem_title") });
      if (info && info.installed) {
        fivemInstalled = true;
        showFivemMessage("fivem_picked", 6000);
      }
    } catch (err) {
      if (err === "not_fivem") showFivemMessage("pick_fivem_wrong", 12000);
      else console.warn("pick FiveM:", err);
    } finally {
      btn.disabled = false;
    }
  }

  // stuck — with the «Close FiveM» button (FiveM processes without a game window, src-tauri/src/running.rs)
  // also — one more line after it (a text key)
  function showFivemMessage(key, ms, stuck, also) {
    fivemMessage = { key, until: Date.now() + ms, stuck: !!stuck, also: also || null };
    renderFivem();
    setTimeout(renderFivem, ms + 50);
  }

  // «Close FiveM»: ends every FiveM process; [closed, failed] — failed: FiveM runs as administrator
  async function closeFivem() {
    const btn = $("btn-close-fivem");
    btn.disabled = true;
    let res = null;
    try { res = await invoke("close_fivem"); } catch (e) { console.warn("close FiveM:", e); }
    btn.disabled = false;
    const failed = Array.isArray(res) ? Number(res[1]) || 0 : 1;
    if (failed > 0) showFivemMessage("fivem_close_failed", 120000, true);
    else showFivemMessage("fivem_closed", 15000);
  }

  // ------------------------------------------------------------ connection check (15.11)
  // Before «Играть», on «Проверить связь» and (0.3.4) once at launcher start: the Cfx.re services FiveM needs, the
  // COLDZ server separately (src-tauri/src/netcheck.rs). A provider that cuts Cfx.re makes FiveM hang or fail at start,
  // and players blame COLDZ: the window says whose problem it is. Advice: only another network, the internet, the
  // router, later. Never anything else (README 7б). 0.5.7 (Alexander 23.11): no questions, «Играть» decides by itself; a
  // card only when it cannot, with one text and one button.
  const NET_SERVICES = ["update", "auth", "policy", "services", "list"];
  const NET_FRESH_MS = 60000;   // «Играть» does not repeat a check this fresh that lets it go on
  // report: the last finished check (kept while a new one runs), null — none yet or the check itself failed
  // fromPlay: the card came up on «Играть»; error: what went wrong there (a text key); more: «Подробнее» is open
  const net = { report: null, at: 0, running: null, fromPlay: false, error: null, more: false };

  // 0.5.7: the check itself says what «Играть» does (src-tauri/src/netcheck.rs `plan`): "go" and "via" (the sign-in through
  // the COLDZ server) need no questions, the rest is one card: offline, server, noroute, via_bad, cfx, proxy.
  const NET_GOES = ["go", "via"];
  function netStops(r) {
    return !!r && !NET_GOES.includes(r.plan);
  }
  // where «Играть» is with the sign-in through the COLDZ server: null | "connect" | "prompt" | "cleanup"
  // (src-tauri/src/lib.rs `signin-step`)
  let viaStep = null;
  const VIA_ERRORS = { cancelled: "via_cancelled", port_busy: "via_port_busy", no_route: "via_no_route", no_setup: "via_no_route" };
  function onSigninStep(step) {
    viaStep = ["connect", "prompt", "cleanup"].includes(step) ? step : null;
    renderFivem();
  }

  // The line under «Играть» (0.3.4): only what stops FiveM itself, or (0.5.7) what «Играть» is going to do. The COLDZ server
  // being down is already in the status above and comes up in the card on «Играть».
  const NET_ADVICE = { offline: "net_offline", cfx: "net_cfx", via: "net_via_short", noroute: "net_noroute_short", via_bad: "via_bad", proxy: "net_proxy_short" };
  function netAdvice(r) {
    return (r && NET_ADVICE[r.plan]) || null;
  }

  // 0.5.7: the sign-in through the COLDZ server (src-tauri/src/signin.rs), by itself on «Играть»: what is going on under the
  // button, the Windows prompt comes up once. null: it is on; otherwise the key of the text that says what went wrong.
  async function connectVia() {
    onSigninStep("connect");
    try {
      await invoke("signin_enable");
      if (net.report && net.report.signin) {
        net.report.signin.active = true;
        net.report.signin.blocked = false;
        net.report.plan = "go";
      }
      return null;
    } catch (e) {
      return VIA_ERRORS[String(e || "")] || "via_failed";
    } finally {
      onSigninStep(null);
    }
  }

  // «Выключить вход через сервер COLDZ» under «Подробнее»: usually without a Windows prompt (the elevated copy does it); when
  // there will be one, the line says so
  let viaStopping = null;
  async function turnViaOff() {
    if (viaStopping || net.running) return;
    viaStopping = "via_stopping";
    net.error = null;
    renderNet();
    const now = await invoke("signin_status").catch(() => null);
    if (now && now.active && !now.quietOff) {
      viaStopping = "via_stopping_ask";
      renderNet();
      await new Promise((r) => setTimeout(r, 1800));
    }
    try { await invoke("signin_disable"); } catch { net.error = "via_off_failed"; }
    viaStopping = null;
    renderNet();
    runNetCheck();
  }

  function runNetCheck() {
    if (net.running) return net.running;
    net.running = invoke("check_network")
      .catch((e) => { console.warn("net check:", e); return null; })
      .then((r) => { net.report = r || null; net.at = Date.now(); return net.report; })
      // 0.3.5: the check also chose the route (main server or reserve route): status, address and the route line
      .finally(() => { net.running = null; renderNet(); renderFivem(); refreshStatus(); });
    renderNet();
    renderFivem();
    return net.running;
  }

  function openNet(fromPlay, error) {
    net.fromPlay = !!fromPlay;
    net.error = error || null;
    net.more = false;
    $("net").hidden = false;
    renderNet();
    (net.fromPlay ? $("btn-net-main") : $("btn-net-close")).focus();
  }

  function closeNet() {
    $("net").hidden = true;
    net.error = null;
  }

  function renderNet() {
    const box = $("net");
    if (box.hidden) return;
    const r = net.report;
    const running = !!net.running;
    const list = $("net-list");
    list.replaceChildren();
    const line = (name, state, word, cls) => {
      const li = el("li", cls);
      li.dataset.state = state;
      li.append(el("span", null, name), el("span", "netlist__state", word));
      list.append(li);
    };
    const byId = {};
    for (const s of (r && r.services) || []) byId[s.id] = s;
    for (const id of NET_SERVICES) {
      const s = byId[id];
      if (running || !s) line(t("svc_" + id), "wait", "");
      else line(t("svc_" + id), s.ok ? "ok" : "fail", t(s.ok ? "net_st_ok" : s.problem === "slow" ? "net_st_slow" : "net_st_fail"));
    }
    // 0.5.0: the ping to the main server and to every reserve route
    const pings = (r && Array.isArray(r.pings)) ? r.pings : [];
    const ms = (p) => (p && p.ms != null ? " · " + t("net_ms", { n: p.ms }) : "");
    const mainPing = pings.find((p) => p.kind === "main");
    if (running || !r) line(t("svc_server"), "wait", "", "netlist__server");
    else {
      const sv = r.server === true ? "ok" : r.server === false ? "fail" : "none";
      line(t("svc_server"), sv, t({ ok: "net_server_ok", fail: "net_server_fail", none: "net_server_none" }[sv]) + (sv === "ok" ? ms(mainPing) : ""), "netlist__server");
    }
    // 0.3.5: the reserve routes to COLDZ (src-tauri/src/route.rs); no line when there are none. 0.5.0: one line per
    // route with its ping when the check measured them
    const reservePings = pings.filter((p) => p.kind === "reserve");
    if (r && r.reserve != null && reservePings.length && !running) {
      for (const p of reservePings) {
        line(t("svc_reserve_n", { n: p.index }), p.ms != null ? "ok" : "fail", p.ms != null ? t("net_ms", { n: p.ms }) : t("net_reserve_fail"));
      }
    } else if (r && r.reserve != null) {
      if (running) line(t("svc_reserve"), "wait", "");
      else line(t("svc_reserve"), r.reserve ? "ok" : "fail", t(r.reserve ? "net_reserve_ok" : "net_reserve_fail"));
    }
    // 0.4.0: the FiveM sign-in and the way to it through the COLDZ server (src-tauri/src/signin.rs); no lines when
    // news.json has no sign-in setup
    const si = r && r.signin;
    if (si) {
      si.groups.forEach((g, i) => {
        const name = t(g.id === "cfx" ? "svc_signin_cfx" : "svc_signin");
        if (running) line(name, "wait", "", i === 0 ? "netlist__server" : null);
        else line(name, g.ok ? "ok" : "fail", t(g.ok ? (si.active ? "net_route_on" : "net_st_ok") : "net_st_fail"), i === 0 ? "netlist__server" : null);
      });
      if (si.tunnel) {
        if (running) line(t("svc_via"), "wait", "");
        else if (si.active) line(t("svc_via"), "ok", t("net_route_on"));
        else line(t("svc_via"), si.tunnel.ok ? "ok" : "fail", t(si.tunnel.ok ? "net_route_ok" : "net_route_fail"));
      }
    }

    // one text: what went wrong on «Играть», or what the check found
    const TEXT = { offline: "net_offline", server: "net_server_down", cfx: "net_cfx", noroute: "via_no_route", via_bad: "via_bad", proxy: "net_proxy", via: "net_via" };
    let text;
    let hint = "";
    let state = "bad";
    if (running) {
      text = t("net_checking");
      state = "checking";
    } else if (net.error) {
      text = t(net.error);
    } else if (!r) {
      text = t("net_failed");
    } else if (TEXT[r.plan]) {
      text = t(TEXT[r.plan]);
      if (r.plan === "offline") hint = t("net_advice");
      if (r.plan === "via") state = "ok";
    } else {
      // "go"
      state = "ok";
      if (r.signin && r.signin.active) text = t("net_via_on");
      else if (r.verdict === "ok") text = t(r.server === true ? "net_ok" : r.reserve === true ? "net_ok_reserve" : "net_ok_fivem");
      else text = t("net_partial");      // only the server list or Cfx.re services: FiveM starts and joins COLDZ
      if (r.proxy) hint = t("net_proxy");
    }
    box.dataset.state = state;
    $("net-text").textContent = text;
    $("net-hint").textContent = hint;
    $("net-hint").hidden = !hint;

    // 0.5.7: one main button. «Проверить связь»: «Проверить ещё раз». On «Играть»: «Играть» once it can go on (or the player
    // has switched the VPN off), «Попробовать ещё раз» where the launcher itself failed, else «Проверить ещё раз».
    const main = $("btn-net-main");
    const step = netMainStep(r);
    main.disabled = running || !!viaStopping;
    main.textContent = t({ play: "play", again: "try_again", check: "net_recheck" }[step]);
    const without = $("btn-net-without");
    without.hidden = !(net.fromPlay && !running && (net.error || !r || netStops(r)));
    $("btn-net-more").textContent = t(net.more ? "net_details_hide" : "net_details");
    $("btn-net-more").setAttribute("aria-expanded", String(net.more));
    $("net-more").hidden = !net.more;
    $("net-via-note").hidden = !(r && r.signin && r.signin.tunnel);
    $("net-via-off").hidden = !(r && r.signin && r.signin.active && !running);
    $("btn-via-off").disabled = !!viaStopping;
    $("btn-via-off").textContent = t(viaStopping || "via_off");
  }

  // what the main button of the card does: "play" | "again" | "check"
  function netMainStep(r) {
    if (!net.fromPlay) return "check";
    if (net.error) return "again";
    if (!r) return "check";
    if (r.plan === "proxy" || !netStops(r)) return "play";
    return r.plan === "noroute" || r.plan === "via_bad" ? "again" : "check";
  }
  function netMain() {
    if (net.running) return;
    if (netMainStep(net.report) === "check") {
      runNetCheck();
      return;
    }
    closeNet();
    play();
  }

  async function play(opts) {
    if (busy) return;
    // «Играть без этого» in the connection card: FiveM at once, nothing is checked; the nick already went to the server with
    // the first «Играть»
    const without = !!(opts && opts.without);
    const checked = nickCheck($("nick").value);
    if (checked.error) return renderNick();
    // 0.5.0: the server restarts within 2 minutes: say so first («Играть сейчас» / «Закрыть»)
    const left = restartLeft();
    if (!(opts && opts.fromRestart) && !without && left != null && left <= RESTART_WARN_S) {
      openRestart();
      return;
    }
    busy = true;
    storageUi = null;
    const btn = $("play");
    const label = btn.querySelector(".play__text");
    const release = (ms) => setTimeout(() => {
      busy = false;
      btn.classList.remove("is-busy");
      label.textContent = t("play");
      renderFivem();
    }, ms);
    btn.disabled = true;
    btn.classList.add("is-busy");
    // 0) Connection to Cfx.re and COLDZ, in parallel with the nick (not repeated when a check that lets «Играть» go on is
    // less than a minute old).
    const fresh = net.report && !net.running && !netStops(net.report) && Date.now() - net.at < NET_FRESH_MS;
    const netCheck = without ? Promise.resolve(null) : fresh ? Promise.resolve(net.report) : runNetCheck();
    label.textContent = t(without || fresh ? "launching" : "net_checking_short");
    // 0.3.5) The route: the main server answers within ~3 s, or the first reserve route that answers
    // (src-tauri/src/route.rs). The nick and FiveM then go the same way.
    const chosen = await invoke("choose_route").catch(() => null);
    if (chosen && chosen.address) {
      address = chosen.address;
      route = chosen.kind;
      renderAddress();
    }
    // 1) The nick to our server. It said no — show why under the field and do not start FiveM.
    // 0.5.4: at the same time the game build the server needs (/info.json vars, src-tauri/src/gamebuild.rs); «play»
    // then sets it in FiveM's CitizenFX.ini before FiveM starts (only while FiveM is not running).
    $("nick").value = checked.nick;
    const buildAsk = invoke("game_build").catch(() => null);
    const [nickOk, netRes, build] = await Promise.all([without ? true : sendNick(checked.nick), netCheck, buildAsk]);
    renderNick();
    if (!nickOk) {
      release(0);
      $("nick").focus();
      return;
    }
    // FiveM would hang or fail, and the launcher cannot help it: say whose problem it is (one button, «Играть без этого»).
    if (netStops(netRes)) {
      release(0);
      openNet(true);
      return;
    }
    // 0.5.7: the sign-in does not answer at the provider, the COLDZ server can carry it: no questions, only Windows asks once.
    // Not for a FiveM that is open already (it has signed in).
    if (netRes && netRes.plan === "via" && (await invoke("fivem_state").catch(() => "launch")) === "launch") {
      label.textContent = t("via_button");
      const error = await connectVia();
      if (error) {
        release(0);
        openNet(true, error);
        return;
      }
    }
    const via = !!(net.report && net.report.signin && net.report.signin.active);
    // whether the sign-in really answers through it: for net-check.log, nobody waits for it
    if (via && netRes && netRes.plan !== "via_bad") setTimeout(runNetCheck, 3000);
    // 0.5.4: a game file FiveM would otherwise download inside the game: first here, with progress and «Отменить»
    // (src-tauri/src/storage.rs). Stopped or failed: «Играть» continues it, «Играть без докачки» leaves it to FiveM.
    if (build && build.storage && !(opts && opts.skipStorage)) {
      label.textContent = t("storage_button");
      if (!(await downloadStorage(build.storage))) {
        release(0);
        return;
      }
    }
    label.textContent = t("launching");
    // 2) FiveM: started, or the link went into an open FiveM, or it is still starting from the last «Play»
    // (not started twice: that is the «LinkProtocolIPC» error), or it is stuck — offer to close it.
    let holdMs = 4000;  // after a real start: guard against double clicks while FiveM is starting
    try {
      const res = await invoke("play");
      const action = (res && res.action) || "launched";
      if (action === "starting") {
        holdMs = 1500;
        showFivemMessage("fivem_starting", 15000);
      } else if (action === "launched" && res && res.build) {
        // 0.5.4: the game build was just set: FiveM may download the game data first («Continue»). A little longer
        // before the window goes away, so the line can be read.
        showFivemMessage("launched_build", 60000, false, via ? "launched_via" : null);
        setTimeout(minimizeWindow, via ? 10000 : 8000);
      } else {
        // 0.5.7: through the COLDZ server the launcher has to stay open while FiveM runs
        const key = action === "passed" ? "launched_passed" : via ? "launched_via" : "launched";
        showFivemMessage(key, via ? 60000 : 10000);
        setTimeout(minimizeWindow, via ? 6000 : 2500);
      }
    } catch (err) {
      if (err === "fivem_missing") {
        fivemInstalled = false;
        renderFivem();
      } else if (err === "no_address") {
        address = "";
        showFivemMessage("no_address", 10000);
      } else if (err === "fivem_stuck") {
        holdMs = 0;
        showFivemMessage("fivem_stuck", 120000, true);
      } else {
        showFivemMessage("launch_failed", 20000);
      }
    } finally {
      release(holdMs);
    }
  }

  // ------------------------------------------------------------ news + rules
  function pick(field) {
    if (field == null) return "";
    if (typeof field === "string") return field;
    return field[lang] || field.ru || field.en || Object.values(field)[0] || "";
  }

  function formatDate(iso) {
    if (!iso) return "";
    const d = new Date(iso + (iso.length === 10 ? "T12:00:00" : ""));
    if (isNaN(d)) return iso;
    try { return new Intl.DateTimeFormat(lang, { day: "numeric", month: "long", year: "numeric" }).format(d); }
    catch { return iso; }
  }

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }

  function renderNews() {
    const list = $("news-list");
    const note = $("news-note");
    note.hidden = tab !== "news" || newsSource === "remote" || !newsData;
    note.textContent = t("news_offline");
    list.replaceChildren();
    if (!newsData) return;

    const items = (Array.isArray(newsData.items) ? newsData.items : [])
      .filter((it) => pick(it.title) || pick(it.body))
      .sort((a, b) => (b.pinned ? 1 : 0) - (a.pinned ? 1 : 0) || String(b.date || "").localeCompare(String(a.date || "")));

    if (!items.length) {
      list.append(el("div", "panel__empty", t("news_empty")));
      return;
    }
    for (const it of items) {
      const card = el("article", "item");
      const meta = el("div", "item__meta");
      if (it.pinned) meta.append(el("span", "item__pin", t("pinned")));
      if (it.date) meta.append(el("span", null, formatDate(it.date)));
      card.append(meta, el("h3", null, pick(it.title)));
      const body = pick(it.body);
      if (body) card.append(el("p", null, body));
      if (typeof it.image === "string" && it.image.startsWith("https://")) {
        const img = el("img");
        img.src = it.image;
        img.alt = "";
        img.loading = "lazy";
        img.onerror = () => img.remove();
        card.append(img);
      }
      const url = pick(it.url);
      if (url) {
        const more = el("button", "item__more", t("read_more") + " →");
        more.onclick = () => openExternal(url);
        card.append(more);
      }
      list.append(card);
    }
  }

  // Rules from news.json "rules": [{ title: {ru, en, ...}, text: {ru, en, ...} }]
  function renderRules() {
    const box = $("rules-list");
    box.replaceChildren();
    const rules = (newsData && Array.isArray(newsData.rules) ? newsData.rules : [])
      .filter((r) => pick(r.title) || pick(r.text));
    if (!rules.length) {
      box.append(el("div", "panel__empty", t("rules_empty")));
      return;
    }
    const ol = el("ol", "rules");
    for (const r of rules) {
      const li = el("li");
      const body = el("div");
      body.append(el("b", null, pick(r.title)), el("span", null, pick(r.text)));
      li.append(body);
      ol.append(li);
    }
    box.append(ol);
  }

  function showTab(name) {
    tab = name;
    document.querySelectorAll(".tab").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
    $("news-list").hidden = name !== "news";
    $("rules-list").hidden = name !== "rules";
    renderNews();
  }

  let newsAt = 0;
  let updateAt = 0;
  async function loadNews() {
    newsAt = Date.now();
    try {
      const res = await invoke("fetch_news");
      newsData = res.data;
      newsSource = res.source;
    } catch {
      newsData = newsData || { items: [] };
    }
    renderNews();
    renderRules();
  }

  // ------------------------------------------------------------ updates
  async function checkUpdate() {
    updateAt = Date.now();
    try {
      const upd = await invoke("check_update");
      if (!upd) return;
      updateVersion = upd.version;
      renderUpdate();
      $("update").hidden = false;
    } catch (e) {
      // No internet or no release yet — the launcher works without updates.
      console.info("update check:", e);
    }
  }

  // 0.5.0: downloaded in the background: «installs itself when you close the launcher», the button still works
  let updateVersion = null;
  let updateReady = false;
  let portable = false;      // 0.5.0: the portable zip: a new version is downloaded by hand
  function renderUpdate() {
    if (!updateVersion || $("btn-update").disabled) return;
    $("update-text").textContent = t(portable ? "update_portable" : updateReady ? "update_ready" : "update_available", { v: updateVersion });
    $("btn-update").textContent = t(portable ? "update_download" : "update_install");
  }

  // 0.5.8: again every NEWS_EVERY_MS and UPDATE_EVERY_MS; `fresh` — the window was just opened: sooner
  function refreshFeed(fresh) {
    const now = Date.now();
    if (now - newsAt >= (fresh ? NEWS_ON_FOCUS_MS : NEWS_EVERY_MS)) loadNews().then(() => { refreshStatus(); refreshInfo(); });
    if (!updateVersion && now - updateAt >= (fresh ? NEWS_EVERY_MS : UPDATE_EVERY_MS)) checkUpdate();
  }

  async function installUpdate() {
    if (portable) {
      if (cfg && cfg.portableDownloadUrl) openExternal(cfg.portableDownloadUrl);
      return;
    }
    const btn = $("btn-update");
    btn.disabled = true;
    $("update-text").textContent = t("updating", { p: 0 });
    const unlisten = await T.event.listen("update-progress", (e) => {
      $("update-text").textContent = t("updating", { p: e.payload });
    });
    try {
      await invoke("install_update");
    } catch (e) {
      unlisten();
      btn.disabled = false;
      $("update-text").textContent = t("update_failed");
    }
  }

  // ------------------------------------------------------------ links
  const SVG_NS = "http://www.w3.org/2000/svg";

  // Social buttons from launcher.config.json "links"; empty url = not shown.
  // "langs" (20.11, Instagram): only in those window languages; none = every language
  function renderLinks() {
    const nav = $("socials");
    nav.replaceChildren();
    for (const link of (cfg && cfg.links) || []) {
      if (!link.url) continue;
      if (Array.isArray(link.langs) && link.langs.length && !link.langs.includes(lang)) continue;
      const btn = el("button", "social");
      btn.title = link.label;
      btn.setAttribute("aria-label", link.label);
      const path = window.ICONS && ICONS[link.icon];
      if (path) {
        const svg = document.createElementNS(SVG_NS, "svg");
        svg.setAttribute("viewBox", "0 0 24 24");
        const p = document.createElementNS(SVG_NS, "path");
        p.setAttribute("d", path);
        svg.append(p);
        btn.append(svg);
      } else {
        btn.textContent = link.label.slice(0, 2).toUpperCase();
      }
      if (link.badge) btn.append(el("span", "social__badge", link.badge));
      btn.onclick = () => openExternal(link.url);
      nav.append(btn);
    }
  }

  function openExternal(url) {
    invoke("open_external", { url }).catch((e) => console.warn("open link:", e));
  }

  // ------------------------------------------------------------ start
  async function init() {
    lang = detectLang();
    applyLang();

    $("lang").onchange = (e) => { lang = e.target.value; storageSet("lang", lang); applyLang(); if (prefs.tray) setTray(true); };
    $("btn-min").onclick = minimizeWindow;
    $("pref-tray").onchange = (e) => setTray(e.target.checked);
    $("btn-max").onclick = () => appWindow.toggleMaximize();
    $("btn-close").onclick = () => appWindow.close();
    $("play").onclick = play;
    $("nick").oninput = onNickInput;
    $("nick").onkeydown = (e) => { if (e.key === "Enter" && !$("play").disabled) play(); };
    $("btn-copy").onclick = copyAddress;
    $("btn-recheck").onclick = refreshFivem;
    $("btn-pick-fivem").onclick = pickFivem;
    $("btn-close-fivem").onclick = closeFivem;
    $("btn-storage-cancel").onclick = () => { invoke("storage_cancel").catch(() => {}); };
    $("btn-storage-skip").onclick = () => { storageUi = null; play({ skipStorage: true }); };
    $("btn-update").onclick = installUpdate;
    $("btn-net").onclick = () => { openNet(false); runNetCheck(); };
    $("btn-net-main").onclick = netMain;
    $("btn-net-more").onclick = () => { net.more = !net.more; renderNet(); };
    $("btn-net-without").onclick = () => { closeNet(); play({ without: true }); };
    $("btn-via-off").onclick = turnViaOff;
    $("btn-net-retry").onclick = () => { runNetCheck(); };
    $("btn-net-details").onclick = () => { openNet(false); };
    $("btn-net-close").onclick = closeNet;
    $("btn-restart-play").onclick = () => { $("restart").hidden = true; play({ fromRestart: true }); };
    $("btn-restart-close").onclick = () => { $("restart").hidden = true; };
    $("btn-hint-report").onclick = openReport;
    $("btn-hint-hide").onclick = hideHint;
    $("btn-settings").onclick = openSettings;
    $("btn-settings-close").onclick = () => { $("settings").hidden = true; };
    $("btn-report-open").onclick = openReport;
    $("btn-report-make").onclick = makeReport;
    $("btn-report-close").onclick = () => { $("report").hidden = true; };
    document.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      if (!$("restart").hidden) $("restart").hidden = true;
      else if (!$("report").hidden) $("report").hidden = true;
      else if (!$("settings").hidden) $("settings").hidden = true;
      else if (!$("net").hidden) closeNet();
    });
    document.querySelectorAll(".tab").forEach((b) => { b.onclick = () => showTab(b.dataset.tab); });
    document.addEventListener("contextmenu", (e) => e.preventDefault());

    cfg = await invoke("get_config");
    $("version").textContent = "v" + cfg.version;
    try { portable = !!(await invoke("is_portable")); } catch { portable = false; }
    try { prefs = (await invoke("get_prefs")) || prefs; } catch { /* first start */ }
    // the tray icon comes back with the launcher (labels in the current language)
    if (prefs.tray) setTray(true);
    $("max-players").textContent = cfg.maxPlayers;
    $("email").textContent = cfg.ownerEmail;
    $("email").onclick = (e) => { e.preventDefault(); openExternal("mailto:" + cfg.ownerEmail); };
    $("btn-download").onclick = () => openExternal(cfg.fivemDownloadUrl);
    renderLinks();
    // the nick saved last time ("" = FiveM name)
    try { $("nick").value = (await invoke("get_nick")) || ""; } catch { /* first start */ }
    renderNick();

    await refreshFivem();
    loadHint();
    refreshStatus();
    // news.json may bring a new server address: check the status again right after it. Then (0.3.4) the connection
    // check once, quietly: about a second and ~70 KB; only a problem shows up (the line under «Играть»), and «Играть»
    // within a minute of a good result does not wait for a new one.
    loadNews().then(() => { refreshStatus(); runNetCheck(); refreshInfo(); });
    setTimeout(checkUpdate, 1500);
    T.event.listen("update-ready", () => { updateReady = true; renderUpdate(); }).catch(() => {});
    T.event.listen("storage-progress", (e) => onStorageProgress(e.payload)).catch(() => {});
    T.event.listen("signin-step", (e) => onSigninStep(e.payload)).catch(() => {});

    setInterval(refreshStatus, STATUS_EVERY_MS);
    setInterval(refreshInfo, INFO_EVERY_MS);
    setInterval(renderRestart, 15000);
    setInterval(() => refreshFeed(false), 60000);
    setInterval(() => { if (!fivemInstalled && document.hasFocus()) refreshFivem(); }, FIVEM_POLL_MS);

    // Endless animations pause while the window is not active (e.g. during the game).
    const setIdle = (idle) => document.body.classList.toggle("idle", idle);
    setIdle(!document.hasFocus());
    window.addEventListener("focus", () => { setIdle(false); refreshFivem(); refreshStatus(); refreshFeed(true); });
    window.addEventListener("blur", () => setIdle(true));
  }

  init().catch((e) => console.error(e));

  // ------------------------------------------------------------ browser preview
  // Only used when the page is opened without Tauri (e.g. ui/index.html in a browser).
  // ?fivem=0 — FiveM "not installed", ?address=0 — address not published yet, ?nick=down — the server does not
  // answer about the nick. A nick with "admin"/"админ" is rejected like the server does, "often" — too often.
  // ?run=open — FiveM is open (the link goes into it), ?run=starting — still starting, ?run=stuck — stuck in the
  // background («Close FiveM» button; ?run=stuck&close=fail — could not close everything).
  // ?fivem=0&pick=ok — «Choose FiveM.exe manually» finds it, &pick=wrong — the chosen file is not FiveM.exe.
  // Connection check (15.11): ?net=blocked — Cfx.re cut, COLDZ answers (the card on «Играть»), ?net=list — only the
  // server list is cut (no card on «Играть»), ?net=offline — nothing answers, ?net=server — the COLDZ server is down,
  // ?net=listserver — the server list and the COLDZ server (0.3.4: «server_down» first).
  // 0.3.4: the check also runs at start (blocked/offline: the line under «Играть»). ?netseq=blocked,ok — one mode per
  // check in turn (start, «Играть» / «Проверить ещё раз», ...), the last one repeats: another network.
  // &card=play — press «Играть» by itself after start, &card=check — «Проверить связь» (screenshots without clicks).
  // 0.3.5: ?route=reserve — the main server does not answer, a reserve route does («Подключение: запасной сервер»);
  // ?route=none — nothing answers (with ?net=server: the «server_down» card).
  // 0.4.0: ?net=signin — the FiveM sign-in is cut, the COLDZ server answers: «Играть» connects through it by itself (0.5.7);
  // &tunnel=0 — the COLDZ server does not answer either; &route=on — the sign-in through it is already on.
  // 0.5.7: &signin=cancel|port|noroute|fail — what goes wrong on «Играть» (the Windows prompt is declined, port 443 is taken,
  // ...); &hold=connect|prompt — stays on that step (screenshots); ?net=proxy — a VPN or proxy on the PC, &net=proxyok — the
  // same, but everything works; &more=1 — «Подробнее» open; ?step=cleanup — the line before Windows asks at start.
  function previewTauri() {
    const q = new URLSearchParams(location.search);
    let netCalls = 0;
    let signinOn = q.get("route") === "on";
    const card = q.get("card");
    if (card === "play" || card === "check") {
      setTimeout(() => document.getElementById(card === "play" ? "play" : "btn-net").click(), 1500);
    }
    if (q.get("more") === "1") setTimeout(() => document.getElementById("btn-net-more").click(), 4500);
    if (q.get("step")) setTimeout(() => { if (listeners["signin-step"]) listeners["signin-step"]({ payload: q.get("step") }); }, 1500);
    if (card === "report" || card === "settings") {
      setTimeout(() => document.getElementById(card === "report" ? "btn-report-open" : "btn-settings").click(), 1500);
    }
    const data = {
      get_config: {
        serverName: "COLDZ", maxPlayers: 48, ownerEmail: "owner@example.com", version: "preview",
        fivemDownloadUrl: "https://fivem.net/", portableDownloadUrl: "https://github.com/Poopseeque/coldz-launcher/releases/latest",
        links: [
          { icon: "vk", label: "ВКонтакте", url: "https://vk.ru/coldz_server" },
          { icon: "youtube", label: "YouTube", url: "https://youtube.com/@coldzgta5" },
          { icon: "tiktok", label: "TikTok", langs: ["ru", "es"], url: "https://www.tiktok.com/@coldz_gta5" },
          { icon: "tiktok", label: "TikTok", langs: ["en"], url: "https://www.tiktok.com/@coldz_gta5_en" },
          { icon: "tiktok", label: "TikTok", langs: ["pt-BR"], url: "https://www.tiktok.com/@coldz_gta5_br" },
          { icon: "instagram", label: "Instagram", langs: ["ru", "es"], url: "https://www.instagram.com/coldz_gta5_dayz/" },
          { icon: "instagram", label: "Instagram", langs: ["en"], url: "https://www.instagram.com/coldz_gta5_dayz_en/" },
          { icon: "instagram", label: "Instagram", langs: ["pt-BR"], url: "https://www.instagram.com/coldz_gta5_dayz_br/" },
          { icon: "telegram", label: "Telegram", url: "" }
        ]
      },
      detect_fivem: { installed: q.get("fivem") !== "0" },
      // 0.5.0: &portable=1 — the portable zip (with ?update=1: «download the new version»)
      is_portable: q.get("portable") === "1",
      get_prefs: { tray: q.get("tray") === "1" },
      server_status: q.get("address") === "0"
        ? { online: false, players: 0, maxPlayers: 48, pingMs: null, address: "" }
        : { online: true, players: 12, maxPlayers: 48, pingMs: 34, address: "203.0.113.10:30120", route: null },
      // 0.5.0: ?update=1 — a new version (downloaded in the background: «installs itself on close»)
      check_update: q.get("update") ? { version: "0.5.0", notes: null } : null
    };
    let previewNick = "";
    let previewStorageCancel = false;
    const listeners = {};
    // the route a check chose: a reserve address with ?route=reserve
    const previewRoute = () => {
      if (q.get("address") === "0") return;
      const mode = q.get("route");
      data.server_status.route = mode === "reserve" ? "reserve" : mode === "none" ? "none" : "main";
      data.server_status.address = mode === "reserve" ? "198.51.100.20:30120" : "203.0.113.10:30120";
    };
    const call = async (cmd, args) => {
      if (cmd === "fetch_news") {
        const r = await fetch("../news/news.json");
        return { source: "remote", data: await r.json() };
      }
      if (cmd === "get_nick") return previewNick;
      if (cmd === "send_nick") {
        const nick = (args && args.nick) || "";
        previewNick = nick;
        await new Promise((r) => setTimeout(r, 300));
        if (q.get("nick") === "down") return { status: "unreachable", nick };
        if (/admin|админ/i.test(nick)) return { status: "rejected", nick, reason: "reserved" };
        if (/often/i.test(nick)) return { status: "rejected", nick, reason: "often" };
        return { status: "ok", nick };
      }
      if (cmd === "play") {
        const run = q.get("run");
        if (run === "stuck") throw "fivem_stuck";
        // 0.5.4: &build=1 — the launcher has just set the game build (the «Continue» line)
        const action = run === "open" ? "passed" : run === "starting" ? "starting" : "launched";
        return { action, build: action === "launched" && q.get("build") === "1" };
      }
      // 0.5.4: ?storage=1 — a game file to download first (progress, «Отменить»); ?storage=fail — the download breaks;
      // ?storage=space — not enough space
      if (cmd === "game_build") {
        const st = q.get("storage");
        return { ok: true, storage: st ? { total: 1423288320, have: 300000000 } : null };
      }
      if (cmd === "storage_cancel") { previewStorageCancel = true; return null; }
      if (cmd === "storage_download") {
        previewStorageCancel = false;
        const st = q.get("storage");
        if (st === "space") throw "space";
        for (let i = 1; i <= 12; i++) {
          await new Promise((r) => setTimeout(r, 400));
          if (previewStorageCancel) throw "cancelled";
          if (st === "fail" && i === 4) throw "network";
          const fn = listeners["storage-progress"];
          if (fn) fn({ payload: i <= 10 ? { done: 300000000 + i * 112328832, total: 1423288320, phase: "download" } : { done: (i - 10) * 711644160, total: 1423288320, phase: "check" } });
        }
        return null;
      }
      if (cmd === "close_fivem") return q.get("close") === "fail" ? [2, 1] : [3, 0];
      if (cmd === "choose_route") {
        await new Promise((r) => setTimeout(r, 300));
        previewRoute();
        return { kind: data.server_status.route, address: data.server_status.address };
      }
      if (cmd === "check_network") {
        await new Promise((r) => setTimeout(r, 900));
        const seq = (q.get("netseq") || q.get("net") || "ok").split(",");
        const mode = seq[Math.min(netCalls++, seq.length - 1)];
        const svc = (id, ok, problem, critical) => ({ id, host: id + ".example", ok, problem: ok ? null : problem, status: ok ? 200 : null, bytes: 0, ms: 120, critical });
        const cut = mode === "blocked" || mode === "offline";
        const services = [
          svc("update", !cut, mode === "offline" ? "dns" : "slow", true),
          svc("auth", !cut, "tls", true),
          svc("policy", !cut, "timeout", true),
          svc("services", mode !== "offline", "tls", false),
          svc("list", !cut && mode !== "list" && mode !== "listserver", "tls", false)
        ];
        const routeMode = ["reserve", "none"].includes(q.get("route")) ? q.get("route") : null;   // "on" is the sign-in
        const server = !["offline", "server", "listserver"].includes(mode) && !routeMode;
        // without ?route the reserve route answers like the main server (the 0.3.4 previews stay as they were)
        const reserve = routeMode === "reserve" ? mode !== "offline" : routeMode === "none" ? false : server;
        let verdict = { blocked: "cfx_blocked", list: "cfx_blocked", offline: "offline", server: "server_down", listserver: "server_down" }[mode] || "ok";
        if (reserve && (mode === "server" || mode === "listserver")) verdict = mode === "server" ? "ok" : "cfx_blocked";
        if (!server && !reserve && verdict === "ok") verdict = "server_down";
        previewRoute();
        // 0.4.0: the sign-in hosts and the backup route for them (always present in the preview, like news.json)
        const proxy = mode === "proxy" || mode === "proxyok";
        const signinCut = (mode === "signin" || mode === "proxy") && !signinOn;
        const tunnelOk = q.get("tunnel") !== "0";
        const signin = {
          groups: [
            { id: "rockstar", host: "scui.rockstargames.com", ok: !signinCut && (!signinOn || tunnelOk), problem: signinCut ? "connect" : null, status: signinCut ? null : 403, ms: 300 },
            { id: "cfx", host: "lambda.fivem.net", ok: true, problem: null, status: 200, ms: 140 }
          ],
          tunnel: { address: "198.51.100.20:8443", ok: tunnelOk, status: tunnelOk ? 403 : null, bytes: tunnelOk ? 99000 : 0, problem: tunnelOk ? null : "relay", ms: 420 },
          active: signinOn,
          blocked: signinCut
        };
        const pings = [
          { kind: "main", index: 0, address: "203.0.113.10:30120", ms: server ? (proxy ? 1 : 34) : null },
          { kind: "reserve", index: 1, address: "198.51.100.20:30120", ms: reserve ? (proxy ? 1 : 52) : null },
          { kind: "reserve", index: 2, address: "198.51.100.30:30120", ms: reserve ? (proxy ? 1 : 88) : null }
        ];
        // the same order as netcheck.rs `plan`
        const down = server === false && reserve !== true;
        let plan = "go";
        if (verdict === "offline") plan = "offline";
        else if (down && (signin.blocked ? tunnelOk : !cut)) plan = "server";
        else if (signin.blocked) plan = tunnelOk ? "via" : "noroute";
        else if (signin.active && signin.groups.some((g) => !g.ok)) plan = "via_bad";
        else if (cut) plan = "cfx";
        if (proxy && plan !== "go") plan = "proxy";
        return { verdict, blocking: cut, services, server, reserve, route: data.server_status.route, signin, pings, proxy, plan, ms: 900 };
      }
      if (cmd === "fivem_state") return { open: "passed", starting: "starting", stuck: "stuck" }[q.get("run")] || "launch";
      if (cmd === "signin_enable") {
        const step = async (name, ms) => {
          if (listeners["signin-step"]) listeners["signin-step"]({ payload: name });
          await new Promise((r) => { if (q.get("hold") !== name) setTimeout(r, ms); });
        };
        await step("connect", 1200);
        const bad = { port: "port_busy", noroute: "no_route", cancel: "cancelled", fail: "failed" }[q.get("signin")];
        if (bad === "port_busy" || bad === "no_route") throw bad;
        await step("prompt", 1800);
        if (bad) throw bad;
        signinOn = true;
        return { active: true, ip: "127.0.0.1", hosts: 12, tunnel: "198.51.100.20:8443", quietOff: true };
      }
      if (cmd === "signin_disable") {
        await new Promise((r) => setTimeout(r, 600));
        if (q.get("signin") === "cancel") throw "cancelled";
        signinOn = false;
        return { active: false, ip: null, hosts: 0, tunnel: null, quietOff: false };
      }
      if (cmd === "signin_status") return { active: signinOn, ip: null, hosts: 0, tunnel: null, quietOff: q.get("quiet") !== "0" };
      if (cmd === "pick_fivem") {
        await new Promise((r) => setTimeout(r, 300));
        if (q.get("pick") === "wrong") throw "not_fivem";
        if (q.get("pick") === "ok") return { installed: true, path: "D:\\Games\\FiveM\\FiveM.exe", foundBy: "manual" };
        return null;
      }
      // 0.5.0: ?hint=fixup|low_disk|two_gta|onedrive|slow_disk|crash — the line after a FiveM crash; &card=report —
      // the support report card, &card=settings — the settings card
      // 0.5.0: ?restart=<minutes> — a restart announced (1 = the card on «Играть»)
      if (cmd === "set_tray") return { tray: !!(args && args.on) };
      if (cmd === "minimize_window") return null;
      if (cmd === "server_info") {
        const m = q.get("restart");
        return { restartIn: m != null ? Number(m) * 60 - 20 : null };
      }
      if (cmd === "crash_hint") {
        const key = q.get("hint");
        if (!key) return null;
        const detail = { fixup: "prop_tree_01.ydr", low_disk: "C:", two_gta: "D:\\Games\\Grand Theft Auto V" }[key] || "";
        return { key, detail, gb: key === "low_disk" ? 3 : 0, at: 1794950000 };
      }
      if (cmd === "support_preview") {
        await new Promise((r) => setTimeout(r, 400));
        return {
          files: [
            { name: "net-check.log", size: 48213, at: 1794950000 },
            { name: "fivem-search.log", size: 9120, at: 1794950000 },
            { name: "CitizenFX_log_2026-11-18T134401.log", size: 812004, at: 1794950000 },
            { name: "85f04185-913c-47f4-a763-f425b702f3d7.dmp.gamelog", size: 116010, at: 1794950000 },
            { name: "system.txt", size: 0, at: 1794950000 }
          ],
          dump: { name: "85f04185-913c-47f4-a763-f425b702f3d7.dmp", size: 18071304, at: 1794950000 },
          dumpsToday: 2
        };
      }
      if (cmd === "support_create") {
        await new Promise((r) => setTimeout(r, 600));
        return { path: "C:\\Users\\<user>\\Desktop\\COLDZ-support-20261118-223005.zip", name: "COLDZ-support-20261118-223005.zip", size: 1000 };
      }
      if (cmd === "open_external") { console.log("[preview]", cmd); return "ok"; }
      return data[cmd];
    };
    const win = { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} };
    const listen = async (name, fn) => {
      listeners[name] = fn;
      if (name === "update-ready" && q.get("update")) setTimeout(() => fn({ payload: "0.5.0" }), 2500);
      return () => {};
    };
    return { core: { invoke: call }, window: { getCurrentWindow: () => win }, event: { listen } };
  }
})();
