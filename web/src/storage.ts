// localStorage that never throws: private browsing, disabled storage or a full quota make
// these do nothing (and reads return null), and the page carries on without them.

export function stored(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}

export function store(key: string, value: string): void {
  try { localStorage.setItem(key, value); } catch { /* kept for this page only */ }
}

export function unstore(key: string): void {
  try { localStorage.removeItem(key); } catch { /* nothing to remove */ }
}
