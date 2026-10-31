/**
 * Event-gap detection (7.08). The server announces every committed revision with at least
 * one event, in revision order, over one connection. A client that sees a revision skip
 * missed a change (dropped frames, a server that lost events) and must refresh what it
 * shows; an event at or below the last one seen is a repeat or a late copy (derived
 * events share their change's revision) and is applied as usual, since refreshing twice
 * is harmless.
 */
export class RevisionTracker {
  /** Highest revision known to be reflected in what is shown. */
  last: number;

  constructor(start = 0) {
    this.last = start;
  }

  /** A fresh connection said the workbook is at `revision`. True when changes were missed
   *  since the last revision seen (the caller refreshes everything after a reconnect). */
  hello(revision: number): boolean {
    const missed = revision > this.last;
    this.last = revision;
    return missed;
  }

  /** An event announced `revision`. True when revisions before it were never announced. */
  see(revision: number): boolean {
    if (revision <= this.last) return false;
    const gap = revision > this.last + 1;
    this.last = revision;
    return gap;
  }

  /** Everything up to `revision` was refreshed (after a resync). */
  caughtUp(revision: number): void {
    this.last = Math.max(this.last, revision);
  }
}
