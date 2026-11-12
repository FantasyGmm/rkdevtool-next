<p align="center">
  <img src="public/icon.png" width="96" alt="RKDevTool Next logo" />
</p>

<h1 align="center">RKDevTool Next</h1>

<p align="center">面向 Rockchip 开发板的原生烧录工作台</p>
<p align="center">
  <a href="https://github.com/FantasyGmm/rkdevtool-next/releases/latest">下载安装包</a> ·
  <a href="https://github.com/FantasyGmm/rkdevtool-next/actions/workflows/desktop-build.yml">构建状态</a> ·
  <a href="https://github.com/FantasyGmm/rkdevtool-next/issues">反馈问题</a>
</p>

RKDevTool Next 是社区维护的跨平台 Rockchip 烧录工具，使用 **Tauri 2 + React / TypeScript + Rust**，将固件选择、Loader 下载、分区管理和运行日志整合到一个桌面窗口中。USB 通信和固件处理在本机完成，运行时不依赖 Python、Qt 或外部 `rkdeveloptool`。

## 界面预览

深色主题 · 演示数据

### 烧录

![RKDevTool Next 烧录页面，展示固件选择、设备详情和运行日志](docs/images/flash.jpg)

### 分区

![RKDevTool Next 分区页面，展示分区列表和分区操作](docs/images/partitions.jpg)

### 高级工具

![RKDevTool Next 高级工具页面，展示设备管理和存储操作](docs/images/advanced.jpg)

## 下载与安装

到 [Releases](https://github.com/FantasyGmm/rkdevtool-next/releases) 下载与你的电脑匹配的安装包：

| 平台 | 文件 | 安装方式 |
| --- | --- | --- |
| macOS Apple Silicon（M 系列，macOS 11+） | `RKDevTool-Next_*_macos-arm64.dmg` | 打开 DMG，将 RKDevTool Next 拖入「应用程序」 |
| Windows x64（Windows 10 / 11） | `RKDevTool-Next_*_windows-x64-setup.exe` | 运行安装程序；按提示安装 WebView2 运行时 |
| Linux x86_64（v0.1.9 之后的新版本） | `RKDevTool-Next_*_linux-x64.deb` / `.AppImage` | Debian 系用 `sudo apt install ./<文件名>.deb`；AppImage 添加执行权限后直接运行 |

macOS 使用无需证书的 ad-hoc 本地签名，未配置 Apple Developer 签名或公证；Windows 安装包未配置代码签名。系统可能显示安全提示，请确认安装包来自本仓库的 Release。

**macOS 首次打开：** 将 App 拖入「应用程序」后，如果系统提示 Apple 无法验证此 App，且你已确认 DMG 来自本仓库的 Release，可以按 [Apple 的说明](https://support.apple.com/zh-cn/102445)在「隐私与安全性」中选择「仍要打开」，也可以在终端运行：

```bash
xattr -dr com.apple.quarantine "/Applications/RKDevTool Next.app"
```

这条命令只移除该 App 的下载隔离标记，不关闭系统的门禁。运行后重新打开 App。

**Windows USB 驱动：** 原生后端需要目标 **Rockusb 接口使用 WinUSB**。Rockchip 专有驱动不等同于 WinUSB。修改驱动前记录原驱动，并只选择开发板对应的 USB 接口，避免改动其他设备。

**Linux USB 权限：** 如果普通用户能启动应用但无法访问开发板，请用发行版的 udev 规则授权目标 Rockusb 设备。安装包不会自动修改 USB 权限，不建议用 root 运行应用。

## 开始烧录

1. 按开发板文档进入 **Maskrom / Loader**，将 USB 数据线接到板卡的 OTG / Device 接口。
2. 在「烧录」页选择整包固件或 RAW 镜像。带内置 Loader 的 RKFW 整包可从 **Maskrom** 直接烧录。
3. RAW、独立 RKAF 和未检查内容的压缩包不能从 Maskrom 直接烧录。先点击「加载 Loader」，选择与板卡匹配的 Loader，等设备以 **Loader** 模式重新连接。
4. 核对设备与写入范围。单个分区镜像可在「分区」页选择目标分区写入；RAW 镜像也可按指定 LBA 写入。选择是否「完成后重启」，确认后开始烧录。

**写入和擦除会覆盖设备数据。** 先备份重要内容，并保持稳定供电。停止任务在数据块边界生效，已写入或擦除的数据不会回滚。一次只连接一台待操作设备。

## 主要功能

- **设备识别**：自动扫描 USB、区分 Maskrom / Loader、Loader 下载后同端口重连与协议握手。
- **芯片与存储信息**：显示原始 Chip ID、已识别的芯片型号、容量、Flash 信息与 Loader 能力。未知 Chip ID 保留原始值，不通过 USB PID 猜型号。
- **固件烧录**：RKFW / RKAF 容器解析、RAW 镜像、写入范围检查及固件 CRC 校验。
- **分区管理**：读取 GPT、分区写入、分区备份与擦除、整机备份、按地址写入。
- **高级工具**：读取设备信息、重启、进入 Maskrom、切换存储、按扇区 / 分区 / 全部存储擦除。
- **本地文件处理**：原生解包 RKFW / RKAF；支持 ZIP、gzip、xz、bzip2 压缩镜像处理。
- **操作反馈**：串行任务、实时进度、日志筛选与导出；向上翻阅日志时暂停自动跟随，回到底部后继续跟随。
- **界面**：浅色 / 深色 / 跟随系统外观，固定导航，独立滚动的分区列表和日志区域。

## 直接使用 CLI

从 Release 下载并解压 `RKDevTool-Next-CLI`，在解压目录运行。无需启动桌面或 MCP：

```bash
./rkdevtool-next-cli scan
./rkdevtool-next-cli inspect update.img
./rkdevtool-next-cli unpack update.img ./output
```

`scan` 将设备列表输出为 JSON。`inspect` 不访问 USB，将 RKFW/RKAF、RAW、MiniLoader 或 ZIP/gzip/xz/bzip2 中可用的镜像信息输出为 JSON。`unpack` 只解包 RKFW/RKAF：在已存在的 `./output` 下新建子目录，不覆盖文件，并输出新目录路径。

烧录前，退出桌面应用和其他烧录工具，从 `scan` 输出取目标设备的 `id`。核对设备与固件并备份后，显式加 `--allow-flash`：

```bash
device_id='从 scan 结果复制的 id'
# RKFW/RKAF 整包：按包内布局写入
./rkdevtool-next-cli flash update.img --device "$device_id" --allow-flash
# RAW：明确指定起始 LBA（512 字节扇区）或 GPT 分区名
./rkdevtool-next-cli flash disk.img --device "$device_id" --start-sector 0 --allow-flash
./rkdevtool-next-cli flash boot.img --device "$device_id" --partition boot --allow-flash
```

RAW 和独立 RKAF 需要设备已进入 Loader；直接烧录压缩包前须先提取镜像。只有加 `--reboot` 才会在成功后重启。按 Ctrl+C 会请求在安全边界停止，已写入的数据不会回滚；请等命令退出再断开设备。

## 连接本机 MCP 客户端

1. 在桌面应用的「偏好设置 → MCP 服务器」勾选启用，点击「保存」。首次运行默认关闭。
2. 点击「复制接入 Skill」，把它粘贴给运行在同一台电脑上的 Agent。Skill 包含 MCP 地址和本机配置路径，Agent 可据此完成注册并用只读调用验证连接。
3. 也可以手动接入：服务使用 Streamable HTTP，地址是 `http://127.0.0.1:38471/mcp`。应用会在自己的数据目录自动生成 `rkdevtool-next.mcp.json`，无需选择保存位置。客户端可以导入其中的 `mcpServers.rkdevtool-next` 条目；例如 Claude Code 可用 `claude --mcp-config "<配置文件路径>"`。

[接入 Skill 源码](skills/rkdevtool-next/SKILL.md)不含密钥。若要撤销旧配置，关闭服务后在「高级设置」重置密钥，再开启服务并重新接入。

在「偏好设置 → MCP 服务器」选择授权模式。读取设备信息等只读操作不弹授权框。

- **逐次批准（默认）**：写操作在桌面逐次确认。
- **仅烧录免批准**：允许直接烧录及该任务请求的烧录后重启；单独加载 Loader、擦除、重启等操作仍需批准。原有的自动烧录设置升级后仍保持这个范围。
- **全权限无人值守（高风险）**：选择该模式，点击「保存」并在弹窗中确认。所有 MCP 已支持的操作都无需逐次批准，包括加载 Loader、擦除、烧录、切换存储、备份、重启、取消任务、解包和导出日志。

> **全权限无人值守有数据丢失风险。** 任何持有本机 MCP 密钥的客户端都能直接擦除或覆盖设备数据，也能将备份和日志写入本机。误操作可能使设备无法启动。只向可信客户端提供配置；先备份，核对目标设备和固件，用完切回「逐次批准」。降低权限或关闭服务不会中止已经启动的任务。

macOS 和 Windows 上，MCP 写操作优先于本地文件选择：如果正在选择 Loader、固件镜像或保存位置，文件窗口会先取消，随后才显示需要的授权框。

关闭偏好设置而未保存会丢弃修改。要重置密钥，先保存关闭 MCP 服务的设置，再勾选「保存时重置 MCP 密钥」并保存。

| 要做什么 | MCP 工具 |
| --- | --- |
| 枚举设备、读 GPT 和 Loader 诊断 | `scan_devices`、`read_partitions`、`query_device` |
| 检查固件或压缩包、解包 RKFW/RKAF | `inspect_firmware`、`inspect_archive`、`start_unpack` |
| 烧录整包、RAW、压缩包成员、分区和批量地址；备份、擦除、加载 Loader、重启或进入 Maskrom | `start_operation` |
| 切换活动存储介质 | `switch_storage`（切换后重新读取 GPT） |
| 查询进度、取消、查看历史和任务日志 | `get_operation`、`cancel_operation`、`get_history`、`get_logs`、`export_logs` |
| 查看程序和后端版本 | `get_runtime_info` |

扇区单位为 512 字节。分区操作传入 GPT 返回的名称、起始扇区和扇区数；整机备份从 LBA 0 开始，扇区数为设备字节容量除以 512。烧录默认不自动重启。

### Codex 桌面还未注册服务器

在运行 RKDevTool Next 的同一台电脑上，用 [MCPorter 0.17.0](https://github.com/openclaw/mcporter/blob/v0.17.0/docs/cli-reference.md)（需 Node.js 24+）直接使用「复制接入 Skill」给出的本机配置路径：

```bash
config="/实际路径/rkdevtool-next.mcp.json"
npx --yes mcporter@0.17.0 --config "$config" call rkdevtool-next.get_runtime_info
npx --yes mcporter@0.17.0 --config "$config" call rkdevtool-next.scan_devices
```

这不会向 Codex 注册服务器，也无需读取或传递授权头。用相同方式调用 `start_operation` 时，可加 `--timeout 120000` 等待桌面批准，再用返回的 `jobId` 调用 `get_operation` 查询进度；超时后不要直接重复提交。

### 无桌面环境：通过 CLI 启动 MCP

从 Release 下载并解压对应平台的 CLI。退出桌面版和其他可能占用 USB 的烧录工具，在运行 MCP 客户端的同一台电脑上执行：

```bash
./rkdevtool-next-cli serve --state-dir ./rkdevtool-headless
```

默认只读。需要无人值守烧录时，停止服务，再在启动命令后加 `--allow-flash`。这只放开烧录（包括请求的烧录后重启）；烧录会覆盖数据，请先备份。全权限无人值守仅在桌面版提供，擦除等其他写操作不会从 CLI 的 MCP 执行。服务只监听 `127.0.0.1:38471`，启动时打印配置文件路径，沿用上面的客户端接入方式；密钥不会打印。使用独立的私有状态目录，不与桌面版共享。按 Ctrl+C 停止接收新请求；正在烧录时，CLI 会等到任务结束再退出。

从源码只构建 CLI，不需要 Node.js、Tauri 或系统 WebView：

```bash
cargo build --locked --release --manifest-path src-tauri/Cargo.toml \
  --no-default-features --features headless-cli --bin rkdevtool-next-cli
```

## 适用范围

- Loader 和固件必须与实际开发板匹配；不支持受保护或签名 Loader 的通用下载流程。
- RKFW 整包写入面向能确定 IDB / GPT 布局的 **eMMC / SD**。加密、NAND 或无法安全规划的布局会被拒绝；不要将 RKFW 容器强行作为 RAW 写入。
- 分区管理以 **GPT** 为基础，不猜测未知分区表或烧录地址。较小 GPT 整盘镜像写入较大设备后的备份 GPT 迁移、按分区操作的实时 GPT 身份复核仍需完善；操作前核对布局，避免在外部工具改表后沿用旧列表。
- 压缩文件支持 ZIP / gzip / xz / bzip2；不支持 7z、zstd、tar。
- USB3 切换、清空序列号、安全模式检测和设备串口日志暂未开放。
- Windows 的构建与打包由 CI 完成，真机 USB 通信仍需在对应 Windows 电脑和板卡上验证。当前 macOS 真机调试使用 RK3576；其他板卡请以实际兼容性为准。

## 独立维护命令（实验性）

维护命令用 Rust 和项目现有 RockUSB 后端实现，不调用外部烧录工具，也不接入 UI 或 MCP。发布的 CLI 用 `rkdevtool-next-cli maintenance --help` 查看这些命令。使用 USB 命令前关闭桌面应用并停止 MCP 服务，只连接一台处于 Loader 模式的设备。可先运行只读的 `ppt`、`rl`；写设备必须显式传入 `--allow-device-write`。

```bash
cargo run --locked --manifest-path src-tauri/Cargo.toml --no-default-features \
  --features maintenance-cli --bin rkdevtool-maintenance -- --help
```

| 命令 | 范围 |
| --- | --- |
| `ppt`、`rl` | 读取并验证 GPT；没有 GPT 特征时才尝试读取带 CRC 的旧 PARM。也可读取指定扇区到新文件。 |
| `wl`、`wlx` | 按地址或经过验证的分区写入 RAW / Android sparse v1。先完整校验 sparse 结构及其 CRC（若提供）；跳过块按零写入。 |
| `gpt`、`prm`、`ul` | 写入 GPT、旧 PARM、或持久化未签名 MiniLoader。GPT 镜像只接受符合当前容量的 67 扇区元数据，不自动迁移；`prm` 不覆盖 GPT；`ul` 要求已有可验证的分区布局。 |
| `unpack`、`pack`、`tagspl` | 本地提取、以未签名 MiniLoader 模板重打包、或为 SPL 加前缀。重打包保留模板芯片、版本与条目顺序，不支持从任意 INI 新建 Loader。输出不覆盖现有文件。 |

设备写入限定 eMMC/SD，逐块读回比较，失败立即停止后续写入。UBI/NAND 的擦块和坏块映射未接入。

## 从源码运行

准备 **Node.js 24 LTS、Rust stable（1.88+）** 及 [Tauri 平台构建依赖](https://v2.tauri.app/start/prerequisites/)：

- macOS：Xcode Command Line Tools。
- Windows：Visual Studio C++ Build Tools、Windows SDK、WebView2。
- Linux：WebKitGTK 4.1 等原生依赖。

```bash
git clone https://github.com/FantasyGmm/rkdevtool-next.git
cd rkdevtool-next
npm ci --ignore-scripts
npm run desktop
```

生成当前平台的安装包：

```bash
npm run check:version
npm run tauri -- build --ci -- --locked
```

产物位于 `src-tauri/target/release/bundle/`。显式指定 `--target` 时，位于对应 target 子目录。macOS 默认生成 `.app` / `.dmg`，Windows 生成 NSIS `.exe`，Linux 生成 `.deb` / `.AppImage`。

仅预览界面可运行 `npm run dev`，打开 `http://localhost:1420/`。加上 `?demo=1` 可进入标注为演示的交互模式，不访问 USB；真实烧录请使用桌面应用。

Linux 源码构建需安装 WebKitGTK 4.1、OpenSSL、AppIndicator、librsvg、patchelf 等依赖。USB 权限应通过发行版的 udev 规则配置，不建议以 root 运行整个桌面应用。

## 自动构建与发布

[GitHub Actions 工作流](.github/workflows/desktop-build.yml) 使用 GitHub-hosted runner：

- 推送 `main`、提交 Pull Request，或手动运行：构建 **macOS arm64 / Windows x64 / Linux x86_64** 桌面安装包和 CLI 压缩包，保存为 Actions artifacts。登录 GitHub 后，可从对应运行的 **Artifacts** 下载 Linux `.deb` 和 `.AppImage` 试用包。
- 推送 `v*` 标签：校验标签与源码版本一致；全部构建成功后，将七个产物一起发布到 GitHub Release。
- 任一平台失败：不发布新的 Release。上传失败时保留草稿，可修复流水线后重跑；已发布的版本不覆盖，后续修改使用新版本号。

发布新版本时，同步修改以下版本号并提交：

```text
package.json
package-lock.json（顶层与根 package）
src-tauri/Cargo.toml
src-tauri/Cargo.lock（rkdevtool-next package）
src-tauri/tauri.conf.json
```

然后推送对应标签，例如：

```bash
npm run check:version
git tag v0.1.9
git push origin main v0.1.9
```

工作流使用仓库内置的 `GITHUB_TOKEN` 发布，无需额外保存 GitHub PAT、Apple 证书或签名密钥。构建不连接开发板，也不执行烧录或擦除。

## 代码结构

```text
src/                    React 界面、Tauri 桥接、共享类型
src-tauri/src/          USB、Loader、固件、GPT 与任务执行
src-tauri/icons/        平台应用图标
public/                 界面品牌资源
assets/                 图标源图与生成说明
scripts/                本机构建与版本一致性检查
.github/workflows/      跨平台构建与 Release 发布
```

## 致谢与许可证

特别感谢 **[gahingwoo / RKDevelopTool-GUI](https://github.com/gahingwoo/RKDevelopTool-GUI)**。RKDevTool Next 的烧录工作流、固件处理与交互组织参考了该项目，并以 Tauri / Rust 实现原生桌面版本。

同时感谢 [rockchip-linux/rkdeveloptool](https://github.com/rockchip-linux/rkdeveloptool)、[rockchip-linux/u-boot](https://github.com/rockchip-linux/u-boot)、[Collabora rockchiprs（rockusb / rockfile）](https://github.com/collabora/rockchiprs)、[nusb](https://github.com/kevinmehall/nusb) 和 [Tauri](https://github.com/tauri-apps/tauri) 等开源项目。各项目在本项目中的用途见 [致谢](ACKNOWLEDGEMENTS.md)。

本项目采用 **GPL-3.0-only**，见 [LICENSE](LICENSE)。第三方依赖保留各自许可证。
