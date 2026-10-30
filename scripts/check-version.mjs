import { readFileSync } from 'node:fs';

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8').replace(/\r\n/g, '\n');
const json = (path) => JSON.parse(read(path));
const version = json('package.json').version;
const lock = json('package-lock.json');
const cargoVersion = read('src-tauri/Cargo.toml').match(/^version\s*=\s*"([^"]+)"/m)?.[1];
const cargoLockVersion = read('src-tauri/Cargo.lock')
  .match(/\[\[package\]\]\nname = "rkdevtool-next"\nversion = "([^"]+)"/)?.[1];

for (const [source, actual] of Object.entries({
  'package-lock.json': lock.version,
  'package-lock.json root package': lock.packages[''].version,
  'tauri.conf.json': json('src-tauri/tauri.conf.json').version,
  'Cargo.toml': cargoVersion,
  'Cargo.lock': cargoLockVersion,
})) {
  if (actual !== version) throw new Error(`${source}: expected ${version}, got ${actual}`);
}

if (process.env.GITHUB_REF_TYPE === 'tag' && process.env.GITHUB_REF_NAME !== `v${version}`) {
  throw new Error(`Release tag must be v${version}; got ${process.env.GITHUB_REF_NAME}`);
}
console.log(`RKDevTool Next version: ${version}`);
