// COLDZ launcher UI. All real work (FiveM, network, updates) is done in Rust
// through invoke(); this file only draws the screen.
(function () {
  const T = window.__TAURI__;
  const invoke = T.core.invoke;
  const appWindow = T.window.getCurrentWindow();
  const $ = (id) => document.getElementById(id);

  const STATUS_EVERY_MS = 20000;
  const LANGS = ["ru", "en", "es", "pt-BR"];

  let lang = "ru";
  let cfg = null;
  let fivemInstalled = false;
  let newsData = null;
  let newsSource = "bundled";
  let fivemMessage = null;   // { key, until }
  let busy = false;

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
    renderStatusText();
    renderFivem();
    renderNews();
  }

  // ------------------------------------------------------------ status
  let lastStatus = null;

  function renderStatusText() {
    const box = $("status");
    const state = box.dataset.state;
    $("status-text").textContent = t(state === "online" ? "online" : state === "offline" ? "offline" : "checking");
  }

  async function refreshStatus() {
    try {
      const s = await invoke("server_status");
      lastStatus = s;
      $("status").dataset.state = s.online ? "online" : "offline";
      $("players").textContent = s.online ? s.players : "—";
      $("max-players").textContent = s.maxPlayers || (cfg && cfg.maxPlayers) || 48;
      $("ping").textContent = s.online && s.pingMs != null ? s.pingMs : "—";
    } catch (e) {
      $("status").dataset.state = "offline";
    }
    renderStatusText();
  }

  // ------------------------------------------------------------ FiveM + play
  function renderFivem() {
    const box = $("fivem");
    const text = $("fivem-text");
    const actions = $("fivem-actions");
    if (fivemMessage && Date.now() < fivemMessage.until) {
      box.dataset.state = "message";
      text.textContent = t(fivemMessage.key);
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
    $("play").disabled = busy || !fivemInstalled;
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

  function showFivemMessage(key, ms) {
    fivemMessage = { key, until: Date.now() + ms };
    renderFivem();
    setTimeout(renderFivem, ms + 50);
  }

  async function play() {
    if (busy) return;
    busy = true;
    const btn = $("play");
    btn.disabled = true;
    btn.classList.add("is-busy");
    btn.querySelector(".play__text").textContent = t("launching");
    try {
      await invoke("play");
      showFivemMessage("launched", 10000);
      setTimeout(() => appWindow.minimize().catch(() => {}), 2500);
    } catch (err) {
      if (err === "fivem_missing") {
        fivemInstalled = false;
        renderFivem();
      } else {
        showFivemMessage("launch_failed", 12000);
      }
    } finally {
      // Guard against double clicks while FiveM is starting.
      setTimeout(() => {
        busy = false;
        btn.classList.remove("is-busy");
        btn.querySelector(".play__text").textContent = t("play");
        renderFivem();
      }, 4000);
    }
  }

  // ------------------------------------------------------------ news
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
    note.hidden = newsSource === "remote" || !newsData;
    note.textContent = t("news_offline");
    list.replaceChildren();
    if (!newsData) return;

    const items = (Array.isArray(newsData.items) ? newsData.items : [])
      .filter((it) => pick(it.title) || pick(it.body))
      .sort((a, b) => (b.pinned ? 1 : 0) - (a.pinned ? 1 : 0) || String(b.date || "").localeCompare(String(a.date || "")));

    if (!items.length) {
      list.append(el("div", "news__empty", t("news_empty")));
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

  async function loadNews() {
    try {
      const res = await invoke("fetch_news");
      newsData = res.data;
      newsSource = res.source;
    } catch {
      newsData = { items: [] };
    }
    renderNews();
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
    $("btn-close").onclick = () => appWindow.close();
    $("play").onclick = play;
    $("btn-recheck").onclick = refreshFivem;
    $("btn-update").onclick = installUpdate;
    document.addEventListener("contextmenu", (e) => e.preventDefault());

    cfg = await invoke("get_config");
    $("version").textContent = "v" + cfg.version;
    $("max-players").textContent = cfg.maxPlayers;
    $("email").textContent = cfg.ownerEmail;
    $("email").onclick = (e) => { e.preventDefault(); openExternal("mailto:" + cfg.ownerEmail); };
    $("btn-download").onclick = () => openExternal(cfg.fivemDownloadUrl);
    renderLinks();

    await refreshFivem();
    refreshStatus();
    loadNews();
    setTimeout(checkUpdate, 1500);

    setInterval(refreshStatus, STATUS_EVERY_MS);
    window.addEventListener("focus", () => { refreshFivem(); refreshStatus(); });
  }

  init().catch((e) => console.error(e));
})();
