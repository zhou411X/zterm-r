use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn config_dir() -> PathBuf {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    root.join("ssh-workbench")
}

pub fn save_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("配置路径没有父目录")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let serialized = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let mut attempt = 0u32;
    let (temporary, mut file) = loop {
        let temporary = parent.join(format!(
            ".settings-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 10 => attempt += 1,
            Err(e) => return Err(e.to_string()),
        }
    };
    let result = (|| {
        file.write_all(&serialized).map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn valid_alias(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub username: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "is_false")]
    pub password_saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Server {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.trim().chars().count() > 80
            || self.name.chars().any(|c| c.is_control())
            || !valid_alias(&self.username)
            || self.port == 0
        {
            return Err("服务器信息无效：请填写别名、IP、用户名和 1–65535 的端口。".into());
        }
        if self.host.parse::<IpAddr>().is_err() {
            return Err("请输入有效的 IPv4 或 IPv6 地址。".into());
        }
        if let Some(id) = &self.credential_id {
            if !self.password_saved || !valid_alias(id) {
                return Err("密码引用无效。".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub servers: BTreeMap<String, Server>,
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for alias in &self.aliases {
            if !valid_alias(alias) || !seen.insert(alias) {
                return Err("别名配置格式错误".into());
            }
        }
        for (id, server) in &self.servers {
            if !seen.contains(id) {
                return Err("服务器配置格式错误".into());
            }
            server.validate()?;
        }
        Ok(())
    }
    pub fn load() -> Result<Self, String> {
        Self::load_from(&config_dir().join("servers.json"))
    }
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        let config: Self = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_dir().join("servers.json"))
    }
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        save_json(path, self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appearance {
    pub font: String,
    pub theme: String,
    #[serde(default = "default_interface_theme")]
    pub interface_theme: String,
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    #[serde(default = "one")]
    pub background_opacity: f64,
    #[serde(default)]
    pub background_image: String,
    #[serde(default = "quarter")]
    pub background_image_opacity: f64,
    #[serde(default)]
    pub background_blur: f64,
    #[serde(default)]
    pub background_on_terminal: bool,
}
fn default_interface_theme() -> String {
    "follow-terminal".into()
}
fn one() -> f64 {
    1.0
}
fn quarter() -> f64 {
    0.25
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            font: "Monospace 11".into(),
            theme: "ubuntu-default".into(),
            interface_theme: default_interface_theme(),
            foreground: "#ffffff".into(),
            background: "#300a24".into(),
            cursor: "#ffffff".into(),
            background_opacity: one(),
            background_image: String::new(),
            background_image_opacity: quarter(),
            background_blur: 0.0,
            background_on_terminal: false,
        }
    }
}

fn valid_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].bytes().all(|c| c.is_ascii_hexdigit())
}

impl Appearance {
    pub fn validate(&self) -> Result<(), String> {
        if self.font.trim().is_empty()
            || !themes().contains_key(&self.theme)
            || !(self.interface_theme == "follow-terminal"
                || self.interface_theme == "system"
                || themes().contains_key(&self.interface_theme))
            || !valid_color(&self.foreground)
            || !valid_color(&self.background)
            || !valid_color(&self.cursor)
        {
            return Err("外观配置格式错误".into());
        }
        if !(0.15..=1.0).contains(&self.background_opacity)
            || !(0.0..=1.0).contains(&self.background_image_opacity)
            || !(0.0..=40.0).contains(&self.background_blur)
        {
            return Err("无效的背景设置".into());
        }
        if self.background_image.contains('\0')
            || (!self.background_image.is_empty()
                && !Path::new(&self.background_image).is_absolute())
        {
            return Err("无效的背景图片设置".into());
        }
        Ok(())
    }
    pub fn load() -> Result<Self, String> {
        Self::load_from(&config_dir().join("appearance.json"))
    }
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        let mut value: serde_json::Value =
            serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        migrate_theme(&mut value);
        let appearance: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        appearance.validate()?;
        Ok(appearance)
    }
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&config_dir().join("appearance.json"))
    }
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        save_json(path, self)
    }
}

fn migrate_theme(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let Some(old) = map.get("theme").and_then(|v| v.as_str()) else {
        return;
    };
    let (new, previous) = match old {
        "classic" => ("ubuntu-default", ["#eeeeec", "#241f31", "#eeeeec"]),
        "dark" => ("gnome-gray-on-black", ["#e5e7eb", "#15181e", "#93c5fd"]),
        "light" => ("gnome-black-on-white", ["#24292f", "#fafafa", "#0969da"]),
        "ocean" => ("ubuntu-default", ["#d8e7f3", "#102332", "#7dd3fc"]),
        _ => return,
    };
    let theme = themes().remove(new).expect("built-in theme exists");
    map.insert("theme".into(), new.into());
    for (key, expected, replacement) in [
        ("foreground", previous[0], theme.foreground),
        ("background", previous[1], theme.background),
        ("cursor", previous[2], theme.cursor),
    ] {
        if map
            .get(key)
            .and_then(|v| v.as_str())
            .is_some_and(|v| v.eq_ignore_ascii_case(expected))
        {
            map.insert(key.into(), replacement.into());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub palette: Vec<String>,
    #[serde(default)]
    pub indexed_colors: BTreeMap<u16, String>,
    #[serde(default)]
    pub accent: Option<String>,
}

pub fn themes() -> BTreeMap<String, Theme> {
    serde_json::from_str(include_str!("../assets/themes.json")).expect("valid built-in themes")
}

pub fn terminal_palette(name: &str) -> Vec<String> {
    let Some(theme) = themes().remove(name) else {
        return Vec::new();
    };
    let mut palette = theme.palette;
    let levels = [0, 95, 135, 175, 215, 255];
    for red in levels {
        for green in levels {
            for blue in levels {
                palette.push(format!("#{red:02x}{green:02x}{blue:02x}"));
            }
        }
    }
    for value in (8..=238).step_by(10) {
        palette.push(format!("#{value:02x}{value:02x}{value:02x}"));
    }
    for (index, color) in theme.indexed_colors {
        palette[index as usize] = color;
    }
    palette
}

#[derive(Clone)]
struct SshAlias {
    host: String,
    user: Option<String>,
    options: Vec<String>,
}

fn parse_ssh_alias(command: &str) -> Option<SshAlias> {
    if command
        .chars()
        .any(|c| matches!(c, '$' | '`' | ';' | '&' | '|' | '<' | '>' | '\n'))
    {
        return None;
    }
    let args = shell_words::split(command).ok()?;
    if !matches!(
        args.first().map(String::as_str),
        Some("ssh" | "/usr/bin/ssh")
    ) {
        return None;
    }
    let (mut options, mut user, mut host) = (Vec::new(), None, None);
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if matches!(arg.as_str(), "-4" | "-6" | "-C") {
            options.push(arg.clone());
        } else if ["-l", "-p", "-i", "-J", "-o"]
            .iter()
            .any(|p| arg.starts_with(p))
        {
            let option = &arg[..2];
            let value = if arg.len() == 2 {
                index += 1;
                args.get(index)?.as_str()
            } else {
                &arg[2..]
            };
            if value.is_empty() {
                return None;
            }
            match option {
                "-l" => user = Some(value.to_owned()),
                "-o" => {
                    let (key, val) = value
                        .split_once('=')
                        .or_else(|| value.split_once(char::is_whitespace))?;
                    let key = key.to_ascii_lowercase();
                    let val = val.trim();
                    if val.is_empty() {
                        return None;
                    }
                    match key.as_str() {
                        "user" => user = Some(val.to_owned()),
                        "hostname" => host = Some(val.to_owned()),
                        "serveraliveinterval"
                        | "serveralivecountmax"
                        | "connecttimeout"
                        | "port"
                        | "identityfile"
                        | "identitiesonly"
                        | "proxyjump" => options.extend(["-o".into(), format!("{key}={val}")]),
                        _ => return None,
                    }
                }
                "-i" => {
                    let expanded = if let Some(rest) = value.strip_prefix("~/") {
                        PathBuf::from(std::env::var_os("HOME")?)
                            .join(rest)
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        value.to_owned()
                    };
                    options.extend([option.into(), expanded]);
                }
                _ => options.extend([option.into(), value.to_owned()]),
            }
        } else {
            if index + 1 != args.len() {
                return None;
            }
            let destination = if let Some((login, destination)) = arg.rsplit_once('@') {
                user = Some(login.into());
                destination
            } else {
                arg
            };
            let target = host.unwrap_or_else(|| destination.into());
            if !valid_alias(&target) || user.as_deref().is_some_and(|u| !valid_alias(u)) {
                return None;
            }
            return Some(SshAlias {
                host: target,
                user,
                options,
            });
        }
        index += 1;
    }
    None
}

fn bash_ssh_aliases(path: &Path) -> BTreeMap<String, SshAlias> {
    let mut aliases = BTreeMap::new();
    let Ok(contents) = fs::read_to_string(path) else {
        return aliases;
    };
    for line in contents.lines() {
        let Ok(parts) = shell_words::split(line) else {
            continue;
        };
        if parts.first().map(String::as_str) != Some("alias") {
            continue;
        }
        for part in &parts[1..] {
            let Some((name, command)) = part.split_once('=') else {
                continue;
            };
            if !valid_alias(name) {
                continue;
            }
            aliases.remove(name);
            if let Some(target) = parse_ssh_alias(command) {
                aliases.insert(name.into(), target);
            }
        }
    }
    aliases
}

fn keepalive(command: &mut Vec<String>) {
    command.extend(
        [
            "-o",
            "ServerAliveInterval=60",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .map(str::to_owned),
    );
}

pub fn ssh_command(id: &str, profile: Option<&Server>) -> Result<Vec<String>, String> {
    if let Some(profile) = profile {
        profile.validate()?;
        let mut command = vec![
            "/usr/bin/ssh".into(),
            "-l".into(),
            profile.username.clone(),
            "-p".into(),
            profile.port.to_string(),
            "-o".into(),
            format!("HostName={}", profile.host),
        ];
        keepalive(&mut command);
        if profile.password_saved {
            command.extend(
                [
                    "-oBatchMode=no",
                    "-oPreferredAuthentications=publickey,password",
                    "-oNumberOfPasswordPrompts=1",
                ]
                .map(str::to_owned),
            );
        }
        command.push(profile.host.clone());
        return Ok(command);
    }
    if !valid_alias(id) {
        return Err("无效的 SSH 别名".into());
    }
    let path = PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".bashrc");
    ssh_alias_command(id, &path)
}

fn ssh_alias_command(id: &str, bashrc: &Path) -> Result<Vec<String>, String> {
    if !valid_alias(id) {
        return Err("无效的 SSH 别名".into());
    }
    let aliases = bash_ssh_aliases(bashrc);
    let target = aliases
        .get(id)
        .or_else(|| aliases.values().find(|value| value.host == id));
    let mut command = vec!["/usr/bin/ssh".into()];
    if let Some(target) = target {
        if let Some(user) = &target.user {
            command.extend(["-l".into(), user.clone()]);
        }
        command.extend(["-o".into(), format!("HostName={}", target.host)]);
        command.extend(target.options.clone());
    }
    keepalive(&mut command);
    command.push(id.into());
    Ok(command)
}

pub fn legacy_server_profile(id: &str) -> Result<Server, String> {
    let command = ssh_command(id, None)?;
    let mut child = Command::new(&command[0])
        .arg("-G")
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(_) => break,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("ssh -G 超时".into());
            }
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let stdout = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    let mut options = BTreeMap::new();
    for line in stdout.lines() {
        if let Some((key, value)) = line.split_once(' ') {
            options.insert(key, value);
        }
    }
    let server = Server {
        name: id.into(),
        host: options
            .get("hostname")
            .ok_or("ssh -G 缺少 hostname")?
            .to_string(),
        username: options.get("user").ok_or("ssh -G 缺少 user")?.to_string(),
        port: options
            .get("port")
            .ok_or("ssh -G 缺少 port")?
            .parse()
            .map_err(|_| "ssh -G 端口无效")?,
        password_saved: false,
        credential_id: None,
    };
    server.validate()?;
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn config_round_trip_and_rejects_bad_server_without_overwrite() {
        let root = tempdir().unwrap();
        let path = root.path().join("servers.json");
        let server = Server {
            name: "开发机".into(),
            host: "2001:db8::1".into(),
            username: "user".into(),
            port: 22,
            password_saved: true,
            credential_id: None,
        };
        let config = Config {
            aliases: vec!["old".into(), "new".into()],
            servers: BTreeMap::from([("new".into(), server)]),
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), config);
        let before = fs::read(&path).unwrap();
        let mut invalid = config.clone();
        invalid.servers.get_mut("new").unwrap().host = "hostname".into();
        assert!(invalid.save_to(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn aliases_are_static_and_preserve_keepalive_priority() {
        let root = tempdir().unwrap();
        let bashrc = root.path().join("bashrc");
        fs::write(&bashrc, "alias evil='ssh user@host; touch /tmp/marker'\nalias lab='ssh -o ServerAliveInterval=30 user@192.0.2.10'\n").unwrap();
        let command = ssh_alias_command("lab", &bashrc).unwrap();
        assert_eq!(
            &command[..5],
            ["/usr/bin/ssh", "-l", "user", "-o", "HostName=192.0.2.10"]
        );
        assert!(command
            .windows(2)
            .any(|args| args == ["-o", "serveraliveinterval=30"]));
        assert!(!bash_ssh_aliases(&bashrc).contains_key("evil"));
    }

    #[test]
    fn appearance_migrates_legacy_theme_and_palette_restores_xterm_slots() {
        let root = tempdir().unwrap();
        let path = root.path().join("appearance.json");
        fs::write(&path, r##"{"font":"Monospace 11","theme":"dark","foreground":"#e5e7eb","background":"#15181e","cursor":"#93c5fd"}"##).unwrap();
        let appearance = Appearance::load_from(&path).unwrap();
        assert_eq!(appearance.theme, "gnome-gray-on-black");
        assert_eq!(appearance.foreground, "#aaaaaa");
        assert_eq!(terminal_palette("papercolor-light")[81], "#005faf");
        assert_eq!(terminal_palette("gnome-gray-on-black").len(), 256);
    }
}
