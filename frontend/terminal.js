// Terminal SSH : liaison entre xterm.js et le cœur Rust (fenêtres de terminal uniquement).
//
// Le chemin critique reste en JavaScript : les trames binaires du cœur (canal IPC
// Tauri) sont écrites directement dans xterm.js, acquittées une fois affichées
// (contrôle de flux), et la saisie est envoyée dans l'ordre, regroupée tant
// qu'un envoi est en cours. L'interface (onglets, recherche, réglages, menus)
// est en Rust/Leptos et pilote chaque vue par les méthodes de TermView ; les
// vues lui signalent leurs changements par `onEvent(type, données)`.
(function () {
  "use strict";

  const FRAME_DATA = 0;
  const FRAME_EXIT = 1;
  const ACK_BATCH = 64 * 1024;
  const isWindows = /^Win/.test(navigator.platform || "");

  const tauri = () => window.__TAURI__.core;
  const invoke = (cmd, args) => tauri().invoke(cmd, args || {});

  // --------------------------------------------------------------- thèmes
  const THEMES = {
    dark: {
      background: "#16181d", foreground: "#d7dae0", cursor: "#61afef", cursorAccent: "#16181d",
      selectionBackground: "#3a4a63", black: "#1e2127", red: "#e06c75", green: "#98c379",
      yellow: "#e5c07b", blue: "#61afef", magenta: "#c678dd", cyan: "#56b6c2", white: "#abb2bf",
      brightBlack: "#5c6370", brightRed: "#ef7b85", brightGreen: "#a9d48a", brightYellow: "#f0cf8f",
      brightBlue: "#7cc0f5", brightMagenta: "#d48ae8", brightCyan: "#6cc8d3", brightWhite: "#ffffff",
    },
    light: {
      background: "#fbfbfc", foreground: "#2b303b", cursor: "#2f6fb3", cursorAccent: "#fbfbfc",
      selectionBackground: "#c5daf3", black: "#2b303b", red: "#c4314b", green: "#2f8a3a",
      yellow: "#a06a00", blue: "#2f6fb3", magenta: "#9a3fb5", cyan: "#13818e", white: "#c8ccd4",
      brightBlack: "#6b7280", brightRed: "#df4660", brightGreen: "#3aa547", brightYellow: "#c08400",
      brightBlue: "#3f86d4", brightMagenta: "#b453d1", brightCyan: "#1a9aa9", brightWhite: "#ffffff",
    },
    "one-dark": {
      background: "#282c34", foreground: "#abb2bf", cursor: "#528bff", cursorAccent: "#282c34",
      selectionBackground: "#3e4451", black: "#282c34", red: "#e06c75", green: "#98c379",
      yellow: "#e5c07b", blue: "#61afef", magenta: "#c678dd", cyan: "#56b6c2", white: "#abb2bf",
      brightBlack: "#5c6370", brightRed: "#e06c75", brightGreen: "#98c379", brightYellow: "#e5c07b",
      brightBlue: "#61afef", brightMagenta: "#c678dd", brightCyan: "#56b6c2", brightWhite: "#ffffff",
    },
    dracula: {
      background: "#282a36", foreground: "#f8f8f2", cursor: "#f8f8f2", cursorAccent: "#282a36",
      selectionBackground: "#44475a", black: "#21222c", red: "#ff5555", green: "#50fa7b",
      yellow: "#f1fa8c", blue: "#bd93f9", magenta: "#ff79c6", cyan: "#8be9fd", white: "#f8f8f2",
      brightBlack: "#6272a4", brightRed: "#ff6e6e", brightGreen: "#69ff94", brightYellow: "#ffffa5",
      brightBlue: "#d6acff", brightMagenta: "#ff92df", brightCyan: "#a4ffff", brightWhite: "#ffffff",
    },
    nord: {
      background: "#2e3440", foreground: "#d8dee9", cursor: "#d8dee9", cursorAccent: "#2e3440",
      selectionBackground: "#434c5e", black: "#3b4252", red: "#bf616a", green: "#a3be8c",
      yellow: "#ebcb8b", blue: "#81a1c1", magenta: "#b48ead", cyan: "#88c0d0", white: "#e5e9f0",
      brightBlack: "#4c566a", brightRed: "#bf616a", brightGreen: "#a3be8c", brightYellow: "#ebcb8b",
      brightBlue: "#81a1c1", brightMagenta: "#b48ead", brightCyan: "#8fbcbb", brightWhite: "#eceff4",
    },
    gruvbox: {
      background: "#282828", foreground: "#ebdbb2", cursor: "#ebdbb2", cursorAccent: "#282828",
      selectionBackground: "#504945", black: "#282828", red: "#cc241d", green: "#98971a",
      yellow: "#d79921", blue: "#458588", magenta: "#b16286", cyan: "#689d6a", white: "#a89984",
      brightBlack: "#928374", brightRed: "#fb4934", brightGreen: "#b8bb26", brightYellow: "#fabd2f",
      brightBlue: "#83a598", brightMagenta: "#d3869b", brightCyan: "#8ec07c", brightWhite: "#ebdbb2",
    },
    "solarized-dark": {
      background: "#002b36", foreground: "#93a1a1", cursor: "#93a1a1", cursorAccent: "#002b36",
      selectionBackground: "#0d4352", black: "#073642", red: "#dc322f", green: "#859900",
      yellow: "#b58900", blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5",
      brightBlack: "#586e75", brightRed: "#cb4b16", brightGreen: "#859900", brightYellow: "#b58900",
      brightBlue: "#268bd2", brightMagenta: "#6c71c4", brightCyan: "#2aa198", brightWhite: "#fdf6e3",
    },
    "solarized-light": {
      background: "#fdf6e3", foreground: "#586e75", cursor: "#586e75", cursorAccent: "#fdf6e3",
      selectionBackground: "#eee8d5", black: "#073642", red: "#dc322f", green: "#859900",
      yellow: "#b58900", blue: "#268bd2", magenta: "#d33682", cyan: "#2aa198", white: "#eee8d5",
      brightBlack: "#657b83", brightRed: "#cb4b16", brightGreen: "#859900", brightYellow: "#b58900",
      brightBlue: "#268bd2", brightMagenta: "#6c71c4", brightCyan: "#2aa198", brightWhite: "#fdf6e3",
    },
  };
  const LIGHT = new Set(["light", "solarized-light"]);
  const darkQuery = window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;

  function themeName(name) {
    if (THEMES[name]) return name;
    return darkQuery && !darkQuery.matches ? "light" : "dark";
  }

  function theme(name) {
    const t = Object.assign({}, THEMES[themeName(name)]);
    const light = LIGHT.has(themeName(name));
    t.scrollbarSliderBackground = light ? "rgba(0,0,0,0.18)" : "rgba(255,255,255,0.16)";
    t.scrollbarSliderHoverBackground = light ? "rgba(0,0,0,0.3)" : "rgba(255,255,255,0.28)";
    t.scrollbarSliderActiveBackground = light ? "rgba(0,0,0,0.4)" : "rgba(255,255,255,0.38)";
    return t;
  }

  /// Couleurs de la fenêtre (onglets, barres) accordées au thème du terminal.
  function applyChrome(name) {
    const t = THEMES[themeName(name)];
    const s = document.documentElement.style;
    s.setProperty("--term-bg", t.background);
    s.setProperty("--term-fg", t.foreground);
    s.setProperty("--term-accent", t.blue);
    s.setProperty("--term-dim", t.brightBlack);
    s.setProperty("--term-ok", t.green);
    s.setProperty("--term-warn", t.yellow);
    s.setProperty("--term-err", t.red);
    document.documentElement.dataset.termScheme = LIGHT.has(themeName(name)) ? "light" : "dark";
  }

  function options(prefs) {
    return {
      fontFamily: prefs.font_family,
      fontSize: prefs.font_size,
      lineHeight: prefs.line_height,
      cursorStyle: prefs.cursor_style,
      cursorBlink: prefs.cursor_blink,
      cursorInactiveStyle: "outline",
      scrollback: prefs.scrollback,
      theme: theme(prefs.theme),
    };
  }

  function utf8(bytes) {
    return new TextDecoder().decode(bytes);
  }

  function toBytes(msg) {
    if (msg instanceof ArrayBuffer) return new Uint8Array(msg);
    if (ArrayBuffer.isView(msg)) return new Uint8Array(msg.buffer, msg.byteOffset, msg.byteLength);
    if (Array.isArray(msg)) return Uint8Array.from(msg);
    return new Uint8Array(0);
  }

  // ------------------------------------------------------------------- vue
  class TermView {
    constructor(container, prefs, onEvent) {
      this.container = container;
      this.prefs = prefs;
      this.emit = (type, data) => {
        try { onEvent(type, data === undefined ? null : data); } catch (e) { console.error(e); }
      };
      this.id = null;
      this.state = "idle";
      this.visible = true;
      this.activity = false;
      this.outq = [];
      this.sending = false;
      this.pendingAck = 0;
      this.writesInFlight = 0;
      this.generation = 0;
      this.cwd = null;

      const term = new Terminal(Object.assign(options(prefs), {
        allowProposedApi: true,
        convertEol: false,
        drawBoldTextInBrightColors: true,
        minimumContrastRatio: 1,
        rightClickSelectsWord: false,
        macOptionIsMeta: true,
        smoothScrollDuration: 0,
        scrollOnUserInput: true,
        // liens OSC 8 : Ctrl+clic, comme les liens détectés
        linkHandler: {
          activate: (ev, uri) => this._openLink(ev, uri),
          hover: () => this._linkHint(true),
          leave: () => this._linkHint(false),
        },
      }));
      this.term = term;
      this.fitAddon = new FitAddon.FitAddon();
      term.loadAddon(this.fitAddon);
      this.search = new SearchAddon.SearchAddon({ highlightLimit: 2000 });
      term.loadAddon(this.search);
      term.loadAddon(new WebLinksAddon.WebLinksAddon((ev, uri) => this._openLink(ev, uri), {
        hover: () => this._linkHint(true),
        leave: () => this._linkHint(false),
      }));
      term.loadAddon(new Unicode11Addon.Unicode11Addon());
      term.unicode.activeVersion = "11";
      try {
        term.loadAddon(new ImageAddon.ImageAddon({ sixelSupport: true, iipSupport: true }));
      } catch (e) {
        console.warn("images (sixel) indisponibles", e);
      }
      term.open(container);
      this._renderer(prefs.gpu);

      term.onData((d) => this._input(d, false));
      term.onBinary((d) => this._input(d, true));
      term.onTitleChange((t) => this.emit("title", t));
      term.onBell(() => this.emit("bell"));
      term.onResize(({ cols, rows }) => this._resized(cols, rows));
      term.onSelectionChange(() => {
        clearTimeout(this.selTimer);
        this.selTimer = setTimeout(() => {
          if (this.prefs.copy_on_select && term.hasSelection()) this.copy();
        }, 120);
      });
      this.search.onDidChangeResults((r) => this.emit("search", { index: r.resultIndex, count: r.resultCount }));
      // copie vers le presse-papiers demandée par une application (OSC 52 : vim, tmux…)
      term.parser.registerOscHandler(52, (data) => {
        const b64 = data.slice(data.indexOf(";") + 1);
        if (b64 && b64 !== "?") {
          try {
            const bin = atob(b64);
            this._copyText(utf8(Uint8Array.from(bin, (c) => c.charCodeAt(0))));
          } catch (e) { /* données invalides */ }
        }
        return true;
      });
      // dossier courant annoncé par le shell (OSC 7) : nouvel onglet au même endroit
      term.parser.registerOscHandler(7, (data) => {
        const m = /^file:\/\/[^/]*(\/.*)$/.exec(data);
        if (m) {
          try { this.cwd = decodeURIComponent(m[1]); } catch (e) { this.cwd = m[1]; }
        }
        return false;
      });
      term.attachCustomKeyEventHandler((ev) => this._key(ev));

      container.addEventListener("contextmenu", (ev) => {
        ev.preventDefault();
        if (term.modes.mouseTrackingMode !== "none" && !ev.shiftKey) return;
        if (this.prefs.right_click_paste) {
          if (term.hasSelection()) {
            this.copy();
            term.clearSelection();
          } else {
            this.paste();
          }
          return;
        }
        this.emit("menu", { x: ev.clientX, y: ev.clientY, selection: term.hasSelection() });
      });
      // clic du milieu : coller (convention Linux)
      container.addEventListener("auxclick", (ev) => {
        if (ev.button === 1 && term.modes.mouseTrackingMode === "none") {
          ev.preventDefault();
          this.paste();
        }
      });
      // Ctrl+molette : taille de la police
      container.addEventListener("wheel", (ev) => {
        if (!ev.ctrlKey) return;
        ev.preventDefault();
        ev.stopPropagation();
        this.emit("key", ev.deltaY < 0 ? "zoom-in" : "zoom-out");
      }, { capture: true, passive: false });
      this.resizeObserver = new ResizeObserver(() => this.fit());
      this.resizeObserver.observe(container);
    }

    // ------------------------------------------------------ cycle de vie
    /// Ouvre le shell distant (ou le relance après la fin de la session).
    async start(session, cwd) {
      this.session = session;
      this.startCwd = cwd || null;
      const gen = ++this.generation;
      this.id = null;
      this.pendingAck = 0;
      this.outq = [];
      this._setState("connecting");
      this.fit(true);
      const channel = new (tauri().Channel)();
      channel.onmessage = (msg) => {
        if (gen === this.generation) this._frame(toBytes(msg));
      };
      try {
        const id = await invoke("term_open", {
          session_id: session,
          cols: this.term.cols,
          rows: this.term.rows,
          cwd: this.startCwd,
          on_data: channel,
        });
        if (gen !== this.generation) {
          invoke("term_close", { id });
          return;
        }
        this.id = id;
        if (this.state === "connecting") this._setState("running");
        this._flushAck(true);
        this._pump();
        // la taille a pu changer pendant l'ouverture
        this._resized(this.term.cols, this.term.rows);
      } catch (e) {
        if (gen !== this.generation) return;
        const message = (e && e.message) || String(e);
        this._setState("ended", { message, error: true, lost: e && e.kind !== "remote" });
      }
    }

    dispose() {
      this.generation++;
      if (this.id !== null && this.state === "running") invoke("term_close", { id: this.id });
      this.id = null;
      this.resizeObserver.disconnect();
      this.term.dispose();
    }

    _setState(state, info) {
      this.state = state;
      this.emit("state", Object.assign({ state }, info || {}));
    }

    _frame(bytes) {
      if (bytes.length === 0) return;
      if (bytes[0] === FRAME_DATA) {
        const data = bytes.subarray(1);
        const n = data.length;
        this.writesInFlight++;
        this.term.write(data, () => {
          this.writesInFlight--;
          this.pendingAck += n;
          this._flushAck(false);
        });
        if (!this.visible && !this.activity) {
          this.activity = true;
          this.emit("activity");
        }
      } else if (bytes[0] === FRAME_EXIT) {
        let exit = {};
        try { exit = JSON.parse(utf8(bytes.subarray(1))); } catch (e) { /* ignoré */ }
        this.id = null;
        // affiché après les dernières sorties
        this.term.write("", () => this._setState("ended", exit));
      }
    }

    _flushAck(force) {
      if (this.id === null || this.pendingAck === 0) return;
      if (force || this.pendingAck >= ACK_BATCH || this.writesInFlight === 0) {
        const bytes = this.pendingAck;
        this.pendingAck = 0;
        invoke("term_ack", { id: this.id, bytes }).catch(() => {});
      }
    }

    // ------------------------------------------------------------ saisie
    _input(data, binary) {
      if (this.state === "ended") {
        if (!binary && data.includes("\r")) this.start(this.session, this.startCwd);
        return;
      }
      const last = this.outq[this.outq.length - 1];
      if (last && last.binary === binary) last.data += data;
      else this.outq.push({ data, binary });
      this._pump();
    }

    /// Envois dans l'ordre : un seul à la fois, la saisie suivante est regroupée.
    async _pump() {
      if (this.sending || this.id === null) return;
      this.sending = true;
      try {
        while (this.outq.length && this.id !== null) {
          const { data, binary } = this.outq.shift();
          await invoke("term_write", { id: this.id, data, binary }).catch(() => {});
        }
      } finally {
        this.sending = false;
      }
    }

    /// Texte envoyé tel quel au shell (comme une frappe).
    sendText(text) {
      this._input(text, false);
    }

    _resized(cols, rows) {
      clearTimeout(this.resizeTimer);
      this.resizeTimer = setTimeout(() => {
        if (this.id !== null) invoke("term_resize", { id: this.id, cols, rows }).catch(() => {});
      }, 40);
    }

    /// Adapte lignes et colonnes au conteneur (`now` : sans attendre l'affichage).
    fit(now) {
      if (!this.visible || !this.container.offsetParent) return;
      const run = () => {
        try { this.fitAddon.fit(); } catch (e) { /* conteneur masqué */ }
      };
      if (now) return run();
      clearTimeout(this.fitTimer);
      this.fitTimer = setTimeout(run, 16);
    }

    // ----------------------------------------------------------- clavier
    _key(ev) {
      if (ev.type !== "keydown") return true;
      const ctrl = ev.ctrlKey || ev.metaKey;
      const k = ev.key;
      const lower = k.length === 1 ? k.toLowerCase() : k;
      let action = null;
      if (ctrl && ev.shiftKey && !ev.altKey) {
        action = {
          c: "copy", v: "paste", f: "search", t: "new-tab", w: "close-tab", a: "select-all",
          k: "clear", Tab: "prev-tab", PageUp: "move-tab-left", PageDown: "move-tab-right",
        }[lower] || null;
      } else if (ctrl && !ev.altKey) {
        if (k === "=" || k === "+") action = "zoom-in";
        else if (k === "-") action = "zoom-out";
        else if (k === "0") action = "zoom-reset";
        else if (k === "Tab" || k === "PageDown") action = "next-tab";
        else if (k === "PageUp") action = "prev-tab";
        else if (k === "Insert") action = "copy";
        // Windows : Ctrl+C copie s'il y a une sélection, Ctrl+V colle
        else if (isWindows && lower === "c" && this.term.hasSelection()) action = "copy-clear";
        else if (isWindows && lower === "v") action = "paste";
      } else if (ev.shiftKey && !ctrl && k === "Insert") {
        action = "paste";
      } else if (!ctrl && !ev.altKey && (k === "F3")) {
        action = ev.shiftKey ? "find-prev" : "find-next";
      }
      if (!action) return true;
      ev.preventDefault();
      if (action === "copy") this.copy();
      else if (action === "copy-clear") { this.copy(); this.term.clearSelection(); }
      else if (action === "paste") this.paste();
      else if (action === "select-all") this.term.selectAll();
      else if (action === "clear") this.clear();
      else this.emit("key", action);
      return false;
    }

    // ------------------------------------------------ presse-papiers, liens
    _copyText(text) {
      if (!text) return;
      if (window.__RUSTSCP_DEV__ && navigator.clipboard) {
        navigator.clipboard.writeText(text).catch(() => {});
      } else {
        invoke("copy_text", { text }).catch(() => {});
      }
    }

    copy() {
      const text = this.term.getSelection();
      if (text) this._copyText(text);
      return !!text;
    }

    async paste() {
      let text = "";
      try {
        text = window.__RUSTSCP_DEV__ && navigator.clipboard
          ? await navigator.clipboard.readText()
          : await invoke("paste_text");
      } catch (e) { /* presse-papiers indisponible */ }
      if (text) {
        this.term.paste(text);
        this.term.focus();
      }
    }

    _openLink(ev, uri) {
      if (!(ev.ctrlKey || ev.metaKey)) return;
      if (window.__RUSTSCP_DEV__) window.open(uri, "_blank", "noopener");
      else invoke("open_url", { url: uri }).catch((e) => console.warn(e));
    }

    _linkHint(on) {
      this.container.title = on ? this.prefs.link_hint || "" : "";
    }

    // ------------------------------------------------------------ divers
    show() {
      this.visible = true;
      this.activity = false;
      this.fit();
      this.focus();
    }

    hide() {
      this.visible = false;
    }

    focus() {
      this.term.focus();
    }

    selectAll() {
      this.term.selectAll();
    }

    hasSelection() {
      return this.term.hasSelection();
    }

    clear() {
      this.term.clear();
    }

    /// Ligne d'information (fin de session…) affichée dans le terminal.
    notice(text, kind) {
      const color = { error: "31", ok: "32", dim: "90" }[kind] || "90";
      const nl = this.term.buffer.active.cursorX > 0 ? "\r\n" : "";
      this.term.write(`${nl}\x1b[0;${color}m${text.replace(/\n/g, "\r\n")}\x1b[0m\r\n`);
    }

    /// Dossier courant si le shell l'annonce (OSC 7), sinon dossier de départ.
    currentDir() {
      return this.cwd || this.startCwd || null;
    }

    find(query, opts, backward) {
      if (!query) {
        this.search.clearDecorations();
        this.emit("search", { index: -1, count: 0 });
        return false;
      }
      const t = theme(this.prefs.theme);
      const o = {
        caseSensitive: !!opts.case_sensitive,
        regex: !!opts.regex,
        wholeWord: !!opts.whole_word,
        incremental: !!opts.incremental && !backward,
        decorations: {
          matchBackground: t.selectionBackground,
          matchOverviewRuler: t.yellow,
          activeMatchBackground: t.yellow,
          activeMatchBorder: t.yellow,
          activeMatchColorOverviewRuler: t.yellow,
        },
      };
      try {
        return backward ? this.search.findPrevious(query, o) : this.search.findNext(query, o);
      } catch (e) {
        return false; // expression régulière invalide
      }
    }

    clearSearch() {
      this.search.clearDecorations();
      this.term.clearSelection();
    }

    setPrefs(prefs) {
      const gpuChanged = prefs.gpu !== this.prefs.gpu;
      this.prefs = prefs;
      Object.assign(this.term.options, options(prefs));
      if (gpuChanged) this._renderer(prefs.gpu);
      this.fit();
    }

    _renderer(gpu) {
      if (this.webgl) {
        this.webgl.dispose();
        this.webgl = null;
      }
      if (!gpu) return;
      try {
        const webgl = new WebglAddon.WebglAddon();
        webgl.onContextLoss(() => {
          webgl.dispose();
          if (this.webgl === webgl) this.webgl = null;
        });
        this.term.loadAddon(webgl);
        this.webgl = webgl;
      } catch (e) {
        console.warn("WebGL indisponible : rendu DOM", e);
      }
    }

    renderer() {
      return this.webgl ? "webgl" : "dom";
    }
  }

  // développement : vues accessibles depuis la console (inspection, tests)
  const devViews = [];

  window.RustScpTerm = {
    create: (container, prefs, onEvent) => {
      const v = new TermView(container, prefs, onEvent);
      if (window.__RUSTSCP_DEV__) devViews.push(v);
      return v;
    },
    devViews,
    applyChrome,
    /// Suit le thème du système (thème « auto »).
    onSchemeChange: (cb) => {
      if (darkQuery) darkQuery.addEventListener("change", () => cb());
    },
  };
})();
