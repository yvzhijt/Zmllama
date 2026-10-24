use std::sync::LazyLock;

const XTERM_CSS: &str = include_str!("xterm.css");
const XTERM_JS: &str = include_str!("xterm.js");
const XTERM_FIT_JS: &str = include_str!("xterm-addon-fit.js");

static CSS_JSON: LazyLock<String> = LazyLock::new(|| serde_json::to_string(XTERM_CSS).unwrap());
static XTERM_JSON: LazyLock<String> = LazyLock::new(|| serde_json::to_string(XTERM_JS).unwrap());
static FIT_JSON: LazyLock<String> = LazyLock::new(|| serde_json::to_string(XTERM_FIT_JS).unwrap());

pub(super) const INIT_JS: &str = r#"
(function() {
    if (window.__xterm_loaded) return;
    window.__xterm_loaded = true;

    // 输入桥：xterm onData/onResize 由此发回 Rust
    window.__inputBridge = function(msg) { dioxus.send(msg); };

    function injectCode(id, code) {
        if (document.getElementById(id)) return;
        var sc = document.createElement('script');
        sc.id = id;
        sc.textContent = code;
        document.head.appendChild(sc);
    }

    try {
        if (!document.getElementById('xterm-css')) {
            var st = document.createElement('style');
            st.id = 'xterm-css';
            st.textContent = {{CSS}};
            document.head.appendChild(st);
        }
        injectCode('xterm-js', {{XTERM}});
        injectCode('xterm-fit', {{FIT}});

        var THEME_DARK = {
            background: '#11111b',
            foreground: '#cdd6f4',
            cursor: '#f5e0dc',
            cursorAccent: '#11111b',
            selectionBackground: '#585b70',
            black:  '#45475a',
            red:    '#f38ba8',
            green:  '#a6e3a1',
            yellow: '#f9e2af',
            blue:   '#89b4fa',
            magenta:'#f5c2e7',
            cyan:   '#94e2d5',
            white:  '#bac2de',
            brightBlack:  '#585b70',
            brightRed:    '#f38ba8',
            brightGreen:  '#a6e3a1',
            brightYellow: '#f9e2af',
            brightBlue:   '#89b4fa',
            brightMagenta:'#f5c2e7',
            brightCyan:   '#94e2d5',
            brightWhite:  '#a6adc8',
        };
        var THEME_LIGHT = {
            background: '#eff1f5',
            foreground: '#4c4f69',
            cursor: '#dc8a78',
            cursorAccent: '#eff1f5',
            selectionBackground: '#acb0be',
            black:  '#5c5f77',
            red:    '#d20f39',
            green:  '#40a02b',
            yellow: '#df8e1d',
            blue:   '#1e66f5',
            magenta:'#ea76cb',
            cyan:   '#179299',
            white:  '#acb0be',
            brightBlack:  '#6c6f85',
            brightRed:    '#d20f39',
            brightGreen:  '#40a02b',
            brightYellow: '#df8e1d',
            brightBlue:   '#1e66f5',
            brightMagenta:'#ea76cb',
            brightCyan:   '#179299',
            brightWhite:  '#bcc0cc',
        };
        window.__darkTheme = true;
        function currentTheme() {
            return window.__darkTheme ? THEME_DARK : THEME_LIGHT;
        }

        // 调试浮标：仅在显式开启时创建（默认关闭）
        function ensureDebug() {
            return null;
        }
        window.__debugTerm = function(fitErr) {
            var d = ensureDebug();
            if (!d) return;
            var body = document.getElementById('terminal');
            var act = window.TermMgr ? window.TermMgr.activeId : 0;
            var inst = act ? document.getElementById('term-' + act) : null;
            var entry = act && window.TermMgr.terms[act];
            var info = 'r=' + (entry ? entry.term.rows : '?') + 'x' + (entry ? entry.term.cols : '?') +
                ' xterm=' + (entry ? entry.term.element.clientHeight : 'x') +
                ' body=' + (body ? body.clientHeight : 'x') +
                ' recv=' + (window.__recvCount || 0) +
                ' w=' + (window.__writeCount || 0) +
                (fitErr ? ' FIT_ERR=' + fitErr : '');
            d.textContent = info;
        };
        // 统一的 fit 入口：成功/失败都可视化
        function doFit(t) {
            try {
                t.fit.fit();
                window.__debugTerm();
            } catch (e) {
                window.__debugTerm(e && e.message);
            }
        }

        window.TermMgr = {
            terms: {},
            activeId: 0,

            ensureContainer: function() {
                var c = document.getElementById('terminals');
                if (!c) {
                    c = document.createElement('div');
                    c.id = 'terminals';
                    var body = document.getElementById('terminal');
                    if (body) body.appendChild(c);
                }
                return c;
            },

            create: function(id) {
                var root = this.ensureContainer();
                var el = document.createElement('div');
                el.className = 'term-instance';
                el.id = 'term-' + id;
                // 先可见再 open，确保 xterm 以正确尺寸初始化
                el.style.display = 'block';
                root.appendChild(el);

                var term = new Terminal({
                    cursorBlink: true,
                    cursorStyle: 'bar',
                    fontSize: 14,
                    fontFamily: '"Cascadia Code", "Fira Code", "Consolas", "Microsoft YaHei", monospace',
                    theme: currentTheme(),
                    allowProposedApi: true,
                    scrollback: 5000,
                });
                var fit = new FitAddon.FitAddon();
                term.loadAddon(fit);
                term.open(el);

                var self = this;

                // 点击终端容器时确保 xterm textarea 获得焦点
                // mousedown 在用户手势上下文中执行，focus 100% 可靠
                el.addEventListener('mousedown', function() {
                    if (term.textarea) term.textarea.focus();
                    term.focus();
                });

                // open 后等一帧再 fit，确保 xterm 内部 DOM 已稳定
                // 注意：此处不 focus！focus 由 activate 负责（仅在 PSReadLine 就绪后调用）
                requestAnimationFrame(function() {
                    doFit(self.terms[id]);
                });

                var observer = new ResizeObserver(function() {
                    if (self.activeId === id) { doFit(self.terms[id]); }
                });
                observer.observe(el);

                // 窗口大小变化也重新 fit
                window.addEventListener('resize', function() {
                    doFit(self.terms[id]);
                });

                term.onData(function(d) {
                    if (window.__inputBridge) window.__inputBridge({ t: 'in', id: id, data: d });
                });
                term.onResize(function(e) {
                    if (window.__inputBridge) window.__inputBridge({ t: 'rs', id: id, cols: e.cols, rows: e.rows });
                });

                this.terms[id] = { term: term, fit: fit, el: el, observer: observer };

                // 如果不是当前活跃 tab，隐藏之
                if (this.activeId !== 0 && this.activeId !== id) {
                    el.style.display = 'none';
                }
                return id;
            },

            activate: function(id) {
                var keys = Object.keys(this.terms);
                for (var i = 0; i < keys.length; i++) {
                    var k = keys[i];
                    var t = this.terms[k];
                    t.el.style.display = (parseInt(k) === id) ? 'block' : 'none';
                }
                this.activeId = id;
                var cur = this.terms[id];
                if (cur) {
                    void cur.el.offsetHeight;
                    doFit(cur);
                    requestAnimationFrame(function() { doFit(cur); });
                    setTimeout(function() { doFit(cur); }, 50);
                    setTimeout(function() { doFit(cur); }, 200);
                    setTimeout(function() { doFit(cur); }, 600);

                    // 聚焦：优先走 textarea（xterm.js allowProposedApi 暴露），
                    // 在非用户手势上下文中 textarea.focus() 比 term.focus() 更可靠
                    function focusTerm(t) {
                        if (t.term.textarea) t.term.textarea.focus();
                        t.term.focus();
                    }
                    focusTerm(cur);
                    // 下一帧验证焦点是否真的落在 textarea 上
                    requestAnimationFrame(function() {
                        if (cur.term.textarea && document.activeElement !== cur.term.textarea) {
                            cur.term.textarea.focus();
                        }
                    });
                }
            },

            write: function(id, data) {
                var t = this.terms[id];
                if (t) {
                    window.__writeCount = (window.__writeCount || 0) + 1;
                    t.term.write(data);
                    window.__debugTerm();
                }
            },

            dispose: function(id) {
                var t = this.terms[id];
                if (!t) return;
                try { t.observer.disconnect(); } catch (_) {}
                try { t.term.dispose(); } catch (_) {}
                if (t.el && t.el.parentNode) t.el.parentNode.removeChild(t.el);
                delete this.terms[id];
                if (this.activeId === id) {
                    var keys = Object.keys(this.terms);
                    if (keys.length > 0) this.activate(parseInt(keys[0]));
                    else this.activeId = 0;
                }
            },

            setTheme: function(dark) {
                window.__darkTheme = !!dark;
                var t = currentTheme();
                var keys = Object.keys(this.terms);
                for (var i = 0; i < keys.length; i++) {
                    this.terms[keys[i]].term.options.theme = t;
                }
            },

            setFontSize: function(size) {
                var keys = Object.keys(this.terms);
                for (var i = 0; i < keys.length; i++) {
                    this.terms[keys[i]].term.options.fontSize = size;
                }
            },
        };

        // 接收 Rust 命令并路由到 TermMgr
        (async function() {
            while (true) {
                var msg = await dioxus.recv();
                if (msg === null || msg === undefined) break;
                window.__recvCount = (window.__recvCount || 0) + 1;
                if (!window.TermMgr) continue;
                switch (msg.t) {
                    case 'create':   window.TermMgr.create(msg.id); break;
                    case 'activate': window.TermMgr.activate(msg.id); break;
                    case 'dispose':  window.TermMgr.dispose(msg.id); break;
                    case 'dead':     window.TermMgr.dispose(msg.id); break;
                    case 'out':      window.TermMgr.write(msg.id, msg.data); break;
                    case 'theme':    window.TermMgr.setTheme(msg.dark); break;
                    case 'fontSize': window.TermMgr.setFontSize(msg.size); break;
                }
            }
        })();
    } catch (e) {
        window.__xterm_loaded = false;
        console.error('xterm init failed:', e);
    }
})();
"#;

pub(super) fn build_init_script() -> String {
    INIT_JS
        .replace("{{CSS}}", &*CSS_JSON)
        .replace("{{XTERM}}", &*XTERM_JSON)
        .replace("{{FIT}}", &*FIT_JSON)
}