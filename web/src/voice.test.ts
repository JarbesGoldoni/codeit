import { expect, test } from "bun:test";
import { floatTo16 } from "./voice";

test("converts samples to 16 bits, clipping what is out of range", () => {
  expect(Array.from(floatTo16(new Float32Array([0, 1, -1, 2, -2, 0.5])))).toEqual([0, 32767, -32768, 32767, -32768, 16383]);
});
