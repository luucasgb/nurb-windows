import assert from "node:assert/strict";
import test from "node:test";
import { formatBytes } from "../src/files.ts";

test("a size reads the way a file manager writes it", () => {
  assert.equal(formatBytes(0), "0 bytes");
  assert.equal(formatBytes(999), "999 bytes");
  assert.equal(formatBytes(1000), "1 KB");
  assert.equal(formatBytes(20_036_448), "20 MB");
  assert.equal(formatBytes(1_234_000_000), "1.2 GB");
  assert.equal(formatBytes(9_960_000), "10 MB");
  assert.equal(formatBytes(999_999), "1 MB");
});

test("Explorer's sizes count in 1024s", () => {
  assert.equal(formatBytes(1023, 1024), "1023 bytes");
  assert.equal(formatBytes(20 * 1024 * 1024, 1024), "20 MB");
  assert.equal(formatBytes(1_288_490_189, 1024), "1.2 GB");
});
