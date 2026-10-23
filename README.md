# zterm-r

Rust + GTK4/VTE 终端与 SSH 工作台，从 [zterm](https://github.com/zhou411X/zterm) 独立整理。支持按服务器分组的标签、SFTP 文件传输、30 款配色和实时背景预览。运行时不依赖 Python，SSH 使用系统 OpenSSH，密码保存在 libsecret 系统钥匙环。

[下载安装包](https://github.com/zhou411X/zterm-r/releases/latest) · [v0.3.0 发布说明](RELEASE_NOTES.txt) · [GPL-3.0 许可证](LICENSE)

## 安装与升级

发布的 `.deb` 面向 **Ubuntu 24.04 amd64**。从 Releases 下载安装包及 `SHA256SUMS`，在下载目录执行：

```sh
sha256sum -c SHA256SUMS
sudo apt install ./zterm-r_0.3.0_amd64.deb
zterm-r
```

也可从应用菜单打开 **zterm-r**。升级时安装新的 `.deb`；卸载使用 `sudo apt remove zterm-r`，个人配置和钥匙环记录保留。包只提供独立的 `zterm-r` 程序、桌面入口和图标，可与原 `zterm` 并存。

如果之前执行过下方的用户级 `install.sh`，用户目录中的旧程序或桌面入口可能优先于系统包。切换到 `.deb` 安装时，退出应用后清理这两个旧入口：

```sh
rm -- ~/.local/bin/zterm-r ~/.local/share/applications/local.zterm.r.desktop
```

仅在确实使用过用户级安装时执行；该操作不删除配置、密码或原 `zterm`。

## 构建与运行

Ubuntu 24.04 系统依赖：

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libvte-2.91-gtk4-dev libsecret-1-dev
```

使用 rustup 提供的 Rust 1.85.1（由 `rust-toolchain.toml` 固定），并保留 Cargo.lock：

```sh
git clone https://github.com/zhou411X/zterm-r.git
cd zterm-r
cargo build --locked --release -j32
./target/release/zterm-r
```

也可执行 `sh run.sh`。系统 apt 的 Rust 1.75 不满足当前依赖要求。

用户级并行安装：

```sh
sh install.sh
~/.local/bin/zterm-r
```

该安装提供独立的 “zterm-r” 桌面入口，不覆盖已有 `zterm` / `ssh-workbench` 启动器，也不修改系统显示后端。

## 构建 deb

在上述构建环境中安装打包工具，运行：

```sh
sudo apt install dpkg-dev python3
sh build_deb.sh
```

产物位于 `dist/`，包括 `.deb` 和 `SHA256SUMS`。脚本根据本机二进制分析共享库依赖，并包含许可证和主题来源说明。构建需要 Python，安装后的应用不依赖 Python。请在目标发行版和架构上构建；其他发行版不保证兼容 Ubuntu 24.04 的预编译包。

## 配置与使用

沿用 `${XDG_CONFIG_HOME:-~/.config}/ssh-workbench/servers.json` 和 `appearance.json`，支持旧版本静态 SSH 别名和钥匙环 `org.zterm.Server` / `server` 属性。修改设置会保存到这两个共享配置文件，建议迁移试用期间仅在一个版本中编辑配置。

左侧搜索和选择服务器；左上角侧栏开关右侧的 `+` 为当前服务器新建会话，底栏左侧按钮重新连接当前会话。服务器列表底部依次为添加、编辑、复制和移除按钮。双行服务器条目显示名称和连接地址，右键提供编辑、移除、复制、文件浏览。拖动服务器或标签可调整顺序。切换服务器保留已有连接。

顶部文件夹按钮展开右侧文件面板，支持文件和目录上传、下载、重命名、新建目录、删除文件或空目录。文件任务在后台串行执行，切换服务器不改变已提交任务的目标。冲突时需选择覆盖、另存为或取消。符号链接不跟随，文件内容经暂存文件完成后再发布。

设置按钮打开非模态外观窗口：“字体与配色”页设置字体、终端配色、界面主题和颜色；“背景效果”页设置图片、透明度、图片强度和毛玻璃。两页共用下方的实时终端预览，预览始终展示选中图片的效果，“背景图片同时应用到终端”控制实际终端会话。取消恢复保存值。图片在后台解码与模糊，同一背景在会话间共享缓存。

内置 30 款配色，终端与界面均可独立选择。新增 Monokai、Ayu Light/Dark、AyuMirage、Breadog、ClawBeige Light/Dark、DragonBeige、FaultySchematics、NightOwl Light/Dark、RetroTheme、TokyoNight Light/Dark/Storm 和 VintageLight，颜色值与本机 zterm 的对应主题一致。来源和 ANSI 映射见 [THEME_SOURCES.txt](THEME_SOURCES.txt)。

底栏的额度刷新按钮通过已安装的 Codex CLI 查询额度；会检查 PATH、`~/.local/bin/codex` 和 `~/bin/codex`。后台查询不会确认新主机密钥或解锁钥匙环。

快捷键：`Ctrl+Shift+T` 新会话、`Ctrl+Shift+W` 关闭、`Ctrl+Shift+C/V` 复制粘贴、`Alt+1…9` 切换标签、`Ctrl+PageUp/PageDown` 前后标签、`Ctrl++/-/0` 调整或重置字号。

## 验证

```sh
sudo apt install xvfb xauth openssh-sftp-server
cargo fmt --all --check
GDK_DEBUG=no-portals G_DEBUG=fatal-criticals GDK_BACKEND=x11 GSK_RENDERER=cairo xvfb-run -a cargo test --locked --release -j32 -- --test-threads=1
```

单元测试使用隔离临时目录，不访问个人服务器。GUI 测试使用本地文件选择器，`GDK_DEBUG=no-portals` 仅用于测试，不改变正常启动时的桌面集成。SFTP 协议测试使用本机 `/usr/lib/openssh/sftp-server`，无需运行 sshd 或提供用户密钥。构建前可用 `cargo fmt --check` 检查 Rust 格式。

跨主机 SSH、跳板、桌面钥匙环和原生 Wayland 渲染仍需在对应环境实际验证。Rust 实现本身不代表已经获得性能提升；需要同负载测量才能给出性能结论。
