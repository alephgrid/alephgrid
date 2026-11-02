// The clipboard TSV codec (pure functions; no server or browser needed).

import { expect, test } from "@playwright/test";
import { parseTsv, tsvCell } from "../src/tsv";

test("parseTsv reads quotes, embedded tabs and newlines, and CRLF", () => {
  expect(parseTsv("a\tb\nc\td")).toEqual([["a", "b"], ["c", "d"]]);
  expect(parseTsv('a\t"b\tc"\n"x""y"\r\nz')).toEqual([["a", "b\tc"], ['x"y'], ["z"]]);
  expect(parseTsv('"line 1\nline 2"\tnext')).toEqual([["line 1\nline 2", "next"]]);
  expect(parseTsv("a\r\nb\rc")).toEqual([["a"], ["b"], ["c"]]);
  expect(parseTsv("a\t\tb")).toEqual([["a", "", "b"]]);
  expect(parseTsv("a\n")).toEqual([["a"]]);
  expect(parseTsv("")).toEqual([]);
  // A quote that does not start the cell is text.
  expect(parseTsv('5" pipe')).toEqual([['5" pipe']]);
});

test("tsvCell quotes only cells that need it, and round-trips through parseTsv", () => {
  expect(tsvCell("plain")).toBe("plain");
  expect(tsvCell(null)).toBe("");
  expect(tsvCell(12.5)).toBe("12.5");
  expect(tsvCell(true)).toBe("true");
  expect(tsvCell('say "hi"')).toBe('"say ""hi"""');
  expect(tsvCell("a\tb")).toBe('"a\tb"');
  const cells = ["x", 'q"uote', "tab\there", "new\nline", ""];
  expect(parseTsv(cells.map(tsvCell).join("\t"))).toEqual([cells]);
});
