// Développement uniquement : hors de Tauri (page servie par « trunk serve »),
// relie l'interface à la passerelle HTTP du cœur (cargo run -p rustscp-devbridge).
// Sans effet dans l'application.
(function () {
  if (window.__TAURI__ || location.port !== "1420") return;
  const base = "http://127.0.0.1:1421";
  const handlers = {};
  let since = 0;
  async function poll() {
    try {
      const r = await fetch(base + "/events?since=" + since);
      const list = await r.json();
      for (const ev of list) {
        since = Math.max(since, ev.seq);
        (handlers[ev.event] || []).forEach((cb) => cb({ event: ev.event, payload: ev.payload }));
      }
    } catch (e) { /* passerelle absente */ }
    setTimeout(poll, 150);
  }
  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        const r = await fetch(base + "/invoke/" + cmd, {
          method: "POST",
          headers: { "Content-Type": "text/plain" },
          body: JSON.stringify(args || {}),
        });
        const data = await r.json();
        if (!r.ok) throw data;
        return data;
      },
    },
    event: {
      listen: async (name, cb) => {
        (handlers[name] = handlers[name] || []).push(cb);
        return () => {};
      },
    },
  };
  poll();
})();
