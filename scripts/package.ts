import { createHash, randomUUID } from "node:crypto";
import { mkdir, rm, stat, writeFile, chmod, rename } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const crateManifest = join(repoRoot, "ufo-cli", "Cargo.toml");
const crateBinary = join(repoRoot, "ufo-cli", "target", "release", "xbrd-selector");
const distDir = join(repoRoot, "dist");
const distBinary = join(distDir, "xbrd-selector");

export function assertSupportedTarget(platform = process.platform, arch = process.arch): void {
  if (platform !== "linux" || arch !== "x64") {
    throw new Error(`unsupported target: ${platform}-${arch}; expected linux-x64`);
  }
}

export async function cargoBuildRelease(): Promise<void> {
  const proc = Bun.spawn([
    "cargo",
    "build",
    "--release",
    "--locked",
    "--manifest-path",
    crateManifest,
  ], {
    cwd: repoRoot,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });
  const code = await proc.exited;
  if (code !== 0) {
    throw new Error(`cargo build failed with exit code ${code}`);
  }
}

export function sha256Hex(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

export function validateNativeArtifact(bytes: Uint8Array, mode: number): void {
  if (bytes.length < 4 || bytes[0] !== 0x7f || bytes[1] !== 0x45 || bytes[2] !== 0x4c || bytes[3] !== 0x46) {
    throw new Error("native artifact is not an ELF binary");
  }
  if ((mode & 0o111) === 0) {
    throw new Error("native artifact is not executable");
  }
}

async function readExact(path: string): Promise<Uint8Array> {
  return new Uint8Array(await Bun.file(path).arrayBuffer());
}

export async function copyBinaryAtomically(source: string, destination: string): Promise<void> {
  const sourceStat = await stat(source);
  if (!sourceStat.isFile()) {
    throw new Error(`source artifact is not a file: ${source}`);
  }

  const sourceBytes = await readExact(source);
  validateNativeArtifact(sourceBytes, sourceStat.mode);

  await mkdir(dirname(destination), { recursive: true });
  const tempPath = join(dirname(destination), `.${basename(destination)}.${randomUUID()}.tmp`);
  try {
    await writeFile(tempPath, sourceBytes);
    await chmod(tempPath, sourceStat.mode & 0o777);

    const tempBytes = await readExact(tempPath);
    if (sha256Hex(tempBytes) !== sha256Hex(sourceBytes)) {
      throw new Error("checksum mismatch while staging native artifact");
    }
    if (tempBytes.length !== sourceBytes.length) {
      throw new Error("byte length mismatch while staging native artifact");
    }

    await rename(tempPath, destination);
  } finally {
    await rm(tempPath, { force: true }).catch(() => {});
  }
}

function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? "xbrd-selector";
}

export async function packageNativeBinary(): Promise<void> {
  assertSupportedTarget();
  await cargoBuildRelease();
  await copyBinaryAtomically(crateBinary, distBinary);
}

async function main(): Promise<void> {
  await packageNativeBinary();
  console.log(`packaged ${distBinary}`);
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error instanceof Error ? error.stack ?? error.message : String(error));
    process.exit(1);
  });
}
