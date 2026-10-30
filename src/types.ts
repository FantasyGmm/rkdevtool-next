export type DeviceMode = "loader" | "maskrom" | "unknown";
export interface DeviceInfo {
  id: string;
  name: string;
  chip: string;
  chipModel?: string | null;
  mode: DeviceMode;
  vid: string;
  pid: string;
  connection: string;
  capacity: number | null;
}
export interface PartitionInfo {
  name: string;
  startSector: number;
  sectors: number;
  size: number;
}
export interface FirmwareInfo {
  path: string;
  name: string;
  size: number;
  kind: "rkfw" | "raw" | "loader";
  model: string | null;
  manufacturer: string | null;
  partitions: { name: string; size: number; startSector: number | null }[];
  warnings: string[];
}
export type OperationKind =
  "flash" | "backup" | "erase" | "loader" | "reboot" | "maskrom" | "unpack";
export interface OperationRequest {
  deviceId: string;
  kind: OperationKind;
  firmwarePath: string | null;
  outputPath: string | null;
  startSector: number;
  sectors: number | null;
  reboot: boolean;
  confirmed: boolean;
  writes?: { name: string; path: string; startSector: number }[];
  archiveMember?: string;
  partitionName?: string;
}
export interface OperationEvent {
  jobId: string;
  status: "running" | "completed" | "failed" | "cancelled";
  stage: string;
  progress: number | null;
  message: string;
  level: "info" | "success" | "warning" | "error";
  timestamp: string;
  bytesDone: number | null;
  bytesTotal: number | null;
}
export interface HistoryEntry {
  id: string;
  device: string;
  operation: OperationKind;
  file: string | null;
  status: "completed" | "failed" | "cancelled";
  startedAt: string;
  durationMs: number;
  message: string;
}
export interface RuntimeInfo {
  version: string;
  platform: string;
  backend: string;
}

export type DeviceQuery = "flashId" | "flashInfo" | "chipInfo" | "capability" | "testDevice" | "currentStorage";
export interface DeviceReport { title: string; values: [string, string][]; currentStorage?: string; }
export interface ArchiveInfo { path: string; format: "zip" | "gzip" | "xz" | "bzip2"; entries: { name: string; size: number | null }[]; }
