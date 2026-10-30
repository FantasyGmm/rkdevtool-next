import { exampleFirmware } from "./bridge";
import type { FirmwareInfo } from "./types";

export type ArchiveEntry = { name: string; size: number };
export type ArchiveSelection = {
  path: string;
  name: string;
  size: number;
  kind: "stream" | "container";
  entries: ArchiveEntry[];
  selected: string | null;
  sample: boolean;
};
export type PickedFile = {
  path: string;
  name: string;
  size: number;
  info?: FirmwareInfo;
  archive?: ArchiveSelection;
};

const raw = (path: string, bytes: number): FirmwareInfo => ({
  path,
  name: path.split("/").pop()!,
  size: bytes,
  kind: "raw",
  model: null,
  manufacturer: null,
  partitions: [],
  warnings: [],
});
export const loaderSample: PickedFile = {
  path: "/demo/MiniLoaderAll.bin",
  name: "MiniLoaderAll.bin",
  size: 307200,
};
export const firmwareSamples: PickedFile[] = [
  {
    path: "/demo/debian.img",
    name: "debian.img",
    size: 4294967296,
    info: raw("/demo/debian.img", 4294967296),
  },
  {
    path: "/demo/update.img",
    name: "update.img",
    size: 3875536896,
    info: {
      ...exampleFirmware,
      path: "/demo/update.img",
      name: "update.img",
      kind: "rkfw",
    },
  },
  {
    path: "/demo/debian.img.xz",
    name: "debian.img.xz",
    size: 1258291200,
    archive: {
      path: "/demo/debian.img.xz",
      name: "debian.img.xz",
      size: 1258291200,
      kind: "stream",
      entries: [{ name: "debian.img", size: 4294967296 }],
      selected: "debian.img",
      sample: true,
    },
  },
  {
    path: "/demo/board-images.zip",
    name: "board-images.zip",
    size: 1610612736,
    archive: {
      path: "/demo/board-images.zip",
      name: "board-images.zip",
      size: 1610612736,
      kind: "container",
      entries: [
        { name: "images/debian.img", size: 4294967296 },
        { name: "images/recovery.img", size: 100663296 },
        { name: "README.txt", size: 2048 },
        { name: "checksums.sha256", size: 256 },
      ],
      selected: null,
      sample: true,
    },
  },
];

export const isArchive = (path: string) =>
  /\.(xz|gz|bz2|zip|7z|tar|tgz|tbz2|txz|zst|zstd|lz4|lzma|rar)$/i.test(path);
export const imageFromArchive = (archive: ArchiveSelection, name: string) => {
  const entry = archive.entries.find((item) => item.name === name);
  return entry && /\.img$/i.test(entry.name)
    ? raw(`${archive.path} › ${entry.name}`, entry.size)
    : null;
};
