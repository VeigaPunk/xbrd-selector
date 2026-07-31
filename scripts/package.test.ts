import { expect, test } from "bun:test";
import { chmod, mkdir, stat, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { randomUUID } from "node:crypto";
import {
  assertSupportedTarget,
  copyBinaryAtomically,
  sha256Hex,
  validateNativeArtifact,
} from "./package";

function tempDir(): string {
  return join(tmpdir(), `xbrd-selector-${randomUUID()}`);
}

test("rejects unsupported targets", () => {
  expect(() => assertSupportedTarget("darwin", "x64")).toThrow(/unsupported target/);
  expect(() => assertSupportedTarget("linux", "arm64")).toThrow(/unsupported target/);
});

test("rejects malformed artifacts", () => {
  expect(() => validateNativeArtifact(new Uint8Array([0x00, 0x01, 0x02, 0x03]), 0o755)).toThrow(/ELF/);
  expect(() => validateNativeArtifact(new Uint8Array([0x7f, 0x45, 0x4c, 0x46]), 0o644)).toThrow(/executable/);
});

test("package metadata points bin directly at the native binary", async () => {
  const pkg = await Bun.file(join(process.cwd(), "package.json")).json();
  expect(pkg.bin["xbrd-selector"]).toBe("dist/xbrd-selector");
  expect(pkg.bin["xbrd-selector"]).not.toMatch(/\.js$/);
  expect(pkg.files).toContain("dist/xbrd-selector");
});

test("preserves bytes and checksum exactly", async () => {
  const root = tempDir();
  await mkdir(root, { recursive: true });
  const source = join(root, "source");
  const destination = join(root, "dist", "xbrd-selector");
  await mkdir(join(root, "dist"), { recursive: true });

  const bytes = new Uint8Array([
    0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01, 0x01, 0x00, 0x11, 0x22, 0x33, 0x44,
  ]);
  await writeFile(source, bytes);
  await chmod(source, 0o755);

  await copyBinaryAtomically(source, destination);

  const sourceBytes = new Uint8Array(await Bun.file(source).arrayBuffer());
  const destinationBytes = new Uint8Array(await Bun.file(destination).arrayBuffer());
  expect(destinationBytes).toEqual(sourceBytes);
  expect(sha256Hex(destinationBytes)).toBe(sha256Hex(sourceBytes));
  expect((await stat(destination)).mode & 0o111).not.toBe(0);
});
