import type { FirmwareInfo } from "./types";

// Preview identification only. Native firmware.rs remains the authoritative
// container parser; never pass this metadata to a native flashing operation.
const HEADER_BYTES = 4096;

/** A display-only relative identifier, not a local filesystem path. */
export function browserFirmwarePath(file: Pick<File, "name">): string {
  return `browser-file/${encodeURIComponent(file.name)}`;
}

function matches(bytes: Uint8Array, signature: readonly number[], offset = 0): boolean {
  return bytes.length >= offset + signature.length
    && signature.every((value, index) => bytes[offset + index] === value);
}

function ascii(bytes: Uint8Array, signature: string, offset = 0): boolean {
  return matches(bytes, Array.from(signature, (char) => char.charCodeAt(0)), offset);
}

/** Read at most 4 KiB, without uploading, extracting, or opening any device. */
export async function inspectBrowserFirmware(file: File): Promise<FirmwareInfo> {
  if (!Number.isSafeInteger(file.size) || file.size <= 0) {
    throw new Error("请选择非空的固件文件。");
  }

  let prefix: Uint8Array;
  try {
    prefix = new Uint8Array(await file.slice(0, HEADER_BYTES).arrayBuffer());
  } catch {
    throw new Error("无法读取此文件的文件头，请重新选择一个可读取的本地文件。");
  }
  if (prefix.length !== Math.min(file.size, HEADER_BYTES)) {
    throw new Error("文件头读取不完整，请重新选择文件。");
  }

  // Match the native parser's encoded-image rejection before classifying raw.
  const extension = file.name.split(".").pop()?.toLowerCase() ?? "";
  const archive = /^(gz|gzip|tgz|xz|bz2|zip|zst|zstd|lz4|7z|tar|rar)$/.test(extension)
    || matches(prefix, [0x1f, 0x8b])
    || ascii(prefix, "PK\x03\x04") || ascii(prefix, "PK\x05\x06")
    || ascii(prefix, "PK\x07\x08") || ascii(prefix, "BZh")
    || ascii(prefix, "\xfd7zXZ\0") || ascii(prefix, "7z\xbc\xaf\x27\x1c")
    || ascii(prefix, "Rar!\x1a\x07")
    || matches(prefix, [0x28, 0xb5, 0x2f, 0xfd])
    || matches(prefix, [0x04, 0x22, 0x4d, 0x18])
    || matches(prefix, [0x02, 0x21, 0x4c, 0x18])
    || ascii(prefix, "ustar", 257);
  if (archive) {
    throw new Error("这是压缩包或归档文件。浏览器预览不解压或列出归档内容，请先在本地解压，再拖入 .img / .bin 固件。");
  }
  if (matches(prefix, [0x3a, 0xff, 0x26, 0xed])) {
    throw new Error("这是 Android sparse 镜像，请先转换为 RAW 镜像；浏览器预览不执行转换。");
  }
  if (/^(qcow|qcow2|vmdk|vdi|vhd|vhdx|sparseimage)$/.test(extension)
    || ascii(prefix, "QFI\xfb") || ascii(prefix, "KDMV")
    || ascii(prefix, "# Disk DescriptorFile") || ascii(prefix, "sprs")
    || ascii(prefix, "vhdxfile") || ascii(prefix, "<<< Oracle VM VirtualBox Disk Image >>>")) {
    throw new Error("这是虚拟磁盘或稀疏容器，请先导出为 RAW 镜像；浏览器预览不执行转换。");
  }

  let kind: FirmwareInfo["kind"] = "raw";
  const warnings = [
    "浏览器轻量识别：仅读取最多 4 KiB 文件头；未进行完整容器校验、CRC / 签名验证或解包，识别结果不代表固件可安全烧录。",
    "browser-file/ 是浏览器文件的显示标识，不是本机路径。浏览器预览不会连接 USB 或执行烧录；实际操作需在桌面版重新选择并校验文件。",
  ];
  if (ascii(prefix, "RKFW")) {
    kind = "rkfw";
    warnings.push("检测到 RKFW 文件头；尚未解析内部 Loader、分区、机型或厂商信息。");
  } else if (ascii(prefix, "RKAF")) {
    kind = "rkfw";
    warnings.push("检测到独立 RKAF 文件头；此格式不包含 RAM Loader。尚未解析归档成员或分区。");
  } else if (ascii(prefix, "BOOT") || ascii(prefix, "LDR ")) {
    kind = "loader";
    warnings.push("检测到 BOOT / LDR Loader 文件头；未校验条目、CRC 或芯片兼容性。");
  } else {
    warnings.push("未检测到 RKFW、RKAF 或 Loader 标识，暂按 RAW / 未知二进制显示；这不能确认它是有效固件，未解析分区表或写入地址。");
  }

  return {
    path: browserFirmwarePath(file),
    name: file.name,
    size: file.size,
    kind,
    model: null,
    manufacturer: null,
    partitions: [],
    warnings,
  };
}
