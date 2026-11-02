// Sheet colour rules (S.10): condition matching and fills (pure functions; no server or browser).

import { expect, test } from "@playwright/test";
import { ruleFill, ruleMatches } from "../src/sheetrules";
import type { SheetColorRule, SheetRules } from "../src/protocol";

const area = { row: 0, column: 0, rows: 2, columns: 2 };
const rule = (condition: string, value: string, color = "#FFC7CE"): SheetColorRule => ({ area, condition, value, color });

test("ruleMatches checks each condition against the displayed text", () => {
  expect(ruleMatches(rule("greater", "10"), "1,000")).toBe(true);
  expect(ruleMatches(rule("greater", "10"), "")).toBe(false);
  expect(ruleMatches(rule("less", "10"), "9.5")).toBe(true);
  expect(ruleMatches(rule("less", "10"), "")).toBe(false);
  expect(ruleMatches(rule("equal", "yes"), "yes")).toBe(true);
  expect(ruleMatches(rule("equal", "yes"), "Yes")).toBe(false);
  expect(ruleMatches(rule("contains", "AB"), "xabx")).toBe(true);
  expect(ruleMatches(rule("empty", ""), "")).toBe(true);
  expect(ruleMatches(rule("not_empty", ""), "")).toBe(false);
  expect(ruleMatches(rule("scale", "0,100"), "50")).toBe(true);
  expect(ruleMatches(rule("scale", "0,100"), "n/a")).toBe(false);
  expect(ruleMatches(rule("unknown", ""), "x")).toBe(false);
});

test("ruleFill uses the first matching rule inside its area, and a cell's own fill wins", () => {
  const rules: SheetRules = { colors: [rule("greater", "5", "#00FF00"), rule("not_empty", "", "#0000FF")], validations: [] };
  expect(ruleFill(rules, undefined, () => "7", 0, 0)).toEqual({ fill: "#00FF00" });
  expect(ruleFill(rules, { b: true }, () => "1", 1, 1)).toEqual({ b: true, fill: "#0000FF" });
  expect(ruleFill(rules, undefined, () => "7", 2, 0)).toBeUndefined();
  expect(ruleFill(rules, { fill: "#123456" }, () => "7", 0, 0)).toEqual({ fill: "#123456" });
  const scale: SheetRules = { colors: [rule("scale", "0,100", "#FF0000")], validations: [] };
  expect(ruleFill(scale, undefined, () => "0", 0, 0)).toEqual({ fill: "#FF000028" });
  expect(ruleFill(scale, undefined, () => "100", 0, 0)).toEqual({ fill: "#FF0000ff" });
});
