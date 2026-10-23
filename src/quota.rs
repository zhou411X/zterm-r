//! Read subscription limits through the installed Codex CLI protocol.
//! Remote probes run the same CLI over SSH and never inspect credential files.

use crate::{
    credentials,
    model::{self, Server},
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(25);
const MAX_RESPONSE: usize = 1024 * 1024;

pub fn fetch(id: &str, profile: Option<&Server>) -> Result<String, String> {
    let remote = !id.is_empty();
    let mut command = if remote {
        let ssh = model::ssh_command(id, profile)?;
        let mut command = Command::new(&ssh[0]);
        command.args([
            "-T",
            if profile.is_some_and(|p| p.password_saved) {
                "-oBatchMode=no"
            } else {
                "-oBatchMode=yes"
            },
            "-oStrictHostKeyChecking=yes",
            "-oConnectTimeout=5",
        ]);
        command.args(&ssh[1..]);
        command.arg("if command -v codex >/dev/null 2>&1; then exec codex app-server; elif [ -x \"$HOME/.local/bin/codex\" ]; then exec \"$HOME/.local/bin/codex\" app-server; else exec \"$HOME/bin/codex\" app-server; fi");
        for (key, value) in credentials::ssh_environment(id, profile, true) {
            command.env(key, value);
        }
        command
    } else {
        let executable = std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|p| p.join("codex"))
                    .find(|p| p.is_file())
            })
            .or_else(|| {
                let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
                [home.join(".local/bin/codex"), home.join("bin/codex")]
                    .into_iter()
                    .find(|p| p.is_file())
            })
            .unwrap_or_else(|| "codex".into());
        let mut command = Command::new(executable);
        command.arg("app-server");
        command
    };
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Own a process group so a timed-out SSH child and its descendants are reaped.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command.spawn().map_err(|_| {
        if remote {
            "SSH 无法启动"
        } else {
            "未安装或无法启动 Codex CLI"
        }
        .to_string()
    })?;
    let result = query(&mut child, TIMEOUT);
    stop_process_group(&mut child);
    result.map(|windows| display(if remote { id } else { "本机" }, &windows))
}

#[derive(Debug, PartialEq)]
struct Window {
    label: String,
    used: f64,
    reset: Option<u64>,
}

fn finite_number(value: Option<&Value>) -> Option<f64> {
    let value = value?.as_f64()?;
    value.is_finite().then_some(value)
}

fn duration_label(minutes: f64, fallback: &str) -> String {
    if minutes <= 0.0 {
        return fallback.into();
    }
    let (value, unit) = if minutes % 1440.0 == 0.0 {
        (minutes / 1440.0, "d")
    } else if minutes % 60.0 == 0.0 {
        (minutes / 60.0, "h")
    } else {
        (minutes, "m")
    };
    format!("{value}{unit}")
}

fn codex_windows(result: &Value) -> Vec<Window> {
    let limits = result
        .pointer("/rateLimitsByLimitId/codex")
        .filter(|v| v.as_object().is_some_and(|limits| !limits.is_empty()))
        .or_else(|| result.get("rateLimits").filter(|v| v.is_object()));
    let Some(limits) = limits else {
        return Vec::new();
    };
    [("primary", "主额度"), ("secondary", "次额度")]
        .into_iter()
        .filter_map(|(key, fallback)| {
            let value = limits.get(key)?.as_object()?;
            let used = finite_number(value.get("usedPercent"))?;
            if !(0.0..=100.0).contains(&used) {
                return None;
            }
            let reset = match value.get("resetsAt") {
                None | Some(Value::Null) => None,
                other => {
                    let seconds = finite_number(other)?;
                    if !(0.0..=253402300799.0).contains(&seconds) {
                        return None;
                    }
                    Some(seconds as u64)
                }
            };
            let minutes = finite_number(value.get("windowDurationMins")).unwrap_or(0.0);
            Some(Window {
                label: duration_label(minutes, fallback),
                used,
                reset,
            })
        })
        .collect()
}

fn display(name: &str, windows: &[Window]) -> String {
    if windows.is_empty() {
        return format!("{name} · 账号未提供订阅额度");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let values = windows
        .iter()
        .map(|window| {
            let used = format!("{}", window.used);
            match window.reset {
                Some(reset) if reset <= now => format!("{} 待刷新", window.label),
                Some(reset) => {
                    let minutes = (reset - now).div_ceil(60);
                    if minutes >= 60 {
                        format!(
                            "{} 已用 {used}%（约{}h 后重置）",
                            window.label,
                            minutes.div_ceil(60)
                        )
                    } else {
                        format!("{} 已用 {used}%（约{minutes}m 后重置）", window.label)
                    }
                }
                None => format!("{} 已用 {used}%（重置时间未知）", window.label),
            }
        })
        .collect::<Vec<_>>();
    format!("{name} · {}", values.join(" · "))
}

fn send(child: &mut Child, message: Value) -> Result<(), String> {
    let input = child.stdin.as_mut().ok_or("CLI 输入已关闭")?;
    serde_json::to_writer(&mut *input, &message).map_err(|_| "CLI 请求编码失败")?;
    input
        .write_all(b"\n")
        .and_then(|_| input.flush())
        .map_err(|_| "CLI 连接已断开".into())
}

fn read_lines(mut output: impl Read, sender: mpsc::Sender<Result<Value, String>>) {
    let mut chunk = [0u8; 4096];
    let mut pending = Vec::new();
    loop {
        match output.read(&mut chunk) {
            Ok(0) => {
                let _ = sender.send(Err("CLI 连接已断开".into()));
                break;
            }
            Ok(count) => {
                for byte in &chunk[..count] {
                    if *byte == b'\n' {
                        if let Ok(value) = serde_json::from_slice::<Value>(&pending) {
                            let _ = sender.send(Ok(value));
                        }
                        pending.clear();
                    } else {
                        pending.push(*byte);
                        if pending.len() > MAX_RESPONSE {
                            let _ = sender.send(Err("CLI 响应过大".into()));
                            return;
                        }
                    }
                }
            }
            Err(_) => {
                let _ = sender.send(Err("CLI 读取失败".into()));
                break;
            }
        }
    }
}

fn receive(
    receiver: &Receiver<Result<Value, String>>,
    id: u64,
    deadline: Instant,
) -> Result<Value, String> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("查询超时")?;
        let response = receiver
            .recv_timeout(remaining)
            .map_err(|_| "查询超时".to_string())??;
        if response.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(response);
        }
    }
}

fn query(child: &mut Child, timeout: Duration) -> Result<Vec<Window>, String> {
    let stdout = child.stdout.take().ok_or("CLI 输出不可用")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || read_lines(stdout, sender));
    let deadline = Instant::now() + timeout;
    send(
        child,
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"zterm","version":"0.3.0"}}}),
    )?;
    if receive(&receiver, 1, deadline)?.get("error").is_some() {
        return Err("CLI 协议不兼容".into());
    }
    send(child, json!({"method":"initialized"}))?;
    send(child, json!({"id":2,"method":"account/rateLimits/read"}))?;
    let response = receive(&receiver, 2, deadline)?;
    if response.get("error").is_some() {
        return Err("无法获取：请检查 CLI 登录或网络".into());
    }
    Ok(codex_windows(
        response.get("result").ok_or("CLI 响应格式不正确")?,
    ))
}

fn stop_process_group(child: &mut Child) {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGTERM);
    }
    for _ in 0..20 {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_notifications_and_invalid_windows() {
        let bytes = b"{\"method\":\"account/updated\"}\n{\"id\":2,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":0,\"windowDurationMins\":300,\"resetsAt\":2000},\"secondary\":{\"usedPercent\":101}}}}\n";
        let (sender, receiver) = mpsc::channel();
        read_lines(&bytes[..], sender);
        let result = receive(&receiver, 2, Instant::now() + Duration::from_secs(1)).unwrap();
        assert_eq!(
            codex_windows(&result["result"]),
            vec![Window {
                label: "5h".into(),
                used: 0.0,
                reset: Some(2000)
            }]
        );
        assert!(display("本机", &codex_windows(&result["result"])).contains("待刷新"));
    }

    #[test]
    fn rejects_unexpected_response_size() {
        let (sender, receiver) = mpsc::channel();
        read_lines(vec![b'x'; MAX_RESPONSE + 1].as_slice(), sender);
        assert_eq!(receiver.recv().unwrap().unwrap_err(), "CLI 响应过大");
    }
}
