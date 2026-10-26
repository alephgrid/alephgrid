// Wire types come from the generated protocol.ts; only client-side unions live here.
export type {
  Align, CellEdit, CellValue, Column, EditOutcome, Filter, FilterOp, Imported, Kind, Op, OpName, Params,
  QueryPreview, Results, RowBlock, SheetBlock, SheetCell, SheetInfo, SheetInput, SortKey, Stats, TableInfo,
  ViewInfo, ViewSpec, WorkbookListing, WorkbookMeta, WorkbookPatch, Workspace,
} from "./protocol";
import type { Event, Hello } from "./protocol";

export type ServerEvent = Hello | Event;
