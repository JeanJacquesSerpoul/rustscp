// Développement uniquement : hors de Tauri (page servie par « trunk serve »),
// relie l'interface à la passerelle HTTP du cœur (cargo run -p rustscp-devbridge).
// Sans effet dans l'application.
(function () {
  if (window.__TAURI__ || location.port !== "1420") return;
  window.__RUSTSCP_DEV__ = true;
  const base = "http://127.0.0.1:1421";
  const handlers = {};
  // canaux IPC (terminal) : identifiants uniques entre les onglets du navigateur
  const channels = {};
  let nextChannel = Math.floor(Math.random() * 1e9);
  class Channel {
    constructor(onmessage) {
      this.id = ++nextChannel;
      this.onmessage = onmessage || (() => {});
      channels[this.id] = this;
    }
    toJSON() {
      return "__CHANNEL__:" + this.id;
    }
  }
  function decode(b64) {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out.buffer;
  }
  let since = 0;
  // la passerelle retient la requête jusqu'au prochain événement (longue attente)
  async function poll() {
    let delay = 0;
    try {
      const r = await fetch(base + "/events?since=" + since);
      const list = await r.json();
      for (const ev of list) {
        since = Math.max(since, ev.seq);
        if (ev.event === "__channel__") {
          const ch = channels[ev.payload.id];
          if (ch) ch.onmessage(decode(ev.payload.data));
          continue;
        }
        (handlers[ev.event] || []).forEach((cb) => cb({ event: ev.event, payload: ev.payload }));
      }
    } catch (e) {
      delay = 1000; // passerelle absente
    }
    setTimeout(poll, delay);
  }
  window.__TAURI__ = {
    core: {
      Channel,
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
