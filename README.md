# DevDock

轻量级 Windows WSLC 镜像与容器管理工具。

A lightweight Windows desktop GUI for managing WSLC images and containers.

## 功能

### 镜像管理

- 查看本地 WSLC 镜像
- 按镜像名称和镜像 ID 筛选
- 多选并批量删除镜像
- 右键复制镜像名称或镜像 ID
- 显示标签、大小和创建时间

### 容器管理

- 查看运行中和已停止的容器
- 按容器名称和镜像名称筛选
- 启动、停止或删除容器
- 右键复制容器名称或容器 ID
- 显示容器状态和完整 ID

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

## 数据来源

镜像列表来自：

```powershell
wslc images --no-trunc --format json
```

容器列表来自：

```powershell
wslc ls --all --no-trunc --format json
```

镜像删除、容器启动、停止和删除操作同样通过本机 `wslc` 命令执行。

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
- [Serde](https://serde.rs/)

## 注意事项

- DevDock 当前仅支持 Windows。
- 删除容器时会使用 `wslc remove --force`，运行中的容器也会被强制删除。
- 删除镜像或容器前，请确认其中没有需要保留的数据。
- DevDock 不包含 WSLC 本身，WSLC 的安装和运行受 Microsoft WSL 要求约束。

## License

本项目基于 [MIT License](LICENSE) 开源。
