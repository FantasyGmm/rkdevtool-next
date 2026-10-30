# 致谢

## 工作流与协议参考

- **[gahingwoo / RKDevelopTool-GUI](https://github.com/gahingwoo/RKDevelopTool-GUI)**：本项目的主要参考，提供了 Rockchip 固件烧录流程、固件容器处理及桌面交互的实践基础。RKDevTool Next 使用 Tauri、React 与 Rust 重新实现原生工作台。原项目采用 [GPL v3](https://github.com/gahingwoo/RKDevelopTool-GUI/blob/main/LICENSE)。
- **[rockchip-linux / rkdeveloptool](https://github.com/rockchip-linux/rkdeveloptool)**：RockUSB 命令、Loader 下载及固件布局的协议参考。
- **[rockchip-linux / u-boot](https://github.com/rockchip-linux/u-boot)**：ChipInfo 型号映射和 USB-PLUG Loader 行为的参考。

## 核心开源组件

- **[Collabora / rockchiprs](https://github.com/collabora/rockchiprs)**：`rockusb` 提供 RockUSB 协议实现，`rockfile` 提供 Rockchip 文件格式支持。
- **[nusb](https://github.com/kevinmehall/nusb)**：原生 USB 枚举和传输。
- **[Tauri](https://github.com/tauri-apps/tauri)**：桌面应用、系统对话框及跨平台打包。
- **[React](https://github.com/facebook/react)、[Vite](https://github.com/vitejs/vite)、[TypeScript](https://github.com/microsoft/TypeScript)**：界面和前端开发工具。
- **[Lucide](https://github.com/lucide-icons/lucide)**：界面功能图标。
- Rust 生态中的 Tokio、Serde、ZIP、flate2、xz2、bzip2 等组件：异步任务、数据交换与本地压缩文件处理。完整依赖版本记录在 `Cargo.lock` 与 `package-lock.json`。

## 品牌资源

RKDevTool Next 的新标志由 OpenAI 图像生成工具按项目设计需求生成。源图和生成提示分别保留在 `assets/icon-source.png` 与 `assets/icon-prompt.txt`；界面及系统图标由该源图派生。

感谢所有上游项目的维护者与贡献者。
