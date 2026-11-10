import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  DeviceInfo,
  DeviceQuery,
  DeviceReport,
  ArchiveInfo,
  FirmwareInfo,
  HistoryEntry,
  McpFlashApproval,
  McpStatus,
  McpStorageState,
  OperationEvent,
  OperationRequest,
  PartitionInfo,
  RuntimeInfo,
} from "./types";

export const native = isTauri();
const preview = new URLSearchParams(location.search);
export const demo = !native && preview.get("demo") === "1";
export const exampleFirmware: FirmwareInfo = {
  path: "/demo/OrangePi_5_RK3588S_Android12.img",
  name: "OrangePi_5_RK3588S_Android12.img",
  size: 3875536896,
  kind: "rkfw",
  model: "Orange Pi 5",
  manufacturer: "Orange Pi",
  partitions: [
    { name: "uboot", size: 4194304, startSector: 16384 },
    { name: "boot", size: 67108864, startSector: 32768 },
    { name: "recovery", size: 100663296, startSector: 163840 },
    { name: "super", size: 3699376128, startSector: 360448 },
  ],
  warnings: [],
};
const examplePartitions = exampleFirmware.partitions;
if (demo && preview.get("image") === "raw") {
  Object.assign(exampleFirmware, {
    path: "/demo/rockchip-debian.img",
    name: "rockchip-debian.img",
    kind: "raw",
    model: null,
    manufacturer: null,
    partitions: [],
  });
}
const exampleDevice: DeviceInfo = {
  id: "demo-rk3588",
  name: "Rockchip USB 设备",
  chip: "",
  chipModel: null,
  mode: "maskrom",
  vid: "2207",
  pid: "350b",
  connection: "USB 3.0 · Port 1-2",
  capacity: null,
};
if (demo && preview.get("device") === "loader") {
  Object.assign(exampleDevice, {
    name: "Rockchip USB 设备",
    chip: "RK3588",
    chipModel: "RK3588",
    mode: "loader",
    capacity: 31268536320,
  });
}
export type DemoTaskOptions = {
  label?: string;
  targetName?: string;
  archive?: boolean;
  stages?: string[];
};
const demoHistory: (HistoryEntry & { label?: string })[] = [];
const operationStage = (kind: OperationRequest["kind"]) =>
  ({
    loader: "下载 Loader 到内存",
    backup: "读取设备数据",
    erase: "擦除分区",
    reboot: "重启设备",
    maskrom: "进入 Maskrom",
    flash: "写入固件",
    unpack: "解包固件",
  })[kind];
const handlers = new Set<(event: OperationEvent) => void>();
let demoJob: {
  id: string;
  timer: ReturnType<typeof setTimeout> | null;
  request: OperationRequest;
  started: number;
  options: DemoTaskOptions;
} | null = null;
function finishDemo(status: HistoryEntry["status"]) {
  if (!demoJob) return;
  const job = demoJob;
  if (job.timer) clearTimeout(job.timer);
  if (status === "completed" && job.request.kind === "loader") {
    exampleDevice.name = "Rockchip USB 设备";
    exampleDevice.chip = "RK3588";
    exampleDevice.chipModel = "RK3588";
    exampleDevice.mode = "loader";
    exampleDevice.capacity = 31268536320;
  }
  if (status === "completed" && job.request.kind === "maskrom") {
    exampleDevice.name = "Rockchip USB 设备";
    exampleDevice.chip = "";
    exampleDevice.chipModel = null;
    exampleDevice.mode = "maskrom";
    exampleDevice.capacity = null;
  }
  demoHistory.unshift({
    id: job.id,
    device: job.options.targetName || exampleDevice.name,
    operation: job.request.kind,
    file: job.request.firmwarePath || job.request.outputPath,
    status,
    startedAt: new Date(job.started).toISOString(),
    durationMs: Date.now() - job.started,
    message:
      status === "completed"
        ? "任务完成"
        : status === "failed"
          ? "模拟 USB 连接中断，请检查数据线后重试"
          : "任务已停止",
    label: job.options.label,
  });
  demoJob = null;
}
export const api = {
  async queryDevice(deviceId: string, query: DeviceQuery): Promise<DeviceReport> {
    if (!native) throw new Error("设备查询需要桌面应用");
    return invoke("query_device", { deviceId, query });
  },
  async switchStorage(deviceId: string, storage: string): Promise<DeviceReport> {
    if (!native) throw new Error("存储切换需要桌面应用");
    return invoke("switch_storage", { deviceId, storage });
  },
  async inspectArchive(path: string): Promise<ArchiveInfo> {
    if (!native) throw new Error("压缩包检查需要桌面应用");
    return invoke("inspect_archive", { path });
  },
  async chooseDirectory(): Promise<string | null> {
    if (!native) return demo ? "/demo/unpacked" : null;
    const path = await open({ directory: true, multiple: false });
    return typeof path === "string" ? path : null;
  },
  async startUnpack(path: string, outputDir: string): Promise<string> {
    if (!native) throw new Error("固件解包需要桌面应用");
    return invoke("start_unpack", { request: { path, outputDir } });
  },
  async scan(): Promise<DeviceInfo[]> {
    return native
      ? invoke("scan_devices")
      : demo && preview.get("device") !== "none"
        ? [{ ...exampleDevice }]
        : [];
  },
  async inspect(path: string): Promise<FirmwareInfo> {
    if (native) return invoke("inspect_firmware", { path });
    if (demo)
      return {
        ...exampleFirmware,
        path,
        name: path.split(/[\\/]/).pop() || exampleFirmware.name,
      };
    throw new Error("请在桌面应用中选择本地固件");
  },
  async partitions(deviceId: string): Promise<PartitionInfo[]> {
    if (native) return invoke("read_partitions", { deviceId });
    if (!demo) throw new Error("分区读取需要桌面应用与已连接设备");
    return examplePartitions.map((p) => ({
      name: p.name,
      startSector: p.startSector || 0,
      sectors: p.size / 512,
      size: p.size,
    }));
  },
  async start(request: OperationRequest): Promise<string> {
    if (native) return invoke("start_operation", { request });
    return startDemoWorkflow(request);
  },
  async cancel(jobId: string) {
    if (native) return invoke<void>("cancel_operation", { jobId });
    if (demoJob?.id === jobId) {
      finishDemo("cancelled");
      handlers.forEach((fn) =>
        fn({
          jobId,
          status: "cancelled",
          stage: "已停止",
          progress: null,
          message: "任务已停止",
          level: "warning",
          timestamp: new Date().toISOString(),
          bytesDone: null,
          bytesTotal: null,
        }),
      );
    }
  },
  async history(): Promise<HistoryEntry[]> {
    return native ? invoke("get_history") : [...demoHistory];
  },
  async runtime(): Promise<RuntimeInfo> {
    return native
      ? invoke("get_runtime_info")
      : {
          version: "0.1.0",
          platform: "浏览器",
          backend: demo ? "交互演示" : "界面预览",
        };
  },
  mcpStatus: () => invoke<McpStatus>("mcp_status"),
  setMcpEnabled: (enabled: boolean) =>
    invoke<McpStatus>("set_mcp_enabled", { enabled }),
  setMcpFlashApproval: (flashApproval: McpFlashApproval) =>
    invoke<McpStatus>("set_mcp_flash_approval", { flashApproval }),
  resetMcpToken: () => invoke<McpStatus>("reset_mcp_token"),
  subscribeMcpJob: (fn: (event: OperationEvent) => void) =>
    listen<OperationEvent>("mcp-operation-event", (event) => fn(event.payload)),
  subscribeMcpStorage: (fn: (state: McpStorageState) => void) =>
    listen<McpStorageState>("mcp-storage-state", (event) => fn(event.payload)),
  async subscribe(fn: (event: OperationEvent) => void) {
    if (native)
      return listen<OperationEvent>("operation-event", (e) => fn(e.payload));
    handlers.add(fn);
    return () => {
      handlers.delete(fn);
    };
  },
  async chooseFile(loader = false): Promise<string | null> {
    if (!native) return null;
    const path = await open({
      multiple: false,
      directory: false,
      ...(loader ? { filters: [{ name: "Loader", extensions: ["bin"] }] } : {}),
    });
    return typeof path === "string" ? path : null;
  },
  async chooseSave(defaultPath: string): Promise<string | null> {
    return native
      ? save({ defaultPath })
      : demo
        ? `/demo/${defaultPath}`
        : null;
  },
  async exportLogs(contents: string) {
    if (native) {
      const path = await save({
        defaultPath: `rkdevtool-next-${new Date().toISOString().slice(0, 10)}.log`,
        filters: [{ name: "日志", extensions: ["log"] }],
      });
      if (path) await invoke("export_logs", { path, contents });
    } else {
      const url = URL.createObjectURL(
        new Blob([contents], { type: "text/plain;charset=utf-8" }),
      );
      const a = document.createElement("a");
      a.href = url;
      a.download = "rkdevtool-next.log";
      a.click();
      URL.revokeObjectURL(url);
    }
  },
};

// UI-only task options never cross the native IPC boundary.
export async function startDemoWorkflow(
  request: OperationRequest,
  options: DemoTaskOptions = {},
): Promise<string> {
  if (!demo) throw new Error("USB 烧录只能在桌面应用中进行");
  if (demoJob) throw new Error("已有任务正在执行");
  const id = `demo-${Date.now()}`;
  demoJob = { id, timer: null, request, options, started: Date.now() };
  let n = 0;
  const tick = () => {
    if (!demoJob || demoJob.id !== id) return;
    const short = ["loader", "reboot", "maskrom"].includes(request.kind);
    n += short ? 50 : 4;
    const failed =
      request.kind === "flash" && preview.get("result") === "failed" && n >= 52;
    const done = n >= 100 || failed;
    const stage = failed
      ? "连接中断"
      : n >= 100
        ? "完成"
        : options.stages?.length
          ? options.stages[
              Math.min(
                Math.floor((n / 100) * options.stages.length),
                options.stages.length - 1,
              )
            ]
          : request.kind !== "flash"
            ? operationStage(request.kind)
            : n < (options.archive ? 28 : 8)
              ? options.archive
                ? "解压镜像"
                : "准备写入"
              : "写入固件";
    const bytesTotal =
      request.kind === "loader"
        ? 307200
        : request.kind === "reboot" || request.kind === "maskrom"
          ? null
          : request.sectors
            ? request.sectors * 512
            : exampleFirmware.size;
    const event: OperationEvent = {
      jobId: id,
      status: failed ? "failed" : done ? "completed" : "running",
      stage,
      progress: Math.min(n, 100),
      message: failed
        ? "模拟 USB 连接中断。请检查数据线和供电，重新连接后重试。"
        : done
          ? "任务完成"
          : stage,
      level: failed ? "error" : done ? "success" : "info",
      timestamp: new Date().toISOString(),
      bytesDone:
        bytesTotal === null
          ? null
          : Math.round(bytesTotal * Math.min(n / 100, 1)),
      bytesTotal,
    };
    if (done) finishDemo(failed ? "failed" : "completed");
    handlers.forEach((fn) => fn(event));
    if (!done && demoJob) demoJob.timer = setTimeout(tick, short ? 180 : 240);
  };
  demoJob.timer = setTimeout(tick, 150);
  return id;
}
