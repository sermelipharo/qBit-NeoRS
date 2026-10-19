// Virtualized list / grid: only the rows in view (plus a small margin)
// exist in the DOM; elements are reused, and a row is redrawn only when its
// item's version or the list epoch (selection, view options) changed.

import { h } from "./util";

export interface VOpts<T> {
  /** CSS custom property on the scroller holding the row height (px). */
  rowVar: string;
  /** Minimum card width for grid mode (0 = one column). */
  minWidth?: () => number;
  gap?: () => number;
  create(): HTMLElement;
  update(el: HTMLElement, item: T, index: number): void;
  key(item: T): string;
  version(item: T): number;
}

interface Slot {
  key: string;
  v: number;
  epoch: number;
  index: number;
}

export class VList<T> {
  readonly canvas: HTMLElement;
  items: T[] = [];
  private live = new Map<string, HTMLElement>();
  private free: HTMLElement[] = [];
  private slots = new WeakMap<HTMLElement, Slot>();
  private rowH = 40;
  private cols = 1;
  private colW = 0;
  private gapPx = 0;
  private epoch = 0;
  private raf = 0;

  constructor(readonly scroller: HTMLElement, private o: VOpts<T>) {
    this.canvas = h("div", { class: "vcanvas", role: "presentation" });
    scroller.appendChild(this.canvas);
    scroller.addEventListener("scroll", () => this.schedule(), { passive: true });
    // Sizes come from CSS (media queries), so every resize re-measures and
    // re-positions the rendered rows.
    new ResizeObserver(() => this.relayout()).observe(scroller);
  }

  private measure(): void {
    const cs = getComputedStyle(this.scroller);
    this.rowH = parseFloat(cs.getPropertyValue(this.o.rowVar)) || 40;
    const w = this.canvas.clientWidth;
    const min = this.o.minWidth?.() ?? 0;
    this.gapPx = this.o.gap?.() ?? 0;
    this.cols = min > 0 ? Math.max(1, Math.floor((w + this.gapPx) / (min + this.gapPx))) : 1;
    this.colW = this.cols > 1 || min > 0 ? (w - this.gapPx * (this.cols - 1)) / this.cols : 0;
  }

  /** Forces every visible row to redraw (selection, mode). */
  invalidate(): void {
    this.epoch++;
  }

  setItems(items: T[]): void {
    this.items = items;
    this.render();
  }

  relayout(): void {
    this.measure();
    this.invalidate();
    this.render();
  }

  schedule(): void {
    if (!this.raf) this.raf = requestAnimationFrame(() => this.render());
  }

  scrollToIndex(i: number): void {
    const cs = getComputedStyle(this.scroller);
    const pt = parseFloat(cs.paddingTop) || 0, pb = parseFloat(cs.paddingBottom) || 0;
    const y = this.canvas.offsetTop + Math.floor(i / this.cols) * this.rowH;
    const st = this.scroller.scrollTop, view = this.scroller.clientHeight;
    if (y < st + pt) this.scroller.scrollTop = y - pt;
    else if (y + this.rowH > st + view - pb) this.scroller.scrollTop = y + this.rowH - view + pb;
  }

  render(): void {
    this.raf = 0;
    const rows = Math.ceil(this.items.length / this.cols);
    const height = rows * this.rowH;
    if (this.canvas.style.height !== height + "px") this.canvas.style.height = height + "px";
    const top = this.scroller.scrollTop - this.canvas.offsetTop;
    const view = this.scroller.clientHeight;
    const first = Math.max(0, Math.floor(top / this.rowH) - 4);
    const last = Math.min(rows, Math.ceil((top + view) / this.rowH) + 4);
    const from = first * this.cols, to = Math.min(this.items.length, last * this.cols);
    const want = new Map<string, number>();
    for (let i = from; i < to; i++) want.set(this.o.key(this.items[i]), i);
    for (const [k, el] of this.live) {
      if (!want.has(k)) {
        this.live.delete(k);
        el.remove();
        this.free.push(el);
      }
    }
    for (const [k, i] of want) {
      const item = this.items[i];
      let el = this.live.get(k);
      if (!el) {
        el = this.free.pop() ?? this.o.create();
        this.live.set(k, el);
        this.canvas.appendChild(el);
      }
      let s = this.slots.get(el);
      if (!s) this.slots.set(el, (s = { key: "", v: -1, epoch: -1, index: -1 }));
      const v = this.o.version(item);
      if (s.key !== k || s.v !== v || s.epoch !== this.epoch || s.index !== i) {
        if (s.index !== i || s.epoch !== this.epoch) {
          const r = Math.floor(i / this.cols), c = i % this.cols;
          const x = this.colW ? c * (this.colW + this.gapPx) : 0;
          el.style.transform = `translate(${x}px,${r * this.rowH}px)`;
          el.style.width = this.colW ? this.colW + "px" : "";
        }
        s.key = k;
        s.v = v;
        s.epoch = this.epoch;
        s.index = i;
        this.o.update(el, item, i);
      }
    }
    // Keep the pool small.
    if (this.free.length > 64) this.free.length = 64;
  }

  /** Element currently showing `key`, if rendered. */
  elementFor(key: string): HTMLElement | undefined {
    return this.live.get(key);
  }
}
