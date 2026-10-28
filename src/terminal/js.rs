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
            background: '#050b1c',
            foreground: '#dbe3f5',
            cursor: '#e2cfaa',
            cursorAccent: '#050b1c',
            selectionBackground: '#31457c',
            black:  '#172347',
            red:    '#ff8e98',
            green:  '#79cca4',
            yellow: '#f0cf8c',
            blue:   '#9bb0e1',
            magenta:'#c9a6e0',
            cyan:   '#8ed6e0',
            white:  '#e7ecf7',
            brightBlack:  '#526aa8',
            brightRed:    '#ff8e98',
            brightGreen:  '#79cca4',
            brightYellow: '#e2cfaa',
            brightBlue:   '#b8c8f0',
            brightMagenta:'#c9a6e0',
            brightCyan:   '#8ed6e0',
            brightWhite:  '#f6f9ff',
        };
        var THEME_LIGHT = {
            background: '#eef2fa',
            foreground: '#172347',
            cursor: '#526aa8',
            cursorAccent: '#eef2fa',
            selectionBackground: '#b9c8e6',
            black:  '#172347',
            red:    '#c52d3b',
            green:  '#247650',
            yellow: '#815c18',
            blue:   '#405a99',
            magenta:'#7a4f9e',
            cyan:   '#0f7280',
            white:  '#63769d',
            brightBlack:  '#3c4d75',
            brightRed:    '#c52d3b',
            brightGreen:  '#247650',
            brightYellow: '#9a7c42',
            brightBlue:   '#405a99',
            brightMagenta:'#7a4f9e',
            brightCyan:   '#0f7280',
            brightWhite:  '#172347',
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
                    fontSize: 16,
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
                    // DEBUG: 追踪第一个输入字符是否到达
                    window.__inputBridge({ t: 'dbg', msg: 'onData id=' + id + ' data=' + JSON.stringify(d) + ' len=' + d.length });
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
                    case 'agent_out':
                        var aid = window.TermMgr.activeId;
                        if (aid) window.TermMgr.write(aid, msg.data);
                        break;
                    
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
        .replace("{{CSS}}", &CSS_JSON)
        .replace("{{XTERM}}", &XTERM_JSON)
        .replace("{{FIT}}", &FIT_JSON)
}