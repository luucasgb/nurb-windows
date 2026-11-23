import assert from "node:assert/strict";
import test from "node:test";
import { folderName, numbered } from "../src/downloads.ts";

test("a saved file names the folder it went to", () => {
  assert.equal(folderName("C:\\Users\\maker\\Downloads"), "Downloads");
  assert.equal(folderName("/Users/maker/Prints/"), "Prints");
  assert.equal(folderName("D:\\"), "D:");
});

test("a second copy is named the way Finder names one", () => {
  assert.equal(numbered("drill_bits.3mf", 2), "drill_bits 2.3mf");
  assert.equal(numbered("Notch v2.step", 3), "Notch v2 3.step");
  assert.equal(numbered("README", 2), "README 2");
});
