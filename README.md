# DevDock

轻量级 Windows WSLC 镜像与容器管理工具。

A lightweight Windows desktop GUI for managing WSLC images and containers.

## 功能

### 镜像管理

- 查看本地 WSLC 镜像
- 按镜像名称和镜像 ID 筛选
- 在应用内拉取镜像并查看实时命令输出
- 多选并批量删除镜像
- 右键复制镜像名称或镜像 ID
- 显示标签、大小和创建时间

### 容器管理

- 查看运行中和已停止的容器
- 按容器名称和镜像名称筛选
- 通过可视化表单配置并创建容器
- 启动、停止或删除容器
- 在应用内进入运行中容器的交互式 Shell
- 右键复制容器名称或容器 ID
- 显示容器状态、端口映射和完整 ID

### 在线更新

- 启动时在后台检查 GitHub Releases
- 可在左侧菜单手动检查更新
- 显示新版本号和 Release 更新说明
- 自动下载 Windows x64 更新包
- 安装前校验发布包 SHA256
- 更新成功后自动替换程序并重启

## 系统要求

- Windows 10 或 Windows 11
- 支持 WSL 2 的系统环境
- [WSLC](https://github.com/microsoft/WSL) 容器命令行工具

DevDock 启动时会执行 `wslc --version`。如果未检测到 WSLC，程序会显示安装窗口，并通过以下官方工具安装或更新 WSL：

```powershell
winget install --id Microsoft.WSL --exact
wsl --update --pre-release
```

部分系统可能需要管理员权限或重启 Windows。

## 安装

从 [GitHub Releases](https://github.com/mickcui/DevDock/releases) 下载最新的 Windows x64 发布包，解压后运行：

```text
DevDock.exe
```

程序使用的 Logo、中文字体加载逻辑和其他必要资源均已包含在可执行文件中。

`v0.2.0` 新增镜像拉取、容器创建、端口映射展示和容器内嵌交互式 Shell，并改进镜像与容器管理界面。

`v0.1.1` 是首个包含在线更新功能的版本，需要手动下载安装一次。从该版本开始，后续版本可在 DevDock 内完成更新。

## 数据来源

镜像列表来自：

```powershell
wslc images --no-trunc --format json
```

容器列表来自：

```powershell
wslc ls --all --no-trunc --format json
```

镜像删除、容器启动、停止、删除和 Shell 操作同样通过本机 `wslc` 命令执行。容器 Shell 使用：

```powershell
wslc exec --interactive --tty <容器 ID> /bin/sh
```

## 从源码构建

### 准备环境

1. 安装最新稳定版 [Rust](https://www.rust-lang.org/tools/install)。
2. 安装 Visual Studio Build Tools，并启用 C++ 桌面开发工具链。
3. 克隆仓库。

```powershell
git clone https://github.com/mickcui/DevDock.git
cd DevDock
```

### 开发构建

```powershell
cargo run
```

### Release 构建

```powershell
cargo build --release
```

生成的程序位于：

```text
target\release\DevDock.exe
```

Windows Release 构建会从 `assets/logo.svg` 自动生成多尺寸图标并嵌入 EXE。

## 技术栈

- [Rust](https://www.rust-lang.org/)
- [eframe/egui](https://github.com/emilk/egui)
- [egui_extras](https://docs.rs/egui_extras/)
- [portable-pty](https://docs.rs/portable-pty/)
- [vt100](https://docs.rs/vt100/)
- [Serde](https://serde.rs/)

## 注意事项

- DevDock 当前仅支持 Windows。
- 删除容器时会使用 `wslc remove --force`，运行中的容器也会被强制删除。
- 删除镜像或容器前，请确认其中没有需要保留的数据。
- 内嵌 Shell 默认执行 `/bin/sh`，不包含 Shell 的精简容器镜像无法使用该功能。
- DevDock 不包含 WSLC 本身，WSLC 的安装和运行受 Microsoft WSL 要求约束。

## License

本项目基于 [MIT License](LICENSE) 开源。
