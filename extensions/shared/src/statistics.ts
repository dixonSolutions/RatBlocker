/**
 * Local-only counters (§17): never transmitted, off unless the user enables
 * them, and bounded so a long session cannot grow without limit.
 */

const MAX_TABS = 256;

export class Statistics {
  private enabled = false;
  private total = 0;
  private readonly perTab = new Map<number, number>();

  setEnabled(enabled: boolean): void {
    this.enabled = enabled;
    if (!enabled) this.reset();
  }

  get isEnabled(): boolean {
    return this.enabled;
  }

  recordBlock(tabId: number | undefined): void {
    if (!this.enabled) return;
    this.total += 1;
    if (tabId === undefined || tabId < 0) return;
    if (!this.perTab.has(tabId) && this.perTab.size >= MAX_TABS) {
      // Drop the oldest entry rather than growing without bound.
      const oldest = this.perTab.keys().next();
      if (!oldest.done) this.perTab.delete(oldest.value);
    }
    this.perTab.set(tabId, (this.perTab.get(tabId) ?? 0) + 1);
  }

  forTab(tabId: number | undefined): number {
    if (tabId === undefined) return 0;
    return this.perTab.get(tabId) ?? 0;
  }

  get blockedTotal(): number {
    return this.total;
  }

  clearTab(tabId: number): void {
    this.perTab.delete(tabId);
  }

  reset(): void {
    this.total = 0;
    this.perTab.clear();
  }
}
