// U.04a / U.T05: commands a space role does not allow are disabled where they are offered, with
// the reason, while the server still refuses them (docs/DESIGN.md, screen states).

import type { MenuItem } from "./ui";

/** Why a viewer cannot use a command that changes the workbook. */
export const VIEW_ONLY = "View only";

/** Menus whose every command changes the workbook. */
const CHANGING_MENUS = new Set(["Insert", "Format"]);
/** Commands of the other menus that change the workbook (viewing and exporting stay). */
const CHANGING = new Set(["New sheet", "Import file…", "Rename workbook…", "Undo", "Redo", "Paste special…",
  "Find and replace…", "Data tools…"]);

/** `items` of menu-bar menu `menu` as a viewer sees them. */
export function forViewer(menu: string, items: MenuItem[]): MenuItem[] {
  return items.map((item) => {
    if (item === "sep" || item.disabled) return item;
    const changes = CHANGING_MENUS.has(menu) || CHANGING.has(item.label) || /^(Un)?freeze /i.test(item.label);
    return changes ? { ...item, disabled: VIEW_ONLY } : item;
  });
}
