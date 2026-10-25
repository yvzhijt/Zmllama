use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use super::state::{workspace_path, TASK_CANCEL};
use super::truncate_output;

// ── 持久后台 Shell（保持 cwd/环境变量，捕获输出）────────────────

static SHELL_SEQ: AtomicU64 = AtomicU64::new(1);

struct PersistShell {
    stdin: std::process::ChildStdin,
    rx: futures_channel::mpsc::UnboundedReceiver<String>,
    child: std::process::Child,
    counter: u64,
}

impl Drop for PersistShell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static PSH: LazyLock<Mutex<Option<PersistShell>>> = LazyLock::new(|| Mutex::new(None));

/// 实时输出通道：执行期间把每一行推送给 UI 侧 writer
pub static LIVE_TX: LazyLock<Mutex<Option<futures_channel::mpsc::UnboundedSender<String>>>> =
    LazyLock::new(|| Mutex::new(None));

fn push_live(line: &str) {
    if let Some(tx) = LIVE_TX.lock().unwrap().as_ref() {
        let _ = tx.unbounded_send(line.to_string());
    }
}

/// 非阻塞地等待 shell 的下一行（轮询，支持取消与超时）。
fn wait_line(
    rx: &mut futures_channel::mpsc::UnboundedReceiver<String>,
    timeout: Duration,
) -> Result<Option<String>, String> {
    use futures_channel::mpsc::TryRecvError;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match rx.try_recv() {
            Ok(line) => return Ok(Some(line)),
            Err(TryRecvError::Empty) => {
                if std::time::Instant::now() > deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(_) => return Err("Shell 输出通道已断开".into()),
        }
    }
}

/// 文本 → 十六进制，避免经 stdin 传递时的引号、换行和编码问题
fn to_hex(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(s.len() * 2);
    for &b in s.as_bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// 生成 PowerShell 前缀：把 UTF-8 文本解码进变量 $b
fn ps_utf8_var(s: &str) -> String {
    let len = s.len();
    format!(
        "$b=New-Object byte[] {len}; for($i=0;$i -lt {len};$i++){{$b[$i]=[Convert]::ToByte('{hex}'.Substring($i*2,2),16)}}; ",
        len = len,
        hex = to_hex(s),
    )
}

/// 持续按行读取子进程输出；非法 UTF-8 不会中断读取
fn pump_lines<R: Read>(reader: R, tx: futures_channel::mpsc::UnboundedSender<String>) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let mut line = String::from_utf8_lossy(&buf).into_owned();
                while line.ends_with('\n') || line.ends_with('\r') {
                    line.pop();
                }
                if tx.unbounded_send(line).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn spawn_persist_shell() -> Result<PersistShell, String> {
    let ws = workspace_path();
    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args(["-NoLogo", "-NoProfile", "-Command", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if ws.is_dir() {
        cmd.current_dir(&ws);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.spawn().map_err(|e| format!("启动 shell 失败：{e}"))?;
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    let (tx, rx) = futures_channel::mpsc::unbounded();
    let tx_err = tx.clone();
    std::thread::spawn(move || pump_lines(stdout, tx));
    std::thread::spawn(move || pump_lines(stderr, tx_err));

    let mut shell = PersistShell {
        stdin,
        rx,
        child,
        counter: 0,
    };

    let init = concat!(
        "$OutputEncoding=[Console]::OutputEncoding=[Text.Encoding]::UTF8; ",
        "$PSDefaultParameterValues['Out-File:Encoding']='utf8'; ",
        "chcp 65001 > $null; $ProgressPreference='SilentlyContinue'\r\n",
        "[Console]::Out.WriteLine('___PSH_READY___')\r\n",
        "[Console]::Out.Flush()\r\n"
    );
    shell
        .write_cmd(init)
        .map_err(|e| format!("初始化 shell 失败：{e}"))?;
    shell
        .wait_for_marker("___PSH_READY___", Duration::from_secs(10))
        .map_err(|e| format!("shell 未就绪：{e}"))?;

    Ok(shell)
}

impl PersistShell {
    fn write_cmd(&mut self, text: &str) -> Result<(), String> {
        self.stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("写入失败：{e}"))?;
        self.stdin.flush().map_err(|e| format!("写入失败：{e}"))
    }

    fn wait_for_marker(&mut self, marker: &str, timeout: Duration) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if TASK_CANCEL.load(Ordering::SeqCst) {
                return Err("已取消".into());
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err("等待 shell 响应超时".into());
            }
            match wait_line(&mut self.rx, left.min(Duration::from_millis(200)))? {
                Some(line) => {
                    if line.trim() == marker {
                        return Ok(());
                    }
                }
                None => continue,
            }
        }
    }

    fn run(
        &mut self,
        command: &str,
        cwd: Option<&str>,
    ) -> Result<(String, Option<i32>), String> {
        self.counter += 1;
        let n = self.counter;
        let nonce = SHELL_SEQ.fetch_add(1, Ordering::SeqCst);

        if let Some(dir) = cwd {
            let cd_marker = format!("___PSH_CD_{nonce}_{n}___");
            let cd = format!(
                "{}Set-Location -LiteralPath ([Text.Encoding]::UTF8.GetString($b))\r\n[Console]::Out.WriteLine('{cd_marker}')\r\n[Console]::Out.Flush()\r\n",
                ps_utf8_var(dir),
            );
            self.write_cmd(&cd)?;
            self.wait_for_marker(&cd_marker, Duration::from_secs(10))?;
        }

        let marker = format!("___PSH_END_{nonce}_{n}___");
        let full = format!(
            "$global:LASTEXITCODE=$null; $err0=$Error.Count; {}try {{ Invoke-Expression ([Text.Encoding]::UTF8.GetString($b)) }} finally {{ $ok=$?; $c=0; if($null -ne $LASTEXITCODE){{$c=[int]$LASTEXITCODE}}elseif(-not $ok -or $Error.Count -gt $err0){{$c=1}}; [Console]::Out.WriteLine('{marker}:' + $c); [Console]::Out.Flush() }}\r\n",
            ps_utf8_var(command),
        );
        self.write_cmd(&full)?;

        let mut output = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(60);

        let exit_code = 'read: loop {
            if TASK_CANCEL.load(Ordering::SeqCst) {
                return Err(format!("已取消\n{output}"));
            }
            if let Ok(Some(_)) = self.child.try_wait() {
                return Err(format!("Shell 进程已退出\n{output}"));
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Err(format!("命令超时（60s）：{command}"));
            }
            match wait_line(&mut self.rx, left.min(Duration::from_millis(200)))? {
                Some(line) => {
                    let trimmed = line.trim();
                    if trimmed.starts_with(marker.as_str()) && trimmed.contains(':') {
                        let rest = trimmed[marker.len()..].trim_start_matches(':').trim();
                        break 'read rest.parse::<i32>().ok();
                    }
                    if let Some(pos) = trimmed.find(&(marker.clone() + ":")) {
                        let before = trimmed[..pos].trim_end();
                        if !before.is_empty() {
                            push_live(before);
                            output.push_str(before);
                            output.push('\n');
                        }
                        let rest = &trimmed[pos + marker.len() + 1..];
                        break 'read rest.trim().parse::<i32>().ok();
                    }
                    push_live(&line);
                    output.push_str(&line);
                    output.push('\n');
                }
                None => continue,
            }
        };

        std::thread::sleep(Duration::from_millis(30));
        while let Ok(line) = self.rx.try_recv() {
            push_live(&line);
            output.push_str(&line);
            output.push('\n');
        }

        let shown = if output.trim().is_empty() {
            "（无输出）".to_string()
        } else {
            truncate_output(output.trim())
        };
        Ok((shown, exit_code))
    }
}

/// 把 echo "..." > file 转成 Set-Content（修复 \n 转义和 UTF-16 BOM）
fn convert_echo_write(cmd: &str) -> String {
    let trimmed = cmd.trim();
    if !trimmed.to_lowercase().starts_with("echo ") {
        return cmd.to_string();
    }
    // 找最后一个 > 后面不是 $null / /dev/null / nul 的
    let gt_pos = match trimmed.rfind(" > ") {
        Some(p) => p,
        None => return cmd.to_string(),
    };
    let file_part = trimmed[gt_pos + 3..].trim();
    if file_part.starts_with('$')
        || file_part.starts_with("/dev/")
        || file_part.to_lowercase().starts_with("nul")
    {
        return cmd.to_string();
    }
    let file_path = file_part.trim_end_matches(|c: char| c == ' ' || c == ';');

    // 提取 echo 参数（去掉 echo 前缀和 >file 后缀）
    let content_part = trimmed[5..gt_pos].trim();
    // 去掉外层引号
    let inner = if (content_part.starts_with('"') && content_part.ends_with('"'))
        || (content_part.starts_with('\'') && content_part.ends_with('\''))
    {
        &content_part[1..content_part.len() - 1]
    } else {
        content_part
    };
    // 反转义：\"→" , \n→换行 , \\→\
    let unescaped = inner
        .replace("\\\"", "\"")
        .replace("\\n", "\n")
        .replace("\\\\", "\\");

    format!(
        "Set-Content -Path {file} -Value @\"\n{content}\n\"@ -Encoding UTF8",
        file = file_path,
        content = unescaped,
    )
}

/// 自动补脚本解释器：s.py → python s.py
fn prepend_interpreter(cmd: &str) -> String {
    let trimmed = cmd.trim();
    let lower = trimmed.to_lowercase();
    // 已经带了解释器就不补
    if lower.starts_with("python ")
        || lower.starts_with("python3 ")
        || lower.starts_with("node ")
        || lower.starts_with("ruby ")
        || lower.starts_with("perl ")
        || trimmed.contains(' ')
        || trimmed.contains('>')
        || trimmed.contains('|')
    {
        return cmd.to_string();
    }
    if lower.ends_with(".py") {
        return format!("python {trimmed}");
    }
    if lower.ends_with(".js") {
        return format!("node {trimmed}");
    }
    if lower.ends_with(".rb") {
        return format!("ruby {trimmed}");
    }
    cmd.to_string()
}

/// 在持久 shell 中执行命令（自动管理生命周期）
pub fn psh_run(command: &str, cwd: Option<&str>) -> Result<(String, Option<i32>), String> {
    // 预处理：echo "..." > file → Set-Content（修复转义和编码问题）
    let command = convert_echo_write(&command);
    // 补脚本解释器：s.py → python s.py
    let command = prepend_interpreter(&command);

    // 预处理：小模型常输出 bash/Linux 语法，自动替换为 PowerShell 兼容形式
    let command = command
        // ls 参数（ls -la / ls -l → dir）
        .replace("ls -la", "dir")
        .replace("ls -l ", "dir ")
        .replace("ls -l;", "dir;")
        .replace("ls -l", "dir")
        // bash 逻辑运算符 → 顺序执行
        .replace(" || ", "; ")
        .replace("|| ", "; ")
        .replace(" ||", ";")
        .replace("||", ";")
        .replace(" && ", "; ")
        .replace("&& ", "; ")
        .replace(" &&", "; ")
        .replace("&&", ";")
        // stderr 重定向
        .replace(" 2>/dev/null", " 2>$null")
        .replace(" 2>&1", " 2>&1");

    let mut guard = PSH.lock().unwrap();
    let mut shell = match guard.take() {
        Some(s) => s,
        None => spawn_persist_shell()?,
    };
    let result = shell.run(&command, cwd);
    match result {
        Ok(output) => {
            *guard = Some(shell);
            Ok(output)
        }
        Err(e) => {
            drop(shell);
            Err(e)
        }
    }
}