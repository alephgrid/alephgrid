// Wire types come from the generated protocol.ts; only client-side unions live here.
export type {
  Align, CellEdit, CellValue, Column, ColumnType, EditOutcome, FilePreview, Filter, FilterOp, ImportProblem, ImportRequest,
  Imported, JobStatus, Kind, Op, OpName, Params, PreviewColumn, UploadCreated, UploadPreview,
  QueryPreview, Results, RowBlock, SavedView, SheetBlock, SheetCell, SheetCopy, SheetInfo, SheetInput, SortKey, Stats, TableInfo,
  ViewInfo, ViewSpec, WorkbookListing, WorkbookMeta, WorkbookPatch,
  AuditEvent, AuthMode, Invite, MemberKind, Org, OrgMember, OrgRole, PublicUser, Space, SpaceMember, SpaceRole, Token,
} from "./protocol";
import type { Event, Hello } from "./protocol";

export type ServerEvent = Hello | Event;
