//! SFTP transfers over the OpenSSH subsystem. All public operations block the
//! caller, so the UI must invoke them on worker threads.

use futures_util::StreamExt;
use openssh_sftp_client::{metadata::MetaData, Sftp, SftpOptions};
use std::fs;
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use uuid::Uuid;

const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub server_id: String,
    pub label: String,
    pub argv: Vec<String>,
    pub environment: Vec<(String, String)>,
}

pub fn endpoint_for(id: &str, profile: Option<&crate::model::Server>) -> Result<Endpoint, String> {
    let command = crate::model::ssh_command(id, profile)?;
    let mut argv = vec![command[0].clone()];
    argv.extend(
        [
            "-T",
            "-oRequestTTY=no",
            "-oRemoteCommand=none",
            "-oPermitLocalCommand=no",
            "-oClearAllForwardings=yes",
            "-oForkAfterAuthentication=no",
            "-oStrictHostKeyChecking=ask",
            "-oConnectTimeout=15",
            "-s",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    argv.extend(command.into_iter().skip(1));
    argv.push("sftp".into());
    let mut environment = vec![
        (
            "SSH_ASKPASS".into(),
            std::env::current_exe()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
        ),
        ("SSH_ASKPASS_REQUIRE".into(), "force".into()),
        ("ZTERM_ASKPASS".into(), "1".into()),
        ("ZTERM_SSH_BACKGROUND".into(), "0".into()),
    ];
    environment.extend(crate::credentials::ssh_environment(id, profile, false));
    Ok(Endpoint {
        server_id: id.into(),
        label: profile.map_or(id, |p| &p.name).into(),
        argv,
        environment,
    })
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub enum Action {
    Upload { source: PathBuf, target: String },
    Download { source: String, target: PathBuf },
    Mkdir { path: String },
    Rename { source: String, target: String },
    Delete { path: String },
}

fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("已取消".into())
    } else {
        Ok(())
    }
}

fn entry_name(name: &str) -> Result<&str, String> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains('/') || name.contains('\0') {
        Err("无效的文件名".into())
    } else {
        Ok(name)
    }
}

fn remote_name(path: &Path) -> Result<String, String> {
    entry_name(path.to_str().ok_or("远程文件名不是 UTF-8")?).map(str::to_owned)
}

fn local_name(path: &Path) -> Result<String, String> {
    remote_name(Path::new(path.file_name().ok_or("无效的本地文件名")?))
}

fn joined(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
}

fn sftp_error(error: openssh_sftp_client::Error) -> String {
    error.to_string()
}

async fn remote_stat(sftp: &Sftp, path: &Path) -> Result<Option<MetaData>, String> {
    match timed(sftp.fs().symlink_metadata(path)).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(openssh_sftp_client::Error::SftpError(code, _))
            if format!("{code:?}") == "NoSuchFile" =>
        {
            Ok(None)
        }
        Err(error) => Err(sftp_error(error)),
    }
}

fn local_stat(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

async fn timed<T>(
    operation: impl Future<Output = Result<T, openssh_sftp_client::Error>>,
) -> Result<T, openssh_sftp_client::Error> {
    tokio::time::timeout(IO_TIMEOUT, operation)
        .await
        .map_err(|_| {
            openssh_sftp_client::Error::IOError(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "SFTP 操作超时",
            ))
        })?
}

fn spawn(endpoint: &Endpoint) -> Result<(Child, tempfile::NamedTempFile), String> {
    let stderr = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let mut command = Command::new(endpoint.argv.first().ok_or("SSH 命令为空")?);
    command
        .args(&endpoint.argv[1..])
        .envs(endpoint.environment.clone())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr.reopen().map_err(|e| e.to_string())?)
        .kill_on_drop(true);
    command.process_group(0);
    Ok((command.spawn().map_err(|e| e.to_string())?, stderr))
}

fn signal_group(pid: Option<u32>, signal: i32) {
    if let Some(pid) = pid {
        // The child starts a new process group, including ProxyJump and askpass.
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }
}

async fn terminate(child: &mut Child) {
    let pid = child.id();
    signal_group(pid, libc::SIGTERM);
    if tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .is_err()
    {
        signal_group(pid, libc::SIGKILL);
        let _ = child.wait().await;
    }
    // The leader may exit before ProxyJump or askpass. Reap surviving members.
    signal_group(pid, libc::SIGKILL);
}

async fn connect(
    endpoint: &Endpoint,
    cancel: &AtomicBool,
) -> Result<(Child, Sftp, tempfile::NamedTempFile), String> {
    let (mut child, stderr) = spawn(endpoint)?;
    let stdin = child.stdin.take().ok_or("无法获取 SSH 输入")?;
    let stdout = child.stdout.take().ok_or("无法获取 SSH 输出")?;
    let result = tokio::select! {
        result = tokio::time::timeout(IO_TIMEOUT, Sftp::new(stdin, stdout, SftpOptions::default())) => result,
        _ = cancellation(cancel) => {
            terminate(&mut child).await;
            return Err("已取消".into());
        }
    };
    match result {
        Ok(Ok(sftp)) => Ok((child, sftp, stderr)),
        result => {
            terminate(&mut child).await;
            let detail = fs::read_to_string(stderr.path()).unwrap_or_default();
            Err(if detail.trim().is_empty() {
                match result {
                    Ok(Err(e)) => e.to_string(),
                    _ => "SFTP 连接超时".into(),
                }
            } else {
                detail.trim().into()
            })
        }
    }
}

async fn shutdown(mut child: Child, sftp: Sftp, abort: bool) {
    if abort {
        drop(sftp);
        terminate(&mut child).await;
        return;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), sftp.close()).await;
    if tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .is_err()
    {
        terminate(&mut child).await;
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

async fn cancellation(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// Fs::rename selects posix-rename when advertised, and that extension replaces
// an existing destination. A standalone SFTP v3 RENAME request preserves the
// standard no-clobber behavior for both files and directories.
async fn standard_rename(
    endpoint: &Endpoint,
    source: &str,
    target: &str,
    cancel: &AtomicBool,
) -> Result<(), String> {
    check(cancel)?;
    if source.as_bytes().contains(&0)
        || target.as_bytes().contains(&0)
        || source.len() > 64 * 1024
        || target.len() > 64 * 1024
    {
        return Err("无效的远程路径".into());
    }
    let (mut child, stderr) = spawn(endpoint)?;
    let mut input = child.stdin.take().ok_or("无法获取 SSH 输入")?;
    let mut output = child.stdout.take().ok_or("无法获取 SSH 输出")?;
    let operation = async {
        input
            .write_all(&[0, 0, 0, 5, 1, 0, 0, 0, 3])
            .await
            .map_err(|e| e.to_string())?;
        input.flush().await.map_err(|e| e.to_string())?;
        let mut header = [0u8; 4];
        output
            .read_exact(&mut header)
            .await
            .map_err(|e| e.to_string())?;
        let packet_length = u32::from_be_bytes(header) as usize;
        if !(5..=256 * 1024).contains(&packet_length) {
            return Err("无效的 SFTP 版本包".into());
        }
        let mut packet = vec![0u8; packet_length];
        output
            .read_exact(&mut packet)
            .await
            .map_err(|e| e.to_string())?;
        if packet[0] != 2 || u32::from_be_bytes(packet[1..5].try_into().unwrap()) != 3 {
            return Err("远端不支持 SFTP v3".into());
        }
        let mut request = Vec::with_capacity(21 + source.len() + target.len());
        request.push(18); // SSH_FXP_RENAME, not posix-rename@openssh.com.
        request.extend_from_slice(&1u32.to_be_bytes());
        for path in [source, target] {
            request.extend_from_slice(&(path.len() as u32).to_be_bytes());
            request.extend_from_slice(path.as_bytes());
        }
        input
            .write_all(&(request.len() as u32).to_be_bytes())
            .await
            .map_err(|e| e.to_string())?;
        input.write_all(&request).await.map_err(|e| e.to_string())?;
        input.flush().await.map_err(|e| e.to_string())?;
        output
            .read_exact(&mut header)
            .await
            .map_err(|e| e.to_string())?;
        let packet_length = u32::from_be_bytes(header) as usize;
        if !(9..=256 * 1024).contains(&packet_length) {
            return Err("无效的 SFTP 状态包".into());
        }
        packet.resize(packet_length, 0);
        output
            .read_exact(&mut packet)
            .await
            .map_err(|e| e.to_string())?;
        if packet[0] != 101 || u32::from_be_bytes(packet[1..5].try_into().unwrap()) != 1 {
            return Err("SFTP 状态响应不匹配".into());
        }
        match u32::from_be_bytes(packet[5..9].try_into().unwrap()) {
            0 => Ok(()),
            4 => Err("CONFLICT:重命名失败，目标可能已存在".into()),
            code => Err(format!("SFTP 重命名失败（状态 {code}）")),
        }
    };
    let result = tokio::select! {
        result = tokio::time::timeout(IO_TIMEOUT, operation) => result.unwrap_or_else(|_| Err("SFTP 重命名超时".into())),
        _ = cancellation(cancel) => Err("已取消".into()),
    };
    drop(input);
    drop(output);
    if result.is_err() {
        terminate(&mut child).await;
        let detail = fs::read_to_string(stderr.path()).unwrap_or_default();
        if !detail.trim().is_empty()
            && !result
                .as_ref()
                .err()
                .is_some_and(|e| e.starts_with("CONFLICT:") || e == "已取消")
        {
            return Err(detail.trim().into());
        }
    } else if tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .is_err()
    {
        terminate(&mut child).await;
    }
    result
}

pub fn browse(
    endpoint: Endpoint,
    path: String,
    cancel: Arc<AtomicBool>,
) -> Result<(String, Vec<Entry>), String> {
    runtime()?.block_on(async {
        check(&cancel)?;
        let (child, sftp, _) = connect(&endpoint, &cancel).await?;
        let result = tokio::select! { result = async {
            let location = timed(sftp.fs().canonicalize(&path)).await.map_err(sftp_error)?;
            let mut rows = Vec::new();
            let stream = timed(sftp.fs().open_dir(&location)).await.map_err(sftp_error)?.read_dir();
            futures_util::pin_mut!(stream);
            while let Some(row) = tokio::time::timeout(IO_TIMEOUT, stream.next()).await.map_err(|_| "目录读取超时")? {
                check(&cancel)?;
                let row = row.map_err(sftp_error)?;
                if matches!(row.filename().to_str(), Some("." | "..")) {
                    continue;
                }
                let kind = row.file_type();
                rows.push(Entry {
                    name: remote_name(row.filename())?,
                    is_dir: kind.is_some_and(|k| k.is_dir()),
                    is_symlink: kind.is_some_and(|k| k.is_symlink()),
                    size: row.metadata().len().unwrap_or(0),
                });
            }
            rows.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok::<_, String>((location.to_string_lossy().into_owned(), rows))
        } => result,
        _ = cancellation(&cancel) => Err("已取消".into()),
        };
        shutdown(child, sftp, result.is_err()).await;
        result
    })
}

#[derive(Clone)]
struct Planned {
    relative: PathBuf,
    is_dir: bool,
    size: u64,
    mode: u32,
}

async fn plan(
    sftp: &Sftp,
    action: &Action,
    overwrite: bool,
    cancel: &AtomicBool,
) -> Result<(Vec<Planned>, u64), String> {
    let upload = matches!(action, Action::Upload { .. });
    let (local_root, remote_root) = match action {
        Action::Upload { source, target } => (source.as_path(), Path::new(target)),
        Action::Download { source, target } => (target.as_path(), Path::new(source)),
        _ => return Err("不是复制操作".into()),
    };
    let mut pending = vec![PathBuf::new()];
    let mut entries = Vec::new();
    let mut total = 0;
    while let Some(relative) = pending.pop() {
        check(cancel)?;
        let (is_dir, size, mode) = if upload {
            let metadata =
                fs::symlink_metadata(joined(local_root, &relative)).map_err(|e| e.to_string())?;
            if !metadata.is_dir() && !metadata.is_file() {
                return Err("仅传输普通文件和目录，不跟随符号链接".into());
            }
            (
                metadata.is_dir(),
                metadata.len(),
                metadata.permissions().mode() & 0o777,
            )
        } else {
            let metadata = timed(sftp.fs().symlink_metadata(joined(remote_root, &relative)))
                .await
                .map_err(sftp_error)?;
            let kind = metadata.file_type().ok_or("远程文件缺少类型信息")?;
            if !kind.is_dir() && !kind.is_file() {
                return Err("仅传输普通文件和目录，不跟随符号链接".into());
            }
            (
                kind.is_dir(),
                metadata.len().unwrap_or(0),
                metadata
                    .permissions()
                    .map_or(0o644, |p| p.as_raw().bits() as u32 & 0o777),
            )
        };
        if is_dir {
            let mut names = Vec::new();
            if upload {
                for item in
                    fs::read_dir(joined(local_root, &relative)).map_err(|e| e.to_string())?
                {
                    names.push(local_name(&item.map_err(|e| e.to_string())?.path())?);
                }
            } else {
                let stream = timed(sftp.fs().open_dir(joined(remote_root, &relative)))
                    .await
                    .map_err(sftp_error)?
                    .read_dir();
                futures_util::pin_mut!(stream);
                while let Some(item) = tokio::time::timeout(IO_TIMEOUT, stream.next())
                    .await
                    .map_err(|_| "目录读取超时")?
                {
                    let item = item.map_err(sftp_error)?;
                    if matches!(item.filename().to_str(), Some("." | "..")) {
                        continue;
                    }
                    names.push(remote_name(item.filename())?);
                }
            }
            names.sort();
            for name in names.into_iter().rev() {
                pending.push(relative.join(name));
            }
        } else {
            total += size;
        }
        entries.push(Planned {
            relative,
            is_dir,
            size,
            mode,
        });
    }
    // Check the complete tree before creating the first directory or temporary file.
    for entry in &entries {
        check(cancel)?;
        let (existing_local, existing_remote) = if upload {
            (
                None,
                remote_stat(sftp, &joined(remote_root, &entry.relative)).await?,
            )
        } else {
            (local_stat(&joined(local_root, &entry.relative))?, None)
        };
        if let Some(metadata) = existing_local {
            if metadata.file_type().is_symlink()
                || metadata.is_dir() != entry.is_dir
                || (!entry.is_dir && !metadata.is_file())
            {
                return Err("目标类型不同或为符号链接".into());
            }
            if !entry.is_dir && !overwrite {
                return Err("CONFLICT:目标已存在".into());
            }
        }
        if let Some(metadata) = existing_remote {
            let kind = metadata.file_type().ok_or("远程文件缺少类型信息")?;
            if kind.is_symlink()
                || kind.is_dir() != entry.is_dir
                || (!entry.is_dir && !kind.is_file())
            {
                return Err("目标类型不同或为符号链接".into());
            }
            if !entry.is_dir && !overwrite {
                return Err("CONFLICT:目标已存在".into());
            }
        }
    }
    Ok((entries, total))
}

async fn copy(
    sftp: &Sftp,
    action: &Action,
    overwrite: bool,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(u64, u64),
    partial: &Arc<Mutex<Option<PathBuf>>>,
) -> Result<(), String> {
    let (entries, total) = plan(sftp, action, overwrite, cancel).await?;
    progress(0, total);
    let mut done = 0;
    for entry in entries {
        check(cancel)?;
        let before = done;
        let (local, remote, upload) = match action {
            Action::Upload { source, target } => (
                joined(source, &entry.relative),
                joined(Path::new(target), &entry.relative),
                true,
            ),
            Action::Download { source, target } => (
                joined(target, &entry.relative),
                joined(Path::new(source), &entry.relative),
                false,
            ),
            _ => unreachable!(),
        };
        if entry.is_dir {
            if upload {
                if remote_stat(sftp, &remote).await?.is_none() {
                    timed(sftp.fs().create_dir(&remote))
                        .await
                        .map_err(sftp_error)?;
                }
            } else if local_stat(&local)?.is_none() {
                fs::create_dir(&local).map_err(|e| e.to_string())?;
            }
            continue;
        }
        if upload {
            let temporary = remote.with_file_name(format!(".zterm-{}.part", Uuid::new_v4()));
            *partial.lock().unwrap() = Some(temporary.clone());
            let result = async {
                let mut source = fs::File::open(&local).map_err(|e| e.to_string())?;
                let mut writer =
                    timed(sftp.options().write(true).create_new(true).open(&temporary))
                        .await
                        .map_err(sftp_error)?;
                let mut buffer = vec![0; 256 * 1024];
                loop {
                    check(cancel)?;
                    let n = source.read(&mut buffer).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    let mut offset = 0;
                    while offset < n {
                        check(cancel)?;
                        offset += timed(writer.write(&buffer[offset..n]))
                            .await
                            .map_err(sftp_error)?;
                    }
                    done += n as u64;
                    progress(done, total);
                }
                timed(writer.close()).await.map_err(sftp_error)?;
                if done - before != entry.size {
                    return Err("传输期间源文件大小发生变化".into());
                }
                timed(
                    sftp.fs()
                        .set_permissions(&temporary, (entry.mode as u16).into()),
                )
                .await
                .map_err(sftp_error)?;
                check(cancel)?;
                if overwrite {
                    if !sftp.support_posix_rename() {
                        return Err("服务器不支持原子覆盖".into());
                    }
                    timed(sftp.fs().rename(&temporary, &remote))
                        .await
                        .map_err(sftp_error)
                } else {
                    // The crate's rename selects posix-rename when available, which
                    // replaces an existing target. Hardlink is the atomic no-clobber
                    // publication primitive exposed by OpenSSH SFTP.
                    if !sftp.support_hardlink() {
                        return Err("服务器不支持无覆盖发布".into());
                    }
                    if let Err(error) = timed(sftp.fs().hard_link(&temporary, &remote)).await {
                        if remote_stat(sftp, &remote).await?.is_some() {
                            return Err("CONFLICT:目标已存在".into());
                        }
                        return Err(sftp_error(error));
                    }
                    timed(sftp.fs().remove_file(&temporary))
                        .await
                        .map_err(sftp_error)
                }
            }
            .await;
            if result.is_err() {
                let _ = timed(sftp.fs().remove_file(&temporary)).await;
            }
            *partial.lock().unwrap() = None;
            result?;
        } else {
            let parent = local.parent().ok_or("目标路径缺少父目录")?;
            let mut temporary = tempfile::Builder::new()
                .prefix(".zterm-")
                .suffix(".part")
                .tempfile_in(parent)
                .map_err(|e| e.to_string())?;
            let mut reader = timed(sftp.open(&remote)).await.map_err(sftp_error)?;
            loop {
                check(cancel)?;
                let chunk = timed(reader.read(256 * 1024, Default::default()))
                    .await
                    .map_err(sftp_error)?;
                let Some(chunk) = chunk else {
                    break;
                };
                temporary.write_all(&chunk).map_err(|e| e.to_string())?;
                done += chunk.len() as u64;
                progress(done, total);
            }
            timed(reader.close()).await.map_err(sftp_error)?;
            if done - before != entry.size {
                return Err("传输期间源文件大小发生变化".into());
            }
            temporary.as_file().sync_all().map_err(|e| e.to_string())?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(entry.mode))
                .map_err(|e| e.to_string())?;
            check(cancel)?;
            if overwrite {
                temporary.persist(&local).map_err(|e| e.to_string())?;
            } else {
                temporary.persist_noclobber(&local).map_err(|e| {
                    if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                        "CONFLICT:目标已存在".into()
                    } else {
                        e.to_string()
                    }
                })?;
            }
        }
    }
    Ok(())
}

pub fn run(
    endpoint: &Endpoint,
    action: &Action,
    overwrite: bool,
    cancel: Arc<AtomicBool>,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    if let Action::Rename { source, target } = action {
        if overwrite {
            return Err("不支持覆盖重命名".into());
        }
        return runtime()?.block_on(standard_rename(endpoint, source, target, &cancel));
    }
    runtime()?.block_on(async {
        check(&cancel)?;
        let (child, sftp, _) = connect(endpoint, &cancel).await?;
        let partial = Arc::new(Mutex::new(None));
        let result = tokio::select! { result = async {
            match action {
                Action::Upload { .. } | Action::Download { .. } => {
                    copy(&sftp, action, overwrite, &cancel, &mut progress, &partial).await
                }
                Action::Mkdir { path } => timed(sftp.fs().create_dir(path)).await.map_err(sftp_error),
                Action::Rename { .. } => unreachable!(),
                Action::Delete { path } => {
                    let metadata = timed(sftp.fs().symlink_metadata(path)).await.map_err(sftp_error)?;
                    if metadata.file_type().is_some_and(|kind| kind.is_dir()) {
                        timed(sftp.fs().remove_dir(path)).await.map_err(sftp_error)
                    } else {
                        timed(sftp.fs().remove_file(path)).await.map_err(sftp_error)
                    }
                }
            }
        } => result,
        _ = cancellation(&cancel) => Err("已取消".into()),
        };
        if let Some(path) = partial.lock().unwrap().take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), sftp.fs().remove_file(path)).await;
        }
        shutdown(child, sftp, result.is_err()).await;
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Endpoint, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let remote = root.path().join("remote files");
        let local = root.path().join("local");
        fs::create_dir(&remote).unwrap();
        fs::create_dir(&local).unwrap();
        let endpoint = Endpoint {
            server_id: "isolated".into(),
            label: "isolated".into(),
            argv: vec![
                "/usr/lib/openssh/sftp-server".into(),
                "-d".into(),
                remote.to_string_lossy().into_owned(),
            ],
            environment: vec![],
        };
        (root, endpoint, remote, local)
    }

    fn path(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn execute(endpoint: &Endpoint, action: Action, overwrite: bool) -> Result<(), String> {
        run(
            endpoint,
            &action,
            overwrite,
            Arc::new(AtomicBool::new(false)),
            |_, _| {},
        )
    }

    #[test]
    fn listing_names_cannot_escape_download_root() {
        for name in ["", ".", "..", "../outside", "/absolute", "a/b", "a\0b"] {
            assert!(entry_name(name).is_err());
        }
        assert_eq!(entry_name("中文 ' ;$ 文件").unwrap(), "中文 ' ;$ 文件");
    }

    #[test]
    fn cancelled_before_connection_does_not_start_process() {
        let endpoint = Endpoint {
            server_id: "test".into(),
            label: "test".into(),
            argv: vec!["/definitely/not/a/program".into()],
            environment: vec![],
        };
        let result = browse(endpoint, ".".into(), Arc::new(AtomicBool::new(true)));
        assert_eq!(result.unwrap_err(), "已取消");
    }

    #[test]
    fn endpoint_uses_verified_openssh_subsystem() {
        let profile = crate::model::Server {
            name: "server".into(),
            host: "192.0.2.1".into(),
            username: "test".into(),
            port: 2222,
            password_saved: false,
            credential_id: None,
        };
        let endpoint = endpoint_for("server-id", Some(&profile)).unwrap();
        assert_eq!(endpoint.label, "server");
        assert!(endpoint
            .argv
            .contains(&"-oStrictHostKeyChecking=ask".into()));
        assert!(endpoint.argv.contains(&"-oClearAllForwardings=yes".into()));
        assert_eq!(endpoint.argv.last().unwrap(), "sftp");
        assert!(endpoint.argv.contains(&"-s".into()));
    }

    #[test]
    fn single_file_paths_round_trip_without_trailing_slash() {
        let (_root, endpoint, remote, local) = fixture();
        let source = local.join("单文件 ;$.txt");
        let target = remote.join("单文件 ;$.txt");
        let downloaded = local.join("downloaded.txt");
        fs::write(&source, b"single file").unwrap();
        execute(
            &endpoint,
            Action::Upload {
                source,
                target: path(&target),
            },
            false,
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"single file");
        execute(
            &endpoint,
            Action::Download {
                source: path(&target),
                target: downloaded.clone(),
            },
            false,
        )
        .unwrap();
        assert_eq!(fs::read(downloaded).unwrap(), b"single file");
    }

    #[test]
    fn browses_binary_protocol_and_transfers_recursive_tree() {
        let (_root, endpoint, remote, local) = fixture();
        fs::write(remote.join("中文 file;$(touch nope)"), b"content").unwrap();
        fs::create_dir(remote.join("empty")).unwrap();
        let (location, rows) = browse(
            endpoint.clone(),
            ".".into(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(location, path(&remote));
        assert_eq!(rows[0].name, "empty");
        assert!(rows.iter().any(|row| row.name == "中文 file;$(touch nope)"));
        let source = local.join("tree 空格");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        fs::write(source.join(".hidden"), []).unwrap();
        let payload = (0..=255).cycle().take(512 * 1024).collect::<Vec<u8>>();
        fs::write(source.join("data ' ;$.bin"), &payload).unwrap();
        let destination = remote.join("tree 空格");
        execute(
            &endpoint,
            Action::Upload {
                source: source.clone(),
                target: path(&destination),
            },
            false,
        )
        .unwrap();
        assert_eq!(
            fs::read(destination.join("data ' ;$.bin")).unwrap(),
            payload
        );
        let downloaded = local.join("download");
        execute(
            &endpoint,
            Action::Download {
                source: path(&destination),
                target: downloaded.clone(),
            },
            false,
        )
        .unwrap();
        assert!(downloaded.join("empty").is_dir());
        assert_eq!(
            fs::read(downloaded.join(".hidden")).unwrap(),
            Vec::<u8>::new()
        );
        assert_eq!(fs::read(downloaded.join("data ' ;$.bin")).unwrap(), payload);
        assert!(fs::read_dir(&destination).unwrap().all(|item| !item
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
    }

    #[test]
    fn conflicts_are_planned_before_copy_and_overwrite_is_explicit() {
        let (_root, endpoint, remote, local) = fixture();
        let source = local.join("tree");
        let target = remote.join("tree");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        fs::write(source.join("a-new"), b"new").unwrap();
        fs::write(source.join("z-existing"), b"replacement").unwrap();
        fs::write(target.join("z-existing"), b"original").unwrap();
        let action = Action::Upload {
            source: source.clone(),
            target: path(&target),
        };
        assert!(execute(&endpoint, action.clone(), false)
            .unwrap_err()
            .starts_with("CONFLICT:"));
        assert!(!target.join("a-new").exists());
        assert_eq!(fs::read(target.join("z-existing")).unwrap(), b"original");
        execute(&endpoint, action, true).unwrap();
        assert_eq!(fs::read(target.join("z-existing")).unwrap(), b"replacement");
        let existing = local.join("existing");
        fs::write(&existing, b"local original").unwrap();
        let download = Action::Download {
            source: path(&target.join("z-existing")),
            target: existing.clone(),
        };
        assert!(execute(&endpoint, download.clone(), false)
            .unwrap_err()
            .starts_with("CONFLICT:"));
        assert_eq!(fs::read(&existing).unwrap(), b"local original");
        execute(&endpoint, download, true).unwrap();
        assert_eq!(fs::read(&existing).unwrap(), b"replacement");
    }

    #[test]
    fn cancellation_does_not_publish_partial_files() {
        let (_root, endpoint, remote, local) = fixture();
        let source = local.join("large");
        fs::write(&source, vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let target = remote.join("large");
        fs::write(&target, b"original").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let result = run(
            &endpoint,
            &Action::Upload {
                source,
                target: path(&target),
            },
            true,
            cancel,
            move |done, _| {
                if done > 0 {
                    signal.store(true, Ordering::Relaxed);
                }
            },
        );
        assert_eq!(result.unwrap_err(), "已取消");
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert!(fs::read_dir(&remote).unwrap().all(|item| !item
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
        fs::write(&target, vec![b'x'; 2 * 1024 * 1024]).unwrap();
        let downloaded = local.join("large-download");
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let result = run(
            &endpoint,
            &Action::Download {
                source: path(&target),
                target: downloaded.clone(),
            },
            false,
            cancel,
            move |done, _| {
                if done > 0 {
                    signal.store(true, Ordering::Relaxed);
                }
            },
        );
        assert_eq!(result.unwrap_err(), "已取消");
        assert!(!downloaded.exists());
        assert!(fs::read_dir(&local).unwrap().all(|item| !item
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".part")));
    }

    #[test]
    fn symlinks_and_nonempty_directory_delete_are_rejected() {
        let (_root, endpoint, remote, local) = fixture();
        fs::write(local.join("real"), b"private").unwrap();
        std::os::unix::fs::symlink(local.join("real"), local.join("link")).unwrap();
        assert!(execute(
            &endpoint,
            Action::Upload {
                source: local.join("link"),
                target: path(&remote.join("copy"))
            },
            false
        )
        .is_err());
        std::os::unix::fs::symlink(local.join("real"), remote.join("link")).unwrap();
        assert!(execute(
            &endpoint,
            Action::Download {
                source: path(&remote.join("link")),
                target: local.join("copy")
            },
            false
        )
        .is_err());
        assert!(!local.join("copy").exists());
        let directory = remote.join("new");
        execute(
            &endpoint,
            Action::Mkdir {
                path: path(&directory),
            },
            false,
        )
        .unwrap();
        fs::write(directory.join("file"), b"keep").unwrap();
        assert!(execute(
            &endpoint,
            Action::Delete {
                path: path(&directory)
            },
            false
        )
        .is_err());
        execute(
            &endpoint,
            Action::Rename {
                source: path(&directory.join("file")),
                target: path(&directory.join("renamed")),
            },
            false,
        )
        .unwrap();
        fs::write(directory.join("existing"), b"old").unwrap();
        assert!(execute(
            &endpoint,
            Action::Rename {
                source: path(&directory.join("renamed")),
                target: path(&directory.join("existing"))
            },
            false
        )
        .unwrap_err()
        .starts_with("CONFLICT:"));
        assert_eq!(fs::read(directory.join("existing")).unwrap(), b"old");
        execute(
            &endpoint,
            Action::Delete {
                path: path(&directory.join("renamed")),
            },
            false,
        )
        .unwrap();
        execute(
            &endpoint,
            Action::Delete {
                path: path(&directory.join("existing")),
            },
            false,
        )
        .unwrap();
        let moved = remote.join("moved");
        execute(
            &endpoint,
            Action::Rename {
                source: path(&directory),
                target: path(&moved),
            },
            false,
        )
        .unwrap();
        assert!(moved.is_dir());
        assert!(!directory.exists());
        execute(&endpoint, Action::Delete { path: path(&moved) }, false).unwrap();
    }
}
