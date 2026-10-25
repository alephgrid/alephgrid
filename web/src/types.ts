// Mirrors crates/rowfinity-engine/src/model.rs

export type Kind = "int" | "number" | "bool" | "date" | "datetime" | "time" | "text";
export type CellValue = string | number | boolean | null;

export interface Column {
  name: string;
  sql_type: string;
  kind: Kind;
  formula?: string;
}

export interface TableInfo {
  name: string;
  row_count: number;
  column_count: number;
}

export interface SortKey {
  column: string;
  desc: boolean;
}

export type FilterOp =
  | "eq" | "neq" | "gt" | "gte" | "lt" | "lte"
  | "contains" | "not_contains" | "starts_with" | "is_empty" | "not_empty";

export interface Filter {
  column: string;
  op: FilterOp;
  value?: CellValue;
}

export interface ViewSpec {
  table: string;
  sort: SortKey[];
  filters: Filter[];
}

export interface ViewInfo {
  view_id: number;
  spec: ViewSpec;
  row_count: number;
  columns: Column[];
  can_append: boolean;
}

export interface RowBlock {
  row_start: number;
  rows: CellValue[][];
}

export interface CellEdit {
  row: number;
  column: string;
  value: string | null;
}

export interface Stats {
  rows: number;
  count: number;
  sum?: number;
  avg?: number;
  min?: CellValue;
  max?: CellValue;
}

export interface QueryPreview {
  columns: Column[];
  rows: CellValue[][];
  truncated: boolean;
}

export interface WorkbookMeta {
  id: string;
  name: string;
  created_at: number;
  workspace?: string | null;
  starred?: boolean;
  opened_at?: number;
  updated_at?: number;
  summary?: { tables: number; sheets: number; rows: number };
}

export type ServerEvent =
  | { event: "hello"; session: number; workbook: WorkbookMeta }
  | { event: "cells_changed"; table: string; origin: number }
  | { event: "table_changed"; table: string; origin: number }
  | { event: "tables_changed"; origin: number }
  | { event: "sheet_changed"; sheet: number | null; origin: number };

export interface EditOutcome {
  table: string;
  cells_changed: number;
  rows_appended: number;
}

export type Align = "left" | "right" | "center";
/** [display, align, input?]; input is present when it differs from display (formulas). */
export type SheetCell = [string, Align, string?];

export interface SheetInfo {
  index: number;
  name: string;
  rows: number;
  columns: number;
}

export interface SheetBlock {
  row_start: number;
  rows: (SheetCell | null)[][];
}

export interface Workspace {
  id: string;
  name: string;
  created_at: number;
}

export interface WorkbookListing extends WorkbookMeta {
  size_bytes: number;
}

export type Imported =
  | { kind: "table"; table: TableInfo }
  | { kind: "sheets"; sheets: SheetInfo[] };
