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
  let tab = "news";

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
  }

  async function refreshStatus() {
    try {
      const s = await invoke("server_status");
      address = s.address || "";
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
    if (showing) {
      box.dataset.state = "message";
      text.textContent = t(fivemMessage.key, { a: address || "—" });
      actions.hidden = true;
      return;
    }
    fivemMessage = null;
    if (box.dataset.state === "checking" && !cfg) {
      text.textContent = t("fivem_checking");
      return;
    }
    box.dataset.state = fivemInstalled ? "ok" : "missing";
    text.textContent = t(fivemInstalled ? "fivem_found" : "fivem_missing");
    actions.hidden = fivemInstalled;
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
  function showFivemMessage(key, ms, stuck) {
    fivemMessage = { key, until: Date.now() + ms, stuck: !!stuck };
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

  async function play() {
    if (busy) return;
    const checked = nickCheck($("nick").value);
    if (checked.error) return renderNick();
    busy = true;
    const btn = $("play");
    const release = (ms) => setTimeout(() => {
      busy = false;
      btn.classList.remove("is-busy");
      btn.querySelector(".play__text").textContent = t("play");
      renderFivem();
    }, ms);
    btn.disabled = true;
    btn.classList.add("is-busy");
    btn.querySelector(".play__text").textContent = t("launching");
    // 1) The nick to our server. It said no — show why under the field and do not start FiveM.
    $("nick").value = checked.nick;
    const nickOk = await sendNick(checked.nick);
    renderNick();
    if (!nickOk) {
      release(0);
      $("nick").focus();
      return;
    }
    // 2) FiveM: started, or the link went into an open FiveM, or it is still starting from the last «Play»
    // (not started twice: that is the «LinkProtocolIPC» error), or it is stuck — offer to close it.
    let holdMs = 4000;  // after a real start: guard against double clicks while FiveM is starting
    try {
      const res = await invoke("play");
      const action = (res && res.action) || "launched";
      if (action === "starting") {
        holdMs = 1500;
        showFivemMessage("fivem_starting", 15000);
      } else {
        showFivemMessage(action === "passed" ? "launched_passed" : "launched", 10000);
        setTimeout(() => appWindow.minimize().catch(() => {}), 2500);
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

  async function loadNews() {
    try {
      const res = await invoke("fetch_news");
      newsData = res.data;
      newsSource = res.source;
    } catch {
      newsData = { items: [] };
    }
    renderNews();
    renderRules();
  }

  // ------------------------------------------------------------ updates
  async function checkUpdate() {
    try {
      const upd = await invoke("check_update");
      if (!upd) return;
      $("update-text").textContent = t("update_available", { v: upd.version });
      $("update").hidden = false;
    } catch (e) {
      // No internet or no release yet — the launcher works without updates.
      console.info("update check:", e);
    }
  }

  async function installUpdate() {
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
  function renderLinks() {
    const nav = $("socials");
    nav.replaceChildren();
    for (const link of cfg.links || []) {
      if (!link.url) continue;
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

    $("lang").onchange = (e) => { lang = e.target.value; storageSet("lang", lang); applyLang(); };
    $("btn-min").onclick = () => appWindow.minimize();
    $("btn-max").onclick = () => appWindow.toggleMaximize();
    $("btn-close").onclick = () => appWindow.close();
    $("play").onclick = play;
    $("nick").oninput = onNickInput;
    $("nick").onkeydown = (e) => { if (e.key === "Enter" && !$("play").disabled) play(); };
    $("btn-copy").onclick = copyAddress;
    $("btn-recheck").onclick = refreshFivem;
    $("btn-pick-fivem").onclick = pickFivem;
    $("btn-close-fivem").onclick = closeFivem;
    $("btn-update").onclick = installUpdate;
    document.querySelectorAll(".tab").forEach((b) => { b.onclick = () => showTab(b.dataset.tab); });
    document.addEventListener("contextmenu", (e) => e.preventDefault());

    cfg = await invoke("get_config");
    $("version").textContent = "v" + cfg.version;
    $("max-players").textContent = cfg.maxPlayers;
    $("email").textContent = cfg.ownerEmail;
    $("email").onclick = (e) => { e.preventDefault(); openExternal("mailto:" + cfg.ownerEmail); };
    $("btn-download").onclick = () => openExternal(cfg.fivemDownloadUrl);
    renderLinks();
    // the nick saved last time ("" = FiveM name)
    try { $("nick").value = (await invoke("get_nick")) || ""; } catch { /* first start */ }
    renderNick();

    await refreshFivem();
    refreshStatus();
    // news.json may bring a new server address: check the status again right after it.
    loadNews().then(refreshStatus);
    setTimeout(checkUpdate, 1500);

    setInterval(refreshStatus, STATUS_EVERY_MS);
    setInterval(() => { if (!fivemInstalled && document.hasFocus()) refreshFivem(); }, FIVEM_POLL_MS);

    // Endless animations pause while the window is not active (e.g. during the game).
    const setIdle = (idle) => document.body.classList.toggle("idle", idle);
    setIdle(!document.hasFocus());
    window.addEventListener("focus", () => { setIdle(false); refreshFivem(); refreshStatus(); });
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
  function previewTauri() {
    const q = new URLSearchParams(location.search);
    const data = {
      get_config: {
        serverName: "COLDZ", maxPlayers: 48, ownerEmail: "owner@example.com", version: "preview",
        fivemDownloadUrl: "https://fivem.net/",
        links: [
          { icon: "vk", label: "ВКонтакте", url: "https://vk.ru/coldz_server" },
          { icon: "youtube", label: "YouTube", url: "https://youtube.com/@coldzgta5" },
          { icon: "tiktok", label: "TikTok", url: "https://www.tiktok.com/@coldz_gta5" },
          { icon: "telegram", label: "Telegram", url: "" }
        ]
      },
      detect_fivem: { installed: q.get("fivem") !== "0" },
      server_status: q.get("address") === "0"
        ? { online: false, players: 0, maxPlayers: 48, pingMs: null, address: "" }
        : { online: true, players: 12, maxPlayers: 48, pingMs: 34, address: "203.0.113.10:30120" },
      check_update: null
    };
    let previewNick = "";
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
        return { action: run === "open" ? "passed" : run === "starting" ? "starting" : "launched" };
      }
      if (cmd === "close_fivem") return q.get("close") === "fail" ? [2, 1] : [3, 0];
      if (cmd === "pick_fivem") {
        await new Promise((r) => setTimeout(r, 300));
        if (q.get("pick") === "wrong") throw "not_fivem";
        if (q.get("pick") === "ok") return { installed: true, path: "D:\\Games\\FiveM\\FiveM.exe", foundBy: "manual" };
        return null;
      }
      if (cmd === "open_external") { console.log("[preview]", cmd); return "ok"; }
      return data[cmd];
    };
    const win = { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {} };
    return { core: { invoke: call }, window: { getCurrentWindow: () => win }, event: { listen: async () => () => {} } };
  }
})();
