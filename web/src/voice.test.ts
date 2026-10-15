import { expect, test } from "bun:test";
import { floatTo16, heard, locale } from "./voice";

test("converts samples to 16 bits, clipping what is out of range", () => {
  expect(Array.from(floatTo16(new Float32Array([0, 1, -1, 2, -2, 0.5])))).toEqual([0, 32767, -32768, 32767, -32768, 16383]);
});

test("the browser's results: final parts, then the guess", () => {
  const r = (t: string, isFinal: boolean) => ({ isFinal, 0: { transcript: t } });
  expect(heard([r("hello there", true), r(" how are", false)])).toEqual({ done: "hello there", guess: " how are" });
});

test("languages become locales", () => {
  expect(locale("auto", "pt-BR")).toBe("pt-BR");
  expect(locale("pt", "en-US")).toBe("pt-BR");
  expect(locale("en", "pt-BR")).toBe("en-US");
});
