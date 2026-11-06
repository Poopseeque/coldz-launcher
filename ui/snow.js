// Light falling snow: 70 flakes at 30 fps on a timer (not requestAnimationFrame,
// which fires 144–360 times a second on fast monitors). Runs only while the
// launcher window is focused, so it costs nothing while the game is running.
(function () {
  const canvas = document.getElementById("snow");
  const ctx = canvas.getContext("2d");
  const FRAME_MS = 33;
  let w = 0, h = 0, flakes = [], timer = null;

  function resize() {
    w = canvas.width = window.innerWidth;
    h = canvas.height = window.innerHeight;
  }

  function makeFlake(randomY) {
    return {
      x: Math.random() * w,
      y: randomY ? Math.random() * h : -5,
      r: 0.6 + Math.random() * 1.8,
      vy: 0.6 + Math.random() * 1.6,
      vx: -0.3 + Math.random() * 0.6,
      a: 0.25 + Math.random() * 0.55
    };
  }

  function draw() {
    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = "#ebf0fa";
    for (const f of flakes) {
      f.y += f.vy;
      f.x += f.vx + Math.sin(f.y / 40) * 0.25;
      if (f.y > h + 5 || f.x < -5 || f.x > w + 5) Object.assign(f, makeFlake(false));
      ctx.globalAlpha = f.a;
      ctx.beginPath();
      ctx.arc(f.x, f.y, f.r, 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.globalAlpha = 1;
  }

  function setActive(on) {
    if (on && !timer) timer = setInterval(draw, FRAME_MS);
    if (!on && timer) { clearInterval(timer); timer = null; }
  }

  resize();
  window.addEventListener("resize", resize);
  for (let i = 0; i < 70; i++) flakes.push(makeFlake(true));
  draw(); // one still frame, so the snow is visible even when paused

  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

  // Window focus from Tauri itself (reliable when minimized), plus DOM events as backup.
  try {
    window.__TAURI__.window.getCurrentWindow().onFocusChanged(({ payload }) => setActive(payload));
  } catch { /* not running inside Tauri */ }
  window.addEventListener("focus", () => setActive(true));
  window.addEventListener("blur", () => setActive(false));
  document.addEventListener("visibilitychange", () => setActive(!document.hidden && document.hasFocus()));
  setActive(document.hasFocus());
})();
