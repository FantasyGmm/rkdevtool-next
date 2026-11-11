import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  ArrowDownToLine,
  ArrowRight,
  Check,
  ChevronDown,
  Cpu,
  FileArchive,
  FileCode2,
  FileImage,
  FolderOpen,
  Layers3,
  LoaderCircle,
  Monitor,
  Moon,
  PlugZap,
  Plus,
  RefreshCw,
  RotateCcw,
  Settings2,
  Square,
  Sun,
  Terminal,
  Trash2,
  TriangleAlert,
  Upload,
  Usb,
  X,
  Zap,
} from "lucide-react";
import {
  api,
  demo,
  exampleFirmware,
  native,
  startDemoWorkflow,
  type DemoTaskOptions,
} from "./bridge";
import {
  firmwareSamples,
  imageFromArchive,
  isArchive,
  loaderSample,
  type ArchiveSelection,
  type PickedFile,
} from "./demo-files";
import { inspectBrowserFirmware } from "./browser-files";
import { McpSettings } from "./McpSettings";
import { productName as appName, version as appVersion } from "../src-tauri/tauri.conf.json";
import type {
  DeviceInfo,
  DeviceQuery,
  FirmwareInfo,
  McpSettingsDraft,
  McpStatus,
  OperationEvent,
  OperationKind,
  OperationRequest,
  PartitionInfo,
} from "./types";

type Page = "flash" | "partitions" | "advanced";
const PREFERENCES_STORAGE_KEY = "rkdevtool-next-preferences";
type LogLine = { id: number; time: string; level: OperationEvent["level"]; text: string };
type Preferences = {
  reboot: boolean;
  theme: "light" | "dark" | "system";
};
type Pending = {
  request: OperationRequest;
  target: DeviceInfo;
  allowMaskrom?: boolean;
  label: string;
  detail: string;
  file: string | null;
  options?: DemoTaskOptions;
  warnings?: string[];
};
type AddressRow = {
  id: number;
  enabled: boolean;
  name: string;
  address: string;
  file: PickedFile | null;
};
type ToolResult = { title: string; values: [string, string][] };
const operationLabels: Record<OperationKind, string> = {
  flash: "写入固件",
  backup: "备份数据",
  erase: "擦除分区",
  loader: "加载 Loader",
  reboot: "重启设备",
  maskrom: "进入 Maskrom",
  unpack: "解包固件",
};
const operationStageLabels: Record<string, string> = {
  preflight: "检查中",
  extracting: "解包中",
  writing: "写入中",
  rebooting: "重启中",
  "loading-loader": "加载 Loader 中",
  erasing: "擦除中",
  reconnecting: "等待设备重连",
  "verifying-package": "校验固件中",
  "writing-gpt": "写入分区表",
  "writing-idb": "写入引导区",
  reading: "备份中",
  unpack: "解包中",
  "close-protected": "任务进行中",
};
const jobDisplayTitle = (event: OperationEvent) => {
  if (event.status === "completed") return "已完成";
  if (event.status === "failed") return "任务失败";
  if (event.status === "cancelled") return "已停止";
  return operationStageLabels[event.stage] || event.stage;
};
const modeLabel = (mode: DeviceInfo["mode"]) =>
  mode === "maskrom" ? "Maskrom" : mode === "loader" ? "Loader" : "未知模式";
const size = (bytes: number | null) => {
  if (bytes === null) return "—";
  if (bytes === 0) return "0 B";
  const n = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 4);
  return (
    (bytes / 1024 ** n).toFixed(n > 1 ? 2 : 0) +
    " " +
    ["B", "KiB", "MiB", "GiB", "TiB"][n]
  );
};
const shortFile = (path: string | null) => path?.split(/[\\/]/).pop() || "—";
const shortFileLabel = (path: string) =>
  path.split(" › ").map(shortFile).join(" › ");
const deviceDisplayName = (device: DeviceInfo | null) => {
  const model = device?.mode === "loader" ? device.chipModel?.trim() : null;
  return model ? "Rockchip " + model : device?.name;
};
const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
const validSector = (value: string) =>
  value.trim() !== "" &&
  Number.isSafeInteger(Number(value)) &&
  Number(value) >= 0;

function Modal({
  title,
  subtitle,
  children,
  close,
  closeDisabled = false,
  footer,
}: {
  title: string;
  subtitle?: string;
  children: ReactNode;
  close: () => void;
  closeDisabled?: boolean;
  footer?: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const dialog = ref.current;
    dialog?.showModal();
    return () => dialog?.close();
  }, []);
  return (
    <dialog
      ref={ref}
      className="modal"
      aria-labelledby={titleId}
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div className="modal-head">
        <div>
          <h2 id={titleId}>{title}</h2>
          {subtitle && <p>{subtitle}</p>}
        </div>
        <button className="icon-button" disabled={closeDisabled} onClick={close} aria-label="关闭">
          <X size={19} />
        </button>
      </div>
      <div className="modal-body">{children}</div>
      {footer && <div className="modal-footer">{footer}</div>}
    </dialog>
  );
}

function PreferencesDialog({ savedTheme, saveTheme, close }: {
  savedTheme: Preferences["theme"];
  saveTheme: (theme: Preferences["theme"]) => void;
  close: () => void;
}) {
  const [theme, setTheme] = useState(savedTheme);
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [draft, setDraft] = useState<McpSettingsDraft | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [confirmUnattended, setConfirmUnattended] = useState(false);

  useEffect(() => {
    if (!native) return;
    let mounted = true;
    void api.mcpStatus().then((next) => {
      if (!mounted) return;
      setStatus(next);
      setDraft({ enabled: next.enabled, flashApproval: next.flashApproval, resetToken: false });
    }, (error) => {
      if (mounted) setLoadError(errorText(error));
    });
    return () => { mounted = false; };
  }, [attempt]);

  const mcpChanged = !!(status && draft && (
    draft.enabled !== status.enabled || draft.flashApproval !== status.flashApproval ||
    draft.resetToken || (draft.enabled && !status.running)
  ));
  const dirty = theme !== savedTheme || mcpChanged;
  const save = async () => {
    setConfirmUnattended(false);
    setSaveError(null);
    setSaving(true);
    try {
      if (mcpChanged && draft) await api.saveMcpSettings(draft);
      if (theme !== savedTheme) saveTheme(theme);
      close();
    } catch (error) {
      setSaveError(errorText(error));
    } finally {
      setSaving(false);
    }
  };
  const requestSave = () => {
    if (mcpChanged && status && draft?.flashApproval === "unattended" && (
      status.flashApproval !== "unattended" || (draft.enabled && !status.running)
    )) {
      setConfirmUnattended(true);
    } else {
      void save();
    }
  };

  return (
    <>
      <Modal
        title="偏好设置"
        close={() => { if (!saving) close(); }}
        closeDisabled={saving}
        footer={
          <button className="button primary" disabled={!dirty || saving} onClick={requestSave}>
            {saving ? "保存中…" : "保存"} <Check size={15} />
          </button>
        }
      >
        <div className="settings-group">
          <h3>外观</h3>
          <div className="theme-options">
            {([
              ["light", Sun, "浅色"],
              ["dark", Moon, "深色"],
              ["system", Monitor, "跟随系统"],
            ] as const).map(([value, Icon, label]) => (
              <button className={theme === value ? "selected" : ""} key={value}
                disabled={saving} aria-pressed={theme === value} onClick={() => setTheme(value)}>
                <Icon size={19} /> {label}
                {theme === value && <Check size={13} />}
              </button>
            ))}
          </div>
        </div>
        <McpSettings status={status} draft={draft} saving={saving} loadError={loadError}
          change={setDraft} reload={() => { setLoadError(null); setAttempt((value) => value + 1); }} />
        {saveError && <p className="field-error" role="alert">{saveError}</p>}
        <div className="about-box">
          <img src="/icon.png" alt="" />
          <div>
            <strong>{appName} <span>{appVersion}</span></strong>
            <p>Tauri 2 · Native RockUSB</p>
            <small>GPL-3.0</small>
          </div>
        </div>
      </Modal>
      {confirmUnattended && (
        <Modal title="保存全权限无人值守？"
          close={() => setConfirmUnattended(false)}
          footer={
            <>
              <button className="button secondary" autoFocus onClick={() => setConfirmUnattended(false)}>取消</button>
              <button className="button secondary danger-text" onClick={() => void save()}>确认并保存</button>
            </>
          }
        >
          <div className="confirm-warning">
            <TriangleAlert size={17} />
            <p>MCP 服务开启时，持有本机 MCP 密钥的客户端可不经确认地擦除或烧录设备。误操作可能永久丢失数据或使设备无法启动。请先备份，只向可信客户端提供配置。</p>
          </div>
        </Modal>
      )}
    </>
  );
}

export default function App() {
  const [page, setPage] = useState<Page>("flash");
  const pageRef = useRef<Page>(page);
  pageRef.current = page;
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [selectedId, setSelectedId] = useState("");
  const [firmware, setFirmware] = useState<FirmwareInfo | null>(
    demo ? exampleFirmware : null,
  );
  const [archive, setArchive] = useState<ArchiveSelection | null>(null);
  const [archiveOpen, setArchiveOpen] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [inspecting, setInspecting] = useState(false);
  const [partitions, setPartitions] = useState<PartitionInfo[]>([]);
  const [reading, setReading] = useState(false);
  const [logs, setLogs] = useState<LogLine[]>([
    { id: 0, time: new Date().toISOString(), level: "info", text: `${appName} 已就绪` },
  ]);
  const logSequence = useRef(0);
  const lastOperationLog = useRef<OperationEvent | null>(null);
  const [filter, setFilter] = useState<"all" | "error">("all");
  const [job, setJob] = useState<OperationEvent | null>(null);
  const [mcpJob, setMcpJob] = useState<OperationEvent | null>(null);
  const [mcpStopping, setMcpStopping] = useState(false);
  const [mcpStorageBusy, setMcpStorageBusy] = useState(false);
  const mcpJobRef = useRef<OperationEvent | null>(null);
  const mcpStorageBusyRef = useRef(false);
  const [starting, setStarting] = useState(false);
  const [toolBusy, setToolBusy] = useState(false);
  const [activeJobId, setActiveJobId] = useState<string | null>(null);
  const activeJobIdRef = useRef<string | null>(null);
  const activeRequestRef = useRef<OperationRequest | null>(null);
  const [eventsReady, setEventsReady] = useState(false);
  const partitionGeneration = useRef(0);
  const [stopping, setStopping] = useState(false);
  const [shortKind, setShortKind] = useState<OperationKind | null>(null);
  const shortKindRef = useRef<OperationKind | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [address, setAddress] = useState("0x0");
  const [error, setError] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [picker, setPicker] = useState<{
    loader: boolean;
    kind: "raw" | "rkfw" | null;
  } | null>(null);
  const pickerResolve = useRef<((file: PickedFile | null) => void) | null>(
    null,
  );
  const [partitionView, setPartitionView] = useState<"table" | "address">(
    "table",
  );
  const rowSequence = useRef(1);
  const [addressRows, setAddressRows] = useState<AddressRow[]>([
    { id: 1, enabled: true, name: "镜像 1", address: "0x0", file: null },
  ]);
  const [toolResult, setToolResult] = useState<ToolResult | null>(null);
  const [storage, setStorage] = useState(demo ? "eMMC" : "—");
  const [storageChoice, setStorageChoice] = useState("emmc");
  const [exportAddress, setExportAddress] = useState("0x0");
  const [exportSectors, setExportSectors] = useState("0x2000");
  const [erasePartition, setErasePartition] = useState("");
  const [eraseMode, setEraseMode] = useState<"range" | "all" | "partition">("range");
  const [eraseAddress, setEraseAddress] = useState("0x0");
  const [eraseSectors, setEraseSectors] = useState("0x2000");
  const [prefs, setPrefs] = useState<Preferences>(() => {
    try {
      const p = JSON.parse(
        localStorage.getItem(PREFERENCES_STORAGE_KEY) || "{}",
      );
      return {
        reboot: p.reboot !== false,
        theme: ["light", "dark", "system"].includes(p.theme)
          ? p.theme
          : "light",
      };
    } catch {
      return { reboot: true, theme: "light" };
    }
  });
  const busy = toolBusy || starting || shortKind !== null ||
    job?.status === "running" || mcpJob !== null || mcpStorageBusy;
  // This synchronous reservation belongs to the active operation, not the last render.
  const busyRef = useRef(false);
  const deviceWorkBusy = useCallback(
    () => busyRef.current || mcpJobRef.current !== null || mcpStorageBusyRef.current,
    [],
  );
  const scanInFlight = useRef<Promise<void> | null>(null);
  const scanGeneration = useRef(0);
  const manualScanRequested = useRef(false);
  const lastScanError = useRef("");
  const logsBody = useRef<HTMLDivElement>(null);
  const followLogs = useRef(true);
  const logAnchor = useRef<{ id: string; offset: number } | null>(null);
  const multipleDevices = devices.length > 1;
  const device = devices.length === 1 ? devices[0] : null;
  const deviceKey = selectedId + ":" + (device?.mode || "none");
  const currentDeviceKey = useRef(deviceKey);
  currentDeviceKey.current = deviceKey;
  const loaderMode = device?.mode === "loader";
  const canFlashFromMaskrom = device?.mode === "maskrom" && !archive &&
    firmware?.kind === "rkfw" && firmware.hasEmbeddedLoader;
  const chipModel = loaderMode ? device?.chipModel?.trim() : null;
  const deviceTitle = deviceDisplayName(device);
  const isRaw = firmware?.kind === "raw";
  const browserFile = !native && !!firmware?.path.startsWith("browser-file/");
  const longTask = starting || !!job;
  const rawAddressError = isRaw && !validSector(address)
    ? "请输入非负整数扇区地址，例如 0x0 或 32768。"
    : isRaw && firmware && device?.capacity != null &&
        Number(address) * 512 + firmware.size > device.capacity
      ? "镜像写入范围超出当前设备容量。" : null;
  const fileName = archive
    ? archive.name + (archive.selected ? " › " + archive.selected : "")
    : firmware?.name;
  const filePath = archive
    ? archive.path + (archive.selected ? " › " + archive.selected : "")
    : firmware?.path;
  const selectedAddressRows = addressRows.filter((row) => row.enabled);
  const addressReady =
    selectedAddressRows.length > 0 &&
    selectedAddressRows.every(
      (row) => row.file?.info?.kind === "raw" && validSector(row.address),
    );

  const appendLog = useCallback(
    (line: Omit<LogLine, "id">) => {
      const next = { ...line, id: ++logSequence.current };
      setLogs((items) => [...items.slice(-999), next]);
    },
    [],
  );
  const addLog = useCallback(
    (text: string, level: LogLine["level"] = "info") => {
      appendLog({ text, level, time: new Date().toISOString() });
    },
    [appendLog],
  );
  const fail = useCallback(
    (message: string) => {
      setError(message);
      addLog(message, "error");
    },
    [addLog],
  );
  const scan = useCallback(
    async (manual = false) => {
      if (deviceWorkBusy()) return;
      if (manual) {
        manualScanRequested.current = true;
        setScanning(true);
      }
      if (scanInFlight.current) return scanInFlight.current;
      const generation = scanGeneration.current;
      const attempt = (async () => {
        try {
          const found = await api.scan();
          if (generation !== scanGeneration.current || deviceWorkBusy()) return;
          lastScanError.current = "";
          setDevices(found);
          setSelectedId(found.length === 1 ? found[0].id : "");
          if (manualScanRequested.current)
            addLog(
              found.length
                ? "发现 " + found.length + " 台设备"
                : "未发现设备，请检查 USB 连接与设备模式",
            );
        } catch (e) {
          const message = errorText(e);
          // Gate contention is a deferred scan, not a failed USB enumeration.
          if (message.trim() === "设备任务运行中，扫描已暂停") return;
          if (generation !== scanGeneration.current || deviceWorkBusy()) {
            addLog(message, "error");
            return;
          }
          if (manualScanRequested.current || message !== lastScanError.current) fail(message);
          lastScanError.current = message;
        } finally {
          scanInFlight.current = null;
          manualScanRequested.current = false;
          setScanning(false);
        }
      })();
      scanInFlight.current = attempt;
      await attempt;
    },
    [addLog, fail, deviceWorkBusy],
  );
  const beginDeviceWork = useCallback(() => {
    // Reserve synchronously so polling cannot race the pending scan or USB call.
    busyRef.current = true;
    scanGeneration.current += 1;
    return scanInFlight.current;
  }, []);

  useEffect(() => {
    void scan();
    const timer = setInterval(() => void scan(), 2500);
    return () => clearInterval(timer);
  }, [scan]);
  useEffect(() => {
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    api
      .subscribe((event) => {
        if (!activeRequestRef.current ||
          (activeJobIdRef.current && event.jobId !== activeJobIdRef.current)) return;
        activeJobIdRef.current = event.jobId;
        setActiveJobId(event.jobId);
        const isShort = shortKindRef.current !== null;
        if (!isShort) {
          setJob(event);
          setStarting(false);
        }
        const previous = lastOperationLog.current;
        if (!previous || previous.jobId !== event.jobId ||
          previous.status !== event.status || previous.stage !== event.stage ||
          previous.level !== event.level || previous.message !== event.message) {
          appendLog({ time: event.timestamp, level: event.level, text: event.message });
        }
        lastOperationLog.current = event;
        if (event.status !== "running") {
          if (["flash", "erase"].includes(activeRequestRef.current.kind)) {
            partitionGeneration.current += 1;
            setPartitions([]);
            setErasePartition("");
          }
          activeRequestRef.current = null;
          activeJobIdRef.current = null;
          setActiveJobId(null);
          setStopping(false);
          setStarting(false);
          setShortKind(null);
          shortKindRef.current = null;
          busyRef.current = false;
          // Long tasks already own the shared status card; short tasks need an error there.
          if (event.status === "failed") setError(isShort ? event.message : null);
          void scan();
        }
      })
      .then((fn) => {
        if (disposed) fn();
        else { unsubscribe = fn; setEventsReady(true); }
      })
      .catch((e) => fail(errorText(e)));
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [appendLog, fail, scan]);
  useEffect(() => {
    if (!native) return;
    let disposed = false;
    const listeners: Array<() => void> = [];
    const invalidatePartitions = () => {
      partitionGeneration.current += 1;
      setReading(false);
      setPartitions([]);
      setErasePartition("");
      setPending(null);
    };
    const subscriptions = [
      api.subscribeMcpJob((event) => {
        const previous = mcpJobRef.current;
        if (event.status === "running") {
          if (previous?.jobId !== event.jobId) {
            scanGeneration.current += 1;
            invalidatePartitions();
            setMcpStopping(false);
          }
          mcpJobRef.current = event;
          setMcpJob(event);
        } else if (!previous || previous.jobId === event.jobId) {
          mcpJobRef.current = null;
          setMcpJob(null);
          setMcpStopping(false);
          invalidatePartitions();
          void scan();
        }
        if (previous?.message !== event.message || previous?.stage !== event.stage ||
            previous?.status !== event.status) {
          appendLog({ time: event.timestamp, level: event.level, text: "MCP · " + event.message });
        }
        if (event.status === "failed") setError(event.message);
      }),
      api.subscribeMcpStorage((state) => {
        mcpStorageBusyRef.current = state.running;
        setMcpStorageBusy(state.running);
        invalidatePartitions();
        setStorage(state.currentStorage ?? "—");
        setToolResult(null);
        if (state.error) fail(state.error);
        if (!state.running) void scan();
      }),
    ];
    for (const subscription of subscriptions) {
      void subscription.then((unlisten) => {
        if (disposed) unlisten();
        else listeners.push(unlisten);
      }).catch((error) => {
        if (!disposed) fail(errorText(error));
      });
    }
    return () => {
      disposed = true;
      listeners.forEach((unlisten) => unlisten());
    };
  }, [appendLog, fail, scan]);
  useEffect(() => {
    if (job?.status !== "completed") return;
    const timer = setTimeout(
      () =>
        setJob((current) => (current?.jobId === job.jobId ? null : current)),
      2200,
    );
    return () => clearTimeout(timer);
  }, [job]);
  useEffect(() => {
    partitionGeneration.current += 1;
    setPartitions([]);
    setReading(false);
    setErasePartition("");
    setToolResult(null);
    setStorage(demo ? "eMMC" : "—");
    if (!native || !device || device.mode !== "loader" || deviceWorkBusy()) return;
    const key = currentDeviceKey.current;
    const pendingScan = beginDeviceWork();
    setToolBusy(true);
    void (async () => {
      try {
        await pendingScan;
        if (currentDeviceKey.current !== key) return;
        const report = await api.queryDevice(device.id, "currentStorage");
        if (currentDeviceKey.current === key) setStorage(report.currentStorage || report.values.find(([label]) => label === "存储介质")?.[1] || "—");
      } catch (e) { addLog(errorText(e), "warning"); }
      finally { busyRef.current = false; setToolBusy(false); }
    })();
  }, [selectedId, device?.mode, addLog, beginDeviceWork]);
  useEffect(() => {
    localStorage.setItem(PREFERENCES_STORAGE_KEY, JSON.stringify(prefs));
    const media = matchMedia("(prefers-color-scheme: dark)");
    const update = () => {
      document.documentElement.dataset.theme =
        prefs.theme === "system"
          ? media.matches
            ? "dark"
            : "light"
          : prefs.theme;
    };
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [prefs]);
  const rememberLogAnchor = useCallback(() => {
    const body = logsBody.current;
    if (!body) return;
    const top = body.getBoundingClientRect().top;
    for (const child of body.children) {
      if (!(child instanceof HTMLElement) || !child.dataset.logId) continue;
      const rect = child.getBoundingClientRect();
      if (rect.bottom > top + body.clientTop) {
        logAnchor.current = { id: child.dataset.logId, offset: rect.top - top };
        return;
      }
    }
    logAnchor.current = null;
  }, []);
  useLayoutEffect(() => {
    const body = logsBody.current;
    if (!body) return;
    if (followLogs.current) {
      body.scrollTop = body.scrollHeight;
    } else if (logAnchor.current) {
      const anchor = body.querySelector<HTMLElement>(`[data-log-id="${logAnchor.current.id}"]`);
      // Stable IDs retain the reading position even when the 1,000-line cap trims the head.
      body.scrollTop = anchor
        ? body.scrollTop + anchor.getBoundingClientRect().top - body.getBoundingClientRect().top - logAnchor.current.offset
        : 0;
    }
    rememberLogAnchor();
  }, [logs, filter, rememberLogAnchor]);
  const changeLogFilter = (next: typeof filter) => {
    if (next === filter) return;
    followLogs.current = true;
    logAnchor.current = null;
    setFilter(next);
  };

  const closePicker = (file: PickedFile | null = null) => {
    const resolve = pickerResolve.current;
    pickerResolve.current = null;
    setPicker(null);
    resolve?.(file);
  };
  const pickFile = async (
    loader = false,
    kind: "raw" | "rkfw" | null = null,
  ): Promise<PickedFile | null> => {
    if (native) {
      const path = await api.chooseFile(loader);
      return path ? { path, name: shortFile(path), size: 0 } : null;
    }
    return new Promise((resolve) => {
      pickerResolve.current = resolve;
      setPicker({ loader, kind });
    });
  };
  const acceptFirmware = useCallback(
    async (picked: PickedFile) => {
      if (busyRef.current) return;
      setError(null);
      setAddress("0x0");
      if (native && isArchive(picked.path)) {
        setInspecting(true); setFirmware(null); setArchive(null);
        try {
          const info = await api.inspectArchive(picked.path);
          const entries = info.entries.map((entry) => ({ name: entry.name, size: entry.size ?? 0 }));
          const selected = entries.length === 1 ? entries[0].name : null;
          setArchive({ path: info.path, name: picked.name, size: picked.size, kind: info.format === "zip" ? "container" : "stream", entries, selected, sample: false });
          setArchiveOpen(!selected); addLog("已读取压缩包目录 · " + entries.length + " 个镜像；开始后解压并检查格式");
        } catch (e) { fail(errorText(e)); }
        finally { setInspecting(false); }
        return;
      }
      if (picked.archive || isArchive(picked.path)) {
        const selectedArchive: ArchiveSelection = picked.archive
          ? { ...picked.archive }
          : {
              path: picked.path,
              name: picked.name,
              size: picked.size,
              kind: /\.(xz|gz|bz2)$/i.test(picked.path)
                ? "stream"
                : "container",
              entries: [],
              selected: null,
              sample: false,
            };
        setArchive(selectedArchive);
        setArchiveOpen(!selectedArchive.selected);
        setFirmware(
          selectedArchive.selected
            ? imageFromArchive(selectedArchive, selectedArchive.selected)
            : null,
        );
        addLog("已选择 " + picked.name);
        return;
      }
      setInspecting(true);
      setArchive(null);
      try {
        const info = picked.info || (await api.inspect(picked.path));
        setFirmware(info);
        addLog("已选择 " + info.name + " · " + size(info.size));
      } catch (e) {
        setFirmware(null);
        fail(errorText(e));
      } finally {
        setInspecting(false);
      }
    },
    [addLog, fail],
  );
  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let unsubscribe: (() => void) | undefined;
    import("@tauri-apps/api/webview")
      .then(async ({ getCurrentWebview }) => {
        const fn = await getCurrentWebview().onDragDropEvent((event) => {
          if (pageRef.current !== "flash") { setDragging(false); return; }
          if (event.payload.type === "over") setDragging(true);
          if (event.payload.type === "leave") setDragging(false);
          if (event.payload.type === "drop") {
            setDragging(false);
            if (event.payload.paths.length === 1) {
              const path = event.payload.paths[0];
              void acceptFirmware({ path, name: shortFile(path), size: 0 });
            } else fail("一次请选择一个文件");
          }
        });
        if (disposed) fn();
        else unsubscribe = fn;
      })
      .catch((e) => fail(errorText(e)));
    return () => {
      disposed = true;
      unsubscribe?.();
    };
  }, [acceptFirmware, fail]);
  const acceptBrowserFile = async (file: File) => {
    if (busyRef.current) return;
    setInspecting(true);
    try {
      const info = await inspectBrowserFirmware(file);
      await acceptFirmware({ path: info.path, name: info.name, size: info.size, info });
    } catch (e) {
      setFirmware(null);
      setArchive(null);
      fail(errorText(e));
    } finally { setInspecting(false); }
  };
  const chooseFirmware = async () => {
    try {
      const picked = await pickFile();
      if (picked) await acceptFirmware(picked);
    } catch (e) {
      fail(errorText(e));
    }
  };
  const requestFor = (kind: OperationKind): OperationRequest => ({
    deviceId: selectedId,
    kind,
    firmwarePath: null,
    outputPath: null,
    startSector: 0,
    sectors: null,
    reboot: false,
    confirmed: false,
  });
  const submit = async (
    request: OperationRequest,
    options?: DemoTaskOptions,
  ) => {
    if (deviceWorkBusy()) return;
    if (request.deviceId && (devices.length !== 1 || devices[0].id !== request.deviceId)) {
      fail("请仅连接一台目标设备，重新确认后操作。"); return;
    }
    if (!eventsReady) { fail("任务事件尚未就绪，请稍后重试。"); return; }
    setError(null);
    setJob(null);
    const pendingScan = beginDeviceWork();
    lastOperationLog.current = null;
    activeRequestRef.current = request;
    activeJobIdRef.current = null;
    setActiveJobId(null);
    if (["flash", "erase"].includes(request.kind)) {
      partitionGeneration.current += 1;
      setPartitions([]);
      setErasePartition("");
      setReading(false);
    }
    const short = ["loader", "reboot", "maskrom"].includes(request.kind);
    if (short) {
      shortKindRef.current = request.kind;
      setShortKind(request.kind);
    } else {
      setStarting(true);
    }
    try {
      await pendingScan;
      const id = options
        ? await startDemoWorkflow({ ...request, confirmed: true }, options)
        : await api.start({ ...request, confirmed: true });
      if (activeRequestRef.current === request) {
        activeJobIdRef.current = id;
        setActiveJobId(id);
      }
    } catch (e) {
      activeRequestRef.current = null;
      activeJobIdRef.current = null;
      setActiveJobId(null);
      setStarting(false);
      setShortKind(null);
      shortKindRef.current = null;
      busyRef.current = false;
      fail(errorText(e));
    }
  };
  const loadLoader = async () => {
    if (!device || busy) return;
    try {
      const picked = await pickFile(true);
      if (picked)
        await submit({ ...requestFor("loader"), firmwarePath: picked.path });
    } catch (e) {
      fail(errorText(e));
    }
  };
  const prepare = async (
    kind: "flash" | "backup" | "erase",
    partition?: PartitionInfo,
  ) => {
    const loadEmbeddedLoader = kind === "flash" && !partition && canFlashFromMaskrom;
    if (!device || busy || (!loaderMode && !loadEmbeddedLoader)) return;
    const request = requestFor(kind);
    if (partition) request.partitionName = partition.name;
    let file: string | null = null;
    let detail = "";
    let options: DemoTaskOptions | undefined;
    try {
      if (kind === "flash") {
        if (partition) {
          const picked = await pickFile(false, "raw");
          if (!picked) return;
          if (isArchive(picked.path)) {
            fail("请先解压文件，再选择要写入的镜像。");
            return;
          }
          const info = picked.info || (await api.inspect(picked.path));
          if (info.kind !== "raw") {
            fail("分区写入请选择原始镜像；固件包请在烧录页写入。");
            return;
          }
          if (info.size > partition.size) {
            fail("镜像大于目标分区，不能写入。");
            return;
          }
          file = picked.path;
          request.firmwarePath = file;
          request.startSector = partition.startSector;
          request.sectors = partition.sectors;
          detail = partition.name + " · LBA " + partition.startSector +
            "–" + (partition.startSector + Math.ceil(info.size / 512) - 1) +
            " · 写入 " + size(info.size);
        } else {
          if (native && archive) {
            if (!archive.selected) return;
            request.firmwarePath = archive.path;
            request.archiveMember = archive.selected;
            request.reboot = prefs.reboot;
            setPending({ request, target: { ...device }, label: operationLabels.flash,
              detail: "解压后检查实际格式：固件包按其分区布局写入，RAW 镜像从 LBA 0 写入。",
              file: archive.path + " › " + archive.selected,
              warnings: ["解压、格式及容量检查在确认后执行；检查通过前不会写入设备。"] });
            return;
          }
          if (!firmware || firmware.kind === "loader") return;
          if (rawAddressError) {
            fail(rawAddressError);
            return;
          }
          file = filePath || firmware.path;
          request.firmwarePath = file;
          request.startSector = isRaw ? Number(address) : 0;
          request.reboot = prefs.reboot;
          detail = isRaw
            ? "起始扇区 " + address + " · " + size(firmware.size)
            : "按固件布局写入：" + (firmware.partitions.map((p) => p.name).join("、") || "以解析后的写入计划为准");
          if (demo && loadEmbeddedLoader) options = { embeddedLoader: true };
          if (archive) options = { archive: true };
        }
      } else if (kind === "backup") {
        if (!partition && !device.capacity) {
          fail("未读取到容量，请先读取分区。");
          return;
        }
        file = await api.chooseSave(
          (partition?.name || device.chip || "device") + "-backup.img",
        );
        if (!file) return;
        request.outputPath = file;
        request.startSector = partition?.startSector || 0;
        request.sectors =
          partition?.sectors || Math.floor(device.capacity! / 512);
        await submit(request);
        return;
      } else {
        if (!partition) return;
        request.startSector = partition.startSector;
        request.sectors = partition.sectors;
        detail = partition.name + " · " + size(partition.size);
      }
      setPending({
        request,
        target: { ...device },
        allowMaskrom: loadEmbeddedLoader,
        label: operationLabels[kind],
        detail,
        file,
        options,
        warnings: kind === "flash" && !partition ? firmware?.warnings : [],
      });
    } catch (e) {
      fail(errorText(e));
    }
  };
  const readPartitions = async () => {
    if (!device || deviceWorkBusy() || !loaderMode) return;
    const key = currentDeviceKey.current;
    const generation = ++partitionGeneration.current;
    const pendingScan = beginDeviceWork();
    setToolBusy(true);
    setReading(true);
    try {
      await pendingScan;
      const items = await api.partitions(device.id);
      if (currentDeviceKey.current !== key || generation !== partitionGeneration.current) return;
      setPartitions(items);
      addLog("已读取 " + items.length + " 个 GPT 分区");
    } catch (e) {
      if (currentDeviceKey.current === key && generation === partitionGeneration.current) fail(errorText(e));
    } finally {
      if (currentDeviceKey.current === key && generation === partitionGeneration.current) setReading(false);
      busyRef.current = false;
      setToolBusy(false);
    }
  };
  const cancel = async () => {
    if (!activeJobId || stopping) return;
    setStopping(true);
    try {
      await api.cancel(activeJobId);
      addLog("已请求停止", "warning");
    } catch (e) {
      setStopping(false);
      fail(errorText(e));
    }
  };
  const changeRow = (id: number, value: Partial<AddressRow>) =>
    setAddressRows((rows) =>
      rows.map((row) => (row.id === id ? { ...row, ...value } : row)),
    );
  const chooseRowFile = async (row: AddressRow) => {
    try {
      const picked = await pickFile(false, "raw");
      if (!picked) return;
      if (isArchive(picked.path)) {
        fail("请为地址条目选择解压后的镜像。");
        return;
      }
      const info = picked.info || (await api.inspect(picked.path));
      if (info.kind !== "raw") {
        fail("按地址写入请选择原始镜像；固件包请在烧录页写入。");
        return;
      }
      changeRow(row.id, { file: { ...picked, info } });
    } catch (e) {
      fail(errorText(e));
    }
  };
  const prepareAddressWrite = () => {
    if ((!native && !demo) || !device || !loaderMode || busy || !addressReady) return;
    const rows = selectedAddressRows;
    const request = {
      ...requestFor("flash"),
      firmwarePath: null,
      writes: rows.map((row) => ({ name: row.name, path: row.file!.path, startSector: Number(row.address) })),
      reboot: prefs.reboot,
    };
    const stages = rows.map((row) => "写入 " + (row.name || row.file!.name));
    setPending({
      request,
      target: { ...device },
      label: "按地址写入",
      detail: rows
        .map((row) => (row.name || row.file!.name) + " @ " + row.address)
        .join("；"),
      file: null,
      options: demo ? { label: "按地址写入", stages } : undefined,
    });
  };
  const showInformation = async (title: string) => {
    if ((!native && !demo) || !device || deviceWorkBusy()) return;
    if (native) {
      const queries: Record<string, DeviceQuery> = { "Flash ID": "flashId", "Flash 信息": "flashInfo", "Chip 信息": "chipInfo", Capability: "capability", "测试设备": "testDevice" };
      const key = currentDeviceKey.current;
      const pendingScan = beginDeviceWork();
      setToolBusy(true);
      try {
        await pendingScan;
        const report = await api.queryDevice(device.id, queries[title]);
        if (currentDeviceKey.current === key) { setToolResult(report); if (report.currentStorage) setStorage(report.currentStorage); }
        addLog(title + " · 读取完成");
      } catch (e) { fail(errorText(e)); }
      finally { busyRef.current = false; setToolBusy(false); }
      return;
    }
    const values: Record<string, [string, string][]> = {
      "Flash ID": [
        ["ID", "45 4D 4D 43 20"],
        ["存储", storage],
      ],
      "Flash 信息": [
        ["容量", size(device.capacity)],
        ["扇区大小", "512 B"],
        ["存储", storage],
      ],
      "Chip 信息": [
        ["Chip ID", device.chip || "—"],
        ["USB ID", device.vid + ":" + device.pid],
      ],
      Capability: [
        ["LBA 读写", "支持"],
        ["存储切换", "支持"],
        ["直接 LBA", "支持"],
      ],
      测试设备: [
        ["连接", "正常"],
        ["响应时间", "2 ms"],
      ],
    };
    setToolResult({ title, values: values[title] });
    addLog(title + " · 读取完成");
  };
  const switchStorage = async () => {
    if (!device || !loaderMode || deviceWorkBusy()) return;
    const labels: Record<string, string> = { emmc: "eMMC", sd0: "SD", "spi-nor": "SPI NOR", "spi-nand": "SPI NAND", ram: "RAM" };
    if (demo) { setStorage(labels[storageChoice]); setToolResult({ title: "当前存储", values: [["介质", labels[storageChoice]]] }); return; }
    if (!native) return;
    const pendingScan = beginDeviceWork();
    setToolBusy(true); setStorage("—");
    try { await pendingScan; const report = await api.switchStorage(device.id, storageChoice); setToolResult(report); setStorage(report.currentStorage || report.values.find(([key]) => key === "当前存储")?.[1] || "—"); addLog("存储切换完成，已重新读取设备状态"); }
    catch (e) { fail(errorText(e)); }
    finally {
      partitionGeneration.current += 1; setPartitions([]); setErasePartition("");
      busyRef.current = false; setToolBusy(false); void scan();
    }
  };
  const unpackFirmware = async () => {
    if ((!native && !demo) || busy) return;
    try {
      const picked = await pickFile(false, "rkfw");
      if (!picked) return;
      const output = await api.chooseDirectory();
      if (!output) return;
      const request = { ...requestFor("unpack"), deviceId: "", firmwarePath: picked.path, outputPath: output };
      if (demo) { await submit(request, { label: "解包固件", targetName: "本机", stages: ["读取固件结构", "解包镜像", "整理输出文件"] }); return; }
      if (!eventsReady || deviceWorkBusy()) return;
      activeRequestRef.current = request; activeJobIdRef.current = null; setActiveJobId(null);
      const pendingScan = beginDeviceWork();
      lastOperationLog.current = null;
      setStarting(true); setJob(null); setError(null);
      try {
        await pendingScan;
        const id = await api.startUnpack(picked.path, output);
        if (activeRequestRef.current === request) { activeJobIdRef.current = id; setActiveJobId(id); }
      } catch (e) {
        activeRequestRef.current = null; activeJobIdRef.current = null; setActiveJobId(null);
        busyRef.current = false; setStarting(false); throw e;
      }
    } catch (e) { fail(errorText(e)); }
  };
  const exportImage = async () => {
    if ((!native && !demo) || !device || !loaderMode || busy) return;
    if (!validSector(exportAddress) || !validSector(exportSectors) || Number(exportSectors) === 0 ||
      !Number.isSafeInteger(Number(exportAddress) + Number(exportSectors)) ||
      (device.capacity != null && (Number(exportAddress) + Number(exportSectors)) * 512 > device.capacity)) {
      fail("请输入容量范围内的起始扇区和大于 0 的扇区数。"); return;
    }
    try {
      const outputPath = await api.chooseSave("export.img");
      if (!outputPath) return;
      await submit({ ...requestFor("backup"), startSector: Number(exportAddress), sectors: Number(exportSectors), outputPath }, demo ? { label: "导出镜像" } : undefined);
    } catch (e) { fail(errorText(e)); }
  };
  const eraseRangeError = !validSector(eraseAddress) || !validSector(eraseSectors) || Number(eraseSectors) === 0
    ? "请输入有效起始扇区和大于 0 的扇区数。"
    : !Number.isSafeInteger(Number(eraseAddress) + Number(eraseSectors))
      ? "擦除范围过大。"
      : device?.capacity != null && (Number(eraseAddress) + Number(eraseSectors)) * 512 > device.capacity
        ? "擦除范围超出当前设备容量。" : null;
  const eraseAllReady = !!device?.capacity && Number.isSafeInteger(device.capacity / 512);
  const prepareErase = () => {
    if (!device || !loaderMode || busy) return;
    if (eraseMode === "partition") {
      if (currentErasePartition) void prepare("erase", currentErasePartition);
      return;
    }
    if (eraseMode === "range" && eraseRangeError) { fail(eraseRangeError); return; }
    if (eraseMode === "all" && !eraseAllReady) { fail("无法确认存储容量，不能擦除全部。"); return; }
    const start = eraseMode === "all" ? 0 : Number(eraseAddress);
    const count = eraseMode === "all" ? device.capacity! / 512 : Number(eraseSectors);
    setPending({
      request: { ...requestFor("erase"), startSector: start, sectors: count },
      target: { ...device },
      label: eraseMode === "all" ? "擦除全部" : "按地址擦除",
      detail: "LBA " + start + "–" + (start + count - 1) + " · " + size(count * 512),
      file: null,
      warnings: eraseMode === "all" ? ["将擦除分区表、引导与全部数据；此操作不代表安全抹除保证。"] : [],
    });
  };
  const ready =
    !!device &&
    (loaderMode || canFlashFromMaskrom) &&
    ((!!firmware && firmware.kind !== "loader") || (native && !!archive?.selected)) &&
    (!archive || archive.sample || native) &&
    !inspecting &&
    !reading &&
    !rawAddressError &&
    !browserFile &&
    eventsReady;
  const visibleLogs =
    filter === "error"
      ? logs.filter((l) => l.level === "error" || l.level === "warning")
      : logs;
  const unsupportedTitle = "连接 Loader 模式设备后使用";
  const currentErasePartition =
    partitions.find((p) => p.name === erasePartition) || null;
  const deviceSummary = device
    ? `${deviceTitle} · ${modeLabel(device.mode)}`
    : multipleDevices ? "请仅保留一台设备" : "未连接设备";
  const connectionWarning = multipleDevices
    ? `检测到 ${devices.length} 台设备。为避免误操作，请断开其他设备，仅保留要操作的一台。`
    : null;
  const statusMessage = error || (job?.status === "failed" ? job.message : null) || connectionWarning;
  const statusFailed = !!error || job?.status === "failed";
  const taskRunning = starting || job?.status === "running";
  const showTaskStatus = longTask && (!error || taskRunning);
  const showStatus = showTaskStatus || !!statusMessage;
  const compactProgress = taskRunning && !statusMessage;
  const statusKind = showTaskStatus ? job?.status || "running" : statusFailed ? "failed" : "warning";
  const statusTitle = showTaskStatus
    ? starting ? "准备任务" : job && jobDisplayTitle(job)
    : error ? "操作失败" : "连接提示";

  return (
    <div className="app-shell">
      <header className="app-toolbar">
        <div className="app-brand">
          <img src="/icon.png" alt="" />
          <strong>{appName}</strong>
        </div>
        <nav className="page-tabs" role="tablist" aria-label="工作区">
          {(
            [
              ["flash", "烧录"],
              ["partitions", "分区"],
              ["advanced", "高级"],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              id={"tab-" + key}
              role="tab"
              aria-selected={page === key}
              aria-controls="workspace"
              className={page === key ? "selected" : ""}
              onClick={() => setPage(key)}
            >
              {label}
            </button>
          ))}
        </nav>
        <div className="toolbar-actions">
          {!native && <span className="preview-label" title={demo ? "模拟设备与任务，不访问真实 USB" : "浏览器预览，USB 操作仅限桌面应用"}>{demo ? "演示 · 不写入设备" : "界面预览 · 无 USB"}</span>}

          <button
            className="icon-button"
            onClick={() => setSettingsOpen(true)}
            aria-label="偏好设置"
            title="偏好设置"
          >
            <Settings2 size={18} />
          </button>
        </div>
      </header>
      <main
        className="workspace"
        id="workspace"
        role="tabpanel"
        aria-labelledby={"tab-" + page}
      >
        <div className="workspace-content">
        {(mcpJob || mcpStorageBusy) && (
          <section className="panel mcp-activity" aria-label="MCP 任务">
            <div>
              <strong>MCP 任务进行中</strong>
              <p>{mcpJob?.message || "正在切换存储介质"}</p>
            </div>
            {mcpJob?.progress != null && <span>{Math.round(mcpJob.progress)}%</span>}
            {mcpJob && (
              <button className="button secondary" disabled={mcpStopping} onClick={() => {
                setMcpStopping(true);
                void api.cancel(mcpJob.jobId).catch((error) => {
                  setMcpStopping(false);
                  fail(errorText(error));
                });
              }}>
                <Square size={13} /> {mcpStopping ? "正在停止…" : "停止"}
              </button>
            )}
          </section>
        )}
        {page === "flash" && (
          <div className="flash-workspace">
            <div className="flash-panels">
            <section className="panel device-panel" aria-label="目标设备">
              <div className="panel-heading">
                <h2>目标设备</h2>
                <button
                  className="text-button"
                  onClick={() => void scan(true)}
                  disabled={busy || scanning}
                >
                  <RefreshCw size={14} className={scanning ? "spin" : ""} />
                  {scanning ? "扫描中" : "刷新"}
                </button>
              </div>
              <div className="device-main">
                <div className="field-icon">
                  <Usb size={22} />
                </div>
                <div className="device-selection">
                  <strong className="single-device-name" title={deviceTitle}>
                    {device ? deviceTitle : multipleDevices ? "请断开多余设备" : "未连接设备"}
                  </strong>
                </div>
                {device && (
                  <>
                    <span
                      className={
                        "mode-badge" +
                        (device.mode === "maskrom" ? " amber" : "")
                      }
                    >
                      {modeLabel(device.mode)}
                    </span>
                    {device.mode === "maskrom" && (
                      <button
                        className="button secondary"
                        disabled={busy || reading}
                        onClick={() => void loadLoader()}
                      >
                        {shortKind === "loader" ? (
                          <LoaderCircle size={15} className="spin" />
                        ) : (
                          <PlugZap size={15} />
                        )}
                        {shortKind === "loader" ? "加载中…" : "加载 Loader"}
                      </button>
                    )}
                  </>
                )}
              </div>
              <div className="device-details-section">
                <dl className="device-details" aria-label="设备详情">
                  <div>
                    <dt>Chip ID</dt>
                    <dd title={chipModel || device?.chip || undefined}>{chipModel || device?.chip || "—"}</dd>
                  </div>
                  <div>
                    <dt>USB 标识</dt>
                    <dd title={device ? `${device.vid}:${device.pid}` : undefined}>{device ? `${device.vid}:${device.pid}` : "—"}</dd>
                  </div>
                  <div>
                    <dt>存储容量</dt>
                    <dd>{size(device?.capacity ?? null)}</dd>
                  </div>
                  <div className="device-connection-detail">
                    <dt>连接位置</dt>
                    <dd title={device?.connection}>{device?.connection || "—"}</dd>
                  </div>
                </dl>
              </div>
            </section>
            <section className="panel firmware-panel" aria-label="固件文件">
              <div className="panel-heading">
                <h2>固件文件</h2>
                <label className="raw-address" title={!isRaw ? "仅用于 RAW 镜像；固件包按自身布局写入" : undefined}>
                  <span>起始扇区（LBA）</span>
                  <input
                    aria-label="起始扇区"
                    className="address-input"
                    value={address}
                    aria-invalid={!inspecting && !!rawAddressError}
                    aria-describedby={!inspecting && rawAddressError ? "raw-address-error" : undefined}
                    onChange={(e) => setAddress(e.target.value)}
                    disabled={busy || inspecting || !isRaw}
                  />
                </label>
              </div>
              <div
                className={
                  "file-drop" +
                  (fileName ? " has-file" : "") +
                  (dragging ? " dragging" : "")
                }
                onDragOver={(e) => {
                  e.preventDefault();
                  if (!busy) setDragging(true);
                }}
                onDragLeave={() => setDragging(false)}
                onDrop={(e) => {
                  e.preventDefault();
                  setDragging(false);
                  if (!native && !busy) {
                    const files = e.dataTransfer.files;
                    if (files.length !== 1) fail("一次请拖入一个固件文件。");
                    else void acceptBrowserFile(files[0]);
                  }
                }}
              >
                <div className="field-icon">
                  {inspecting ? (
                    <LoaderCircle size={22} className="spin" />
                  ) : archive ? (
                    <FileArchive size={22} />
                  ) : (
                    <FileImage size={22} />
                  )}
                </div>
                <div className="file-description">
                  <h3 title={fileName}>
                    {inspecting
                      ? "正在读取文件…"
                      : fileName || "选择或拖入固件"}
                  </h3>
                  {fileName && (
                    <p>
                      {archive
                        ? (archive.size ? size(archive.size) + " · " : "") +
                          (archive.kind === "stream" ? "压缩镜像" : "压缩包")
                        : firmware
                          ? size(firmware.size) +
                            " · " +
                            (firmware.kind === "rkfw"
                              ? "Rockchip 固件"
                              : firmware.kind === "loader"
                                ? "Loader"
                                : "磁盘镜像")
                          : ""}
                    </p>
                  )}
                </div>
                <button
                  className="button secondary"
                  disabled={busy || inspecting}
                  onClick={() => void chooseFirmware()}
                >
                  <FolderOpen size={15} />
                  {fileName ? "更换…" : "选择文件…"}
                </button>
                {fileName && (
                  <button
                    className="icon-button"
                    aria-label="移除固件"
                    title="移除固件"
                    disabled={busy || inspecting}
                    onClick={() => {
                      setFirmware(null);
                      setArchive(null);
                    }}
                  >
                    <X size={16} />
                  </button>
                )}
              </div>
              {browserFile && <div className="inline-unavailable">已读取本地文件头 · 浏览器仅预览识别结果，实际烧录请使用桌面应用。</div>}
              {archive &&
                (archive.sample || native ? (
                  <details
                    className="archive-contents"
                    open={archiveOpen}
                    onToggle={(e) => setArchiveOpen(e.currentTarget.open)}
                  >
                    <summary className="archive-heading">
                      <span>
                        {archive.kind === "stream" ? "目标镜像" : "包内文件"}
                      </span>
                      {!archive.selected && <span>选择一个镜像</span>}
                      <ChevronDown size={14} />
                    </summary>
                    {archive.entries.map((entry) => {
                      const selectable = /\.img$/i.test(entry.name);
                      return (
                        <label
                          key={entry.name}
                          className={
                            "archive-entry" +
                            (!selectable ? " unavailable" : "")
                          }
                        >
                          <input
                            type="radio"
                            name="archive-image"
                            checked={archive.selected === entry.name}
                            disabled={!selectable || busy}
                            onChange={() => {
                              const updated = {
                                ...archive,
                                selected: entry.name,
                              };
                              setArchive(updated);
                              setFirmware(native ? null : imageFromArchive(updated, entry.name));
                              setArchiveOpen(false);
                            }}
                          />
                          {selectable ? (
                            <FileImage size={15} />
                          ) : (
                            <FileCode2 size={15} />
                          )}
                          <span title={entry.name}>{entry.name}</span>
                          <small>{entry.size ? size(entry.size) : "解压后确认"}</small>
                        </label>
                      );
                    })}
                  </details>
                ) : (
                  <div className="inline-unavailable">
                    压缩包写入尚未接入，请选择解压后的镜像。
                  </div>
                ))}
              {firmware && !inspecting && (
                <>
                  {rawAddressError && <p id="raw-address-error" className="field-error">{rawAddressError}</p>}
                  {firmware.warnings.length > 0 && <div className="firmware-warnings" role="status">
                    <TriangleAlert size={15} /><div>{firmware.warnings.map((warning, i) => <p key={i}>{warning}</p>)}</div>
                  </div>}
                </>
              )}
            </section>
            </div>
            {!showTaskStatus && <section className="flash-action" aria-label="烧录选项与操作">
              <div className="write-options">
                <label>
                  <input
                    type="checkbox"
                    checked={prefs.reboot}
                    disabled={busy}
                    onChange={(e) =>
                      setPrefs((p) => ({ ...p, reboot: e.target.checked }))
                    }
                  />
                  完成后重启
                </label>
              </div>
              <button
                className="button primary start-button"
                disabled={!ready || busy}
                title={
                  device?.mode === "maskrom" && !canFlashFromMaskrom ? "请先加载 Loader" : undefined
                }
                onClick={() => void prepare("flash")}
              >
                <Zap size={16} />
                开始烧录…
              </button>
            </section>}
          </div>
        )}

        {page === "partitions" && (
          <section className="panel partition-panel">
            <div className="panel-heading page-panel-heading">
              <div className="segmented-control" aria-label="分区视图">
                <button
                  className={partitionView === "table" ? "selected" : ""}
                  onClick={() => setPartitionView("table")}
                >
                  设备分区
                </button>
                <button
                  className={partitionView === "address" ? "selected" : ""}
                  onClick={() => setPartitionView("address")}
                >
                  按地址写入
                </button>
              </div>
              <span className="single-device-summary" title={device?.connection}>{deviceSummary}</span>
            </div>
            {partitionView === "table" ? (
              <>
                <div className="section-toolbar">
                  <span className="subtle">
                    {partitions.length
                      ? partitions.length + " 个分区"
                      : "GPT 分区表"}
                  </span>
                  <div className="button-row">
                    <button
                      className="button secondary"
                      disabled={!device || busy || !loaderMode}
                      onClick={() => void prepare("backup")}
                    >
                      <ArrowDownToLine size={15} />
                      整机备份
                    </button>
                    <button
                      className="button primary"
                      disabled={!device || busy || reading || !loaderMode}
                      onClick={() => void readPartitions()}
                    >
                      <RefreshCw size={15} className={reading ? "spin" : ""} />
                      {reading ? "读取中" : "读取分区"}
                    </button>
                  </div>
                </div>
                {partitions.length ? (
                  <>
                    <div className="storage-map">
                      {partitions.map((p, i) => (
                        <div
                          key={p.name}
                          style={{
                            flex: Math.max(
                              p.size /
                                partitions.reduce((a, b) => a + b.size, 0),
                              0.06,
                            ),
                            background: "var(--storage-" + (i % 4) + ")",
                          }}
                          title={p.name + " · " + size(p.size)}
                        />
                      ))}
                    </div>
                    <div className="table-scroll">
                      <table>
                        <thead>
                          <tr>
                            <th>分区名称</th>
                            <th>起始扇区</th>
                            <th>容量</th>
                            <th className="align-right">操作</th>
                          </tr>
                        </thead>
                        <tbody>
                          {partitions.map((p, i) => (
                            <tr key={p.name + "-" + p.startSector}>
                              <td>
                                <span className="partition-label">
                                  <span
                                    style={{
                                      background:
                                        "var(--storage-" + (i % 4) + ")",
                                    }}
                                  />
                                  <FileCode2 size={16} />
                                  <strong>{p.name}</strong>
                                </span>
                              </td>
                              <td className="mono">
                                {"0x" +
                                  p.startSector.toString(16).padStart(8, "0")}
                              </td>
                              <td>{size(p.size)}</td>
                              <td>
                                <div className="table-actions">
                                  <button
                                    disabled={busy || !loaderMode}
                                    onClick={() => void prepare("flash", p)}
                                  >
                                    <Upload size={14} />
                                    写入
                                  </button>
                                  <button
                                    disabled={busy || !loaderMode}
                                    onClick={() => void prepare("backup", p)}
                                  >
                                    <ArrowDownToLine size={14} />
                                    备份
                                  </button>
                                  <button
                                    className="danger-text"
                                    disabled={busy || !loaderMode}
                                    onClick={() => void prepare("erase", p)}
                                  >
                                    <Trash2 size={14} />
                                    擦除
                                  </button>
                                </div>
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  </>
                ) : (
                  <div className="large-empty">
                    <Layers3 size={28} strokeWidth={1.5} />
                    <h3>
                      {!device
                        ? "未连接设备"
                        : !loaderMode
                          ? "设备处于 Maskrom"
                          : "尚未读取分区"}
                    </h3>
                    {device && !loaderMode && (
                      <button
                        className="button secondary"
                        onClick={() => setPage("flash")}
                      >
                        加载 Loader
                        <ArrowRight size={14} />
                      </button>
                    )}
                  </div>
                )}
              </>
            ) : (
              <>
                <div className="section-toolbar">
                  <span className="subtle">
                    {!native && !demo ? "请使用桌面应用 · " : ""}
                    {addressRows.filter((r) => r.enabled).length} 个已选条目
                  </span>
                  <button
                    className="text-button"
                    disabled={busy}
                    onClick={() => {
                      const id = ++rowSequence.current;
                      setAddressRows((rows) => [
                        ...rows,
                        {
                          id,
                          enabled: true,
                          name: "镜像 " + id,
                          address: "0x0",
                          file: null,
                        },
                      ]);
                    }}
                  >
                    <Plus size={15} />
                    添加条目
                  </button>
                </div>
                <div className="table-scroll address-table">
                  <table>
                    <thead>
                      <tr>
                        <th className="check-cell">
                          <input
                            type="checkbox"
                            aria-label="全选条目"
                            checked={
                              addressRows.length > 0 &&
                              addressRows.every((r) => r.enabled)
                            }
                            disabled={busy}
                            onChange={(e) =>
                              setAddressRows((rows) =>
                                rows.map((r) => ({
                                  ...r,
                                  enabled: e.target.checked,
                                })),
                              )
                            }
                          />
                        </th>
                        <th>名称</th>
                        <th>起始扇区</th>
                        <th>文件</th>
                        <th />
                      </tr>
                    </thead>
                    <tbody>
                      {addressRows.map((row) => (
                        <tr key={row.id}>
                          <td className="check-cell">
                            <input
                              type="checkbox"
                              aria-label={"选择 " + row.name}
                              checked={row.enabled}
                              disabled={busy}
                              onChange={(e) =>
                                changeRow(row.id, { enabled: e.target.checked })
                              }
                            />
                          </td>
                          <td>
                            <input
                              className="row-name"
                              aria-label={"条目 " + row.id + " 名称"}
                              value={row.name}
                              disabled={busy}
                              onChange={(e) =>
                                changeRow(row.id, { name: e.target.value })
                              }
                            />
                          </td>
                          <td>
                            <input
                              className="address-input"
                              aria-label={row.name + " 起始扇区"}
                              value={row.address}
                              disabled={busy}
                              onChange={(e) =>
                                changeRow(row.id, { address: e.target.value })
                              }
                            />
                          </td>
                          <td>
                            <button
                              className="row-file"
                              title={row.file?.path}
                              disabled={busy}
                              onClick={() => void chooseRowFile(row)}
                            >
                              <FolderOpen size={15} />
                              <span>{row.file?.name || "选择文件…"}</span>
                            </button>
                          </td>
                          <td>
                            <button
                              className="icon-button"
                              aria-label={"移除 " + row.name}
                              title="移除条目"
                              disabled={busy}
                              onClick={() =>
                                setAddressRows((rows) =>
                                  rows.filter((r) => r.id !== row.id),
                                )
                              }
                            >
                              <X size={15} />
                            </button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                <div className="address-actions">
                  <button
                    className="button primary"
                    title={
                      !native && !demo
                        ? "请使用桌面应用"
                        : !loaderMode
                          ? "请先加载 Loader"
                          : undefined
                    }
                    disabled={
                      (!native && !demo) || !device || !loaderMode || busy || !addressReady
                    }
                    onClick={prepareAddressWrite}
                  >
                    <Upload size={15} />
                    执行写入…
                  </button>
                </div>
              </>
            )}
          </section>
        )}

        {page === "advanced" && (
          <section className="panel advanced-panel">
            <div className="panel-heading page-panel-heading">
              <h2>
                <Settings2 size={17} />
                高级工具
              </h2>
            </div>
            <div className="tool-group">
              <h3>设备信息</h3>
              <div className="tool-group-content">
                <div className="tool-buttons">
                  {[
                    "Flash ID",
                    "Flash 信息",
                    "Chip 信息",
                    "Capability",
                    "测试设备",
                  ].map((title) => (
                    <button
                      key={title}
                      className="button secondary"
                      title={
                        (!native && !demo) || !loaderMode ? unsupportedTitle : undefined
                      }
                      disabled={(!native && !demo) || !loaderMode || busy}
                      onClick={() => showInformation(title)}
                    >
                      {title}
                    </button>
                  ))}
                </div>
                <div className="device-info-slot" aria-live="polite">
                  {toolResult ? <div className="tool-result">
                    <strong>{toolResult.title}</strong>
                    <dl>{toolResult.values.map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{value}</dd></div>)}</dl>
                  </div> : <span className="info-placeholder">{loaderMode ? "选择上方项目读取设备信息" : "加载 Loader 后读取设备信息"}</span>}
                </div>
              </div>
            </div>
            <div className="tool-group">
              <h3>设备控制</h3>
              <div className="tool-group-content">
                <div className="tool-buttons">
                  <button
                    className="button secondary"
                    disabled={!device || busy}
                    onClick={() => void submit(requestFor("reboot"))}
                  >
                    {shortKind === "reboot" ? (
                      <LoaderCircle size={14} className="spin" />
                    ) : (
                      <RotateCcw size={14} />
                    )}
                    重启设备
                  </button>
                  <button
                    className="button secondary"
                    disabled={!loaderMode || busy}
                    onClick={() => void submit(requestFor("maskrom"))}
                  >
                    进入 Maskrom
                  </button>
                  <button
                    className="button secondary"
                    disabled
                    title="USB3 切换的版本与协议待核"
                  >
                    切换 USB3
                  </button>
                </div>
                <div className="tool-form storage-form">
                  <label htmlFor="storage-choice">存储</label>
                  <select
                    id="storage-choice"
                    value={storageChoice}
                    disabled={(!native && !demo) || !loaderMode || busy}
                    onChange={(e) => setStorageChoice(e.target.value)}
                  >
                    {[["emmc", "eMMC"], ["sd0", "SD"], ["spi-nor", "SPI NOR"], ["spi-nand", "SPI NAND"], ["ram", "RAM"]].map(([value, label]) => (
                      <option key={value} value={value}>{label}</option>
                    ))}
                  </select>
                  <button
                    className="button secondary"
                    title={!native && !demo ? "请使用桌面应用" : undefined}
                    disabled={(!native && !demo) || !loaderMode || busy}
                    onClick={() => void switchStorage()}
                  >
                    切换
                  </button>
                  <span className="subtle">
                    当前：{loaderMode ? storage : "—"}
                  </span>
                </div>
                <div className="tool-buttons reserved-tools">
                  <button
                    className="button secondary"
                    disabled
                    title="清空序列号的协议待核"
                  >
                    清空序列号
                  </button>
                  <button
                    className="button secondary"
                    disabled
                    title="检测安全模式的协议待核"
                  >
                    检测安全模式
                  </button>
                  <button
                    className="button secondary"
                    disabled
                    title="设备串口日志的协议待核"
                  >
                    设备串口日志
                  </button>
                </div>
              </div>
            </div>
            <details className="tool-group advanced-tool-disclosure">
              <summary>
                <h3>读取与文件</h3>
                <ChevronDown size={14} />
              </summary>
              <div className="tool-group-content">
                <div className="tool-buttons">
                  <button
                    className="button secondary"
                    title={!native && !demo ? "请使用桌面应用" : undefined}
                    disabled={(!native && !demo) || busy}
                    onClick={() => void unpackFirmware()}
                  >
                    <FileArchive size={14} />
                    解包固件…
                  </button>

                </div>
                <div className="tool-form export-form">
                  <span className="form-label">导出镜像</span>
                  <label>
                    起始扇区
                    <input
                      className="address-input"
                      value={exportAddress}
                      disabled={(!native && !demo) || !loaderMode || busy}
                      onChange={(e) => setExportAddress(e.target.value)}
                    />
                  </label>
                  <label>
                    扇区数
                    <input
                      className="address-input"
                      value={exportSectors}
                      disabled={(!native && !demo) || !loaderMode || busy}
                      onChange={(e) => setExportSectors(e.target.value)}
                    />
                  </label>
                  <button
                    className="button secondary"
                    title={!native && !demo ? "请使用桌面应用" : undefined}
                    disabled={(!native && !demo) || !loaderMode || busy}
                    onClick={() => void exportImage()}
                  >
                    <ArrowDownToLine size={14} />
                    导出…
                  </button>
                </div>
              </div>
            </details>
            <details className="tool-group advanced-tool-disclosure danger-group">
              <summary>
                <h3>擦除存储</h3>
                <ChevronDown size={14} />
              </summary>
              <div className="tool-group-content">
                <div className="segmented-control erase-mode" aria-label="擦除方式">
                  {([["range", "按地址"], ["all", "全部存储"], ["partition", "按分区"]] as const).map(([value, label]) =>
                    <button key={value} className={eraseMode === value ? "selected" : ""} disabled={busy} onClick={() => setEraseMode(value)}>{label}</button>
                  )}
                </div>
                {eraseMode !== "all" && <div className="erase-input-slot">
                {eraseMode === "range" && <div className="tool-form">
                  <label>起始扇区 <input className="address-input" value={eraseAddress} disabled={!loaderMode || busy} onChange={(e) => setEraseAddress(e.target.value)} /></label>
                  <label>扇区数 <input className="address-input" value={eraseSectors} disabled={!loaderMode || busy} onChange={(e) => setEraseSectors(e.target.value)} /></label>
                </div>}
                {eraseMode === "partition" && <div className="tool-form erase-form">
                  <select aria-label="擦除目标分区" value={erasePartition} disabled={!loaderMode || busy} onChange={(e) => setErasePartition(e.target.value)}>
                    <option value="">选择分区</option>
                    {partitions.map((p) => <option key={p.name} value={p.name}>{p.name}</option>)}
                  </select>
                  <button className="button secondary" disabled={!loaderMode || busy || reading} onClick={() => void readPartitions()}>{reading ? "读取中" : "读取分区"}</button>
                </div>}
                </div>}
                <div className="tool-form erase-action-row">
                  <button className="button secondary danger-text" disabled={!loaderMode || busy || !eventsReady || (eraseMode === "range" ? !!eraseRangeError : eraseMode === "all" ? !eraseAllReady : !currentErasePartition)} onClick={prepareErase}>
                    <Trash2 size={14} />{eraseMode === "all" ? "擦除全部…" : eraseMode === "range" ? "擦除范围…" : "擦除分区…"}
                  </button>
                  <span className="subtle">{eraseMode === "all" ? `包括分区表与引导 · ${size(device?.capacity ?? null)}` : "每扇区 512 B · 支持十六进制"}</span>
                </div>
                {loaderMode && eraseMode === "range" && eraseRangeError && <p className="field-error erase-validation-slot">{eraseRangeError}</p>}
              </div>
            </details>

          </section>
        )}

        </div>

        <section className="panel console-panel">
          <div className="console-heading">
            <div className="console-title">
              <Terminal size={16} />
              <strong>运行日志</strong>
              <span className="count-badge">{logs.length}</span>
            </div>
            <div className="console-actions">
                <div className="log-tabs">
                  <button
                    className={filter === "all" ? "selected" : ""}
                    onClick={() => changeLogFilter("all")}
                  >
                    全部
                  </button>
                  <button
                    className={filter === "error" ? "selected" : ""}
                    onClick={() => changeLogFilter("error")}
                  >
                    警告与错误
                  </button>
                </div>
                <button
                  className="icon-button"
                  disabled={busy}
                  onClick={() => {
                    followLogs.current = true;
                    logAnchor.current = null;
                    setLogs([]);
                  }}
                  aria-label="清空日志"
                  title="清空日志"
                >
                  <Trash2 size={14} />
                </button>
              </div>
          </div>
            <div
              ref={logsBody}
              className="console-body"
              id="runtime-log"
              role="log"
              aria-label="运行日志"
              onScroll={(e) => {
                const body = e.currentTarget;
                followLogs.current = body.scrollHeight - body.clientHeight - body.scrollTop <= 24;
                rememberLogAnchor();
              }}
            >
              {visibleLogs.length ? (
                visibleLogs.map((line) => (
                  <div className="log-line" key={line.id} data-log-id={line.id}>
                    <time>
                      {new Date(line.time).toLocaleTimeString("zh-CN", {
                        hour12: false,
                      })}
                    </time>
                    <span className={"log-level " + line.level}>
                      {
                        {
                          info: "INFO",
                          error: "ERROR",
                          warning: "WARN",
                          success: "DONE",
                        }[line.level]
                      }
                    </span>
                    <span>{line.text}</span>
                  </div>
                ))
              ) : (
                <div className="log-empty">
                  {filter === "error" ? "暂无警告或错误" : "暂无日志"}
                </div>
              )}
            </div>
        </section>

      </main>

      {showStatus && (
        <section
          className={"job-panel " + statusKind + (statusFailed ? " has-error" : "") + (compactProgress ? " compact-progress" : "")}
          aria-live="polite"
        >
          <div className="job-top">
            <div className="job-label">
              {taskRunning ? (
                <LoaderCircle size={18} className="spin" />
              ) : showTaskStatus && job?.status === "completed" ? (
                <Check size={18} />
              ) : (
                <TriangleAlert size={18} />
              )}
              <div>
                <strong>{statusTitle}</strong>
                {job?.status === "running" && job.bytesDone != null && job.bytesTotal != null && (
                  <span className="job-bytes">{size(job.bytesDone)} / {size(job.bytesTotal)}</span>
                )}
                {statusMessage && <span>{statusMessage}</span>}
              </div>
            </div>
            <div className="job-controls">
              {taskRunning && (
                <strong className="progress-number">
                  {job?.progress != null ? Math.round(job.progress) + "%" : "—"}
                </strong>
              )}
              {taskRunning && activeJobId && (
                <button
                  className="button secondary"
                  disabled={stopping}
                  title="停止任务不会撤销已写入或擦除的数据"
                  onClick={() => void cancel()}
                >
                  <Square size={12} />
                  {stopping ? "正在停止…" : "停止任务"}
                </button>
              )}
              {error && busy && (
                <button className="icon-button" onClick={() => setError(null)} aria-label="关闭错误">
                  <X size={16} />
                </button>
              )}
              {!busy && (!multipleDevices || !!error || !!job) && (
                <button
                  className="icon-button"
                  onClick={() => { setJob(null); setError(null); }}
                  aria-label="关闭状态提示"
                >
                  <X size={16} />
                </button>
              )}
            </div>
          </div>
          {showTaskStatus && (job?.status === "cancelled" || job?.status === "failed") && (
            <p className="recovery-note">若任务已开始写入或擦除，已修改的数据不会回滚。请检查连接与供电，重新确认设备后完整重试或恢复备份。</p>
          )}
          {taskRunning && (
            <div
              className={
                "progress-track" +
                (job?.progress == null ? " indeterminate" : "")
              }
            >
              <span style={{ width: (job?.progress || 0) + "%" }} />
            </div>
          )}
        </section>
      )}

      {picker && (
        <Modal
          title="演示文件"
          subtitle={picker.loader ? "Loader" : "固件"}
          close={() => closePicker()}
          footer={
            <button className="button secondary" onClick={() => closePicker()}>
              取消
            </button>
          }
        >
          <div className="file-picker-path">
            <FolderOpen size={15} />
            <span>文件 / Rockchip</span>
          </div>
          <div className="sample-file-list">
            {(picker.loader
              ? [loaderSample]
              : firmwareSamples.filter(
                  (file) => !picker.kind || file.info?.kind === picker.kind,
                )
            ).map((file) => (
              <button
                key={file.path}
                className="sample-file"
                onClick={() => closePicker(file)}
              >
                {picker.loader ? (
                  <PlugZap size={22} />
                ) : file.archive ? (
                  <FileArchive size={22} />
                ) : (
                  <FileImage size={22} />
                )}
                <span>
                  <strong>{file.name}</strong>
                  <small>
                    {picker.loader
                      ? "Loader"
                      : file.archive
                        ? file.archive.kind === "stream"
                          ? "单镜像压缩文件"
                          : "多文件压缩包"
                        : file.info?.kind === "rkfw"
                          ? "Rockchip 固件"
                          : "磁盘镜像"}
                  </small>
                </span>
                <small>{size(file.size)}</small>
                <ArrowRight size={15} />
              </button>
            ))}
          </div>
        </Modal>
      )}
      {pending && (
        <Modal
          title={pending.label + "？"}
          close={() => setPending(null)}
          footer={
            <>
              <button
                className="button secondary"
                onClick={() => setPending(null)}
              >
                取消
              </button>
              <button
                className={
                  "button " +
                  (pending.request.kind === "erase" ? "danger" : "primary")
                }
                disabled={
                  devices.length !== 1 || !devices.some(
                    (d) => d.id === pending.target.id &&
                      (d.mode === "loader" || (d.mode === "maskrom" && pending.allowMaskrom)),
                  )
                }
                onClick={() => {
                  const selected = pending;
                  setPending(null);
                  void submit(selected.request, selected.options);
                }}
              >
                确认{pending.label}
                <ArrowRight size={15} />
              </button>
            </>
          }
        >
          <div className="confirm-target">
            <Cpu size={25} />
            <div>
              <strong>{deviceDisplayName(pending.target)}</strong>
              <small>{pending.target.connection}</small>
            </div>
            <span className="mode-badge">{modeLabel(pending.target.mode)}</span>
          </div>
          <dl className="confirm-details">
            {pending.file && (
              <div>
                <dt>文件</dt>
                <dd className="break-path">{shortFileLabel(pending.file)}</dd>
              </div>
            )}
            <div>
              <dt>范围</dt>
              <dd>{pending.detail}</dd>
            </div>
          </dl>
          {pending.request.kind === "flash" && <p className="confirm-options">
            {pending.request.reboot ? "完成后重启" : "完成后保持连接"}
          </p>}
          {pending.warnings?.map((warning, i) => <p className="field-error" key={i}>{warning}</p>)}
          <div className="confirm-warning">
            <TriangleAlert size={17} />
            <p>
              目标范围内的数据将被
              {pending.request.kind === "erase" ? "擦除" : "覆盖"}
              ，此操作无法撤销。
            </p>
          </div>
        </Modal>
      )}
      {settingsOpen && (
        <PreferencesDialog
          savedTheme={prefs.theme}
          saveTheme={(theme) => setPrefs((p) => ({ ...p, theme }))}
          close={() => setSettingsOpen(false)}
        />
      )}

    </div>
  );
}
