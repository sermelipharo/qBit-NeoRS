// Small line icons drawn for this UI on a 24×24 grid (stroked with
// `currentColor`; a leading "F" marks a filled path).

const C = (x: number, y: number, r: number) => `M${x - r} ${y}a${r} ${r} 0 1 0 ${2 * r} 0a${r} ${r} 0 1 0 ${-2 * r} 0z`;
const R = (x: number, y: number, w: number, hh: number, r: number) =>
  `M${x + r} ${y}h${w - 2 * r}a${r} ${r} 0 0 1 ${r} ${r}v${hh - 2 * r}a${r} ${r} 0 0 1 ${-r} ${r}h${-(w - 2 * r)}a${r} ${r} 0 0 1 ${-r} ${-r}v${-(hh - 2 * r)}a${r} ${r} 0 0 1 ${r} ${-r}z`;
const RING = C(12, 12, 8.5);

const P: Record<string, string[]> = {
  plus: ["M12 5v14M5 12h14"],
  play: ["F M8 5.6v12.8c0 .8.9 1.3 1.6.8l9.8-6.4c.6-.4.6-1.2 0-1.6L9.6 4.8C8.9 4.3 8 4.8 8 5.6z"],
  stop: ["F " + R(6.5, 6.5, 11, 11, 2)],
  trash: ["M4.5 7h15M10 11v6M14 11v6M6.5 7l.9 11.6A2 2 0 0 0 9.4 20.5h5.2a2 2 0 0 0 2-1.9L17.5 7M9.5 7V4.8c0-.4.4-.8.8-.8h3.4c.4 0 .8.4.8.8V7"],
  sliders: ["M4 7.5h8.5M17.5 7.5H20M4 16.5h2.5M11.5 16.5H20", C(15, 7.5, 2.5), C(9, 16.5, 2.5)],
  search: [C(10.5, 10.5, 6.2), "M15.2 15.2L20 20"],
  list: ["M9 6.5h11M9 12h11M9 17.5h11", "F " + C(5, 6.5, 1.3), "F " + C(5, 12, 1.3), "F " + C(5, 17.5, 1.3)],
  cards: [R(4, 4, 7, 7, 1.8), R(13, 4, 7, 7, 1.8), R(4, 13, 7, 7, 1.8), R(13, 13, 7, 7, 1.8)],
  sidebar: [R(3, 4.5, 18, 15, 3), "M9.5 4.5v15M5.8 8h1.4M5.8 11h1.4"],
  down: ["M12 4.5v14M6.5 13l5.5 5.5 5.5-5.5"],
  up: ["M12 19.5v-14M6.5 11L12 5.5l5.5 5.5"],
  x: ["M6.5 6.5l11 11M17.5 6.5l-11 11"],
  check: ["M5 12.5l4.5 4.5L19 7.5"],
  chevR: ["M9.5 6l6 6-6 6"],
  chevD: ["M6 9.5l6 6 6-6"],
  chevL: ["M14.5 6l-6 6 6 6"],
  tag: ["M3.8 11.6V5.3c0-.8.7-1.5 1.5-1.5h6.3c.4 0 .8.2 1.1.4l7.6 7.6c.6.6.6 1.5 0 2.1l-6.3 6.3c-.6.6-1.5.6-2.1 0l-7.6-7.6c-.3-.3-.5-.7-.5-1z", "F " + C(8, 8, 1.4)],
  folder: ["M3.5 8V6.3c0-1 .8-1.8 1.8-1.8h4l2 2.3h7.4c1 0 1.8.8 1.8 1.8v9.1c0 1-.8 1.8-1.8 1.8H5.3c-1 0-1.8-.8-1.8-1.8zM3.5 9.5h17"],
  tray: ["M4 13.5l2.1-7.1c.2-.5.6-.9 1.2-.9h9.4c.6 0 1 .4 1.2.9l2.1 7.1v4.7c0 1-.8 1.8-1.8 1.8H5.8c-1 0-1.8-.8-1.8-1.8zM4 13.5h4.3c.3 1.8 1.8 3 3.7 3s3.4-1.2 3.7-3H20"],
  dl: [RING, "M12 7.5v8.5M8.5 12.6l3.5 3.5 3.5-3.5"],
  ul: [RING, "M12 16.5V8M8.5 11.4L12 7.9l3.5 3.5"],
  done: [RING, "M8 12.3l2.7 2.7 5.3-5.6"],
  run: [RING, "M10.3 8.8v6.4l5-3.2z"],
  stopped: [RING, "M10 9.2v5.6M14 9.2v5.6"],
  bolt: ["M13.2 3L5.5 13.4h6l-1 7.6 8-10.6h-6z"],
  moon: ["M19.5 14.6A8 8 0 1 1 9.4 4.5a6.4 6.4 0 0 0 10.1 10.1z"],
  clock: [RING, "M12 7.5V12l3 2"],
  sync: ["M19.5 12a7.5 7.5 0 1 1-2.2-5.3M19.5 4.5v4h-4"],
  move: ["M4 12h15M13.5 6.5L19 12l-5.5 5.5"],
  warn: ["M10.7 4.6L3 18c-.6 1 .1 2.2 1.3 2.2h15.4c1.2 0 1.9-1.2 1.3-2.2L13.3 4.6c-.6-1-2-1-2.6 0zM12 9.5v4.5", "F " + C(12, 17, 1.1)],
  antenna: [C(12, 12, 1.6), "M8.6 8.6a4.8 4.8 0 0 0 0 6.8M15.4 8.6a4.8 4.8 0 0 1 0 6.8M5.6 5.6a9 9 0 0 0 0 12.8M18.4 5.6a9 9 0 0 1 0 12.8"],
  rss: ["M5.5 4.5a14 14 0 0 1 14 14M5.5 10.5a8 8 0 0 1 8 8", "F " + C(6.5, 17.5, 1.7)],
  gauge: ["M4.6 17.5a8.2 8.2 0 1 1 14.8 0M12 13.5l3.6-4.2", "F " + C(12, 13.5, 1.6)],
  more: ["F " + C(6, 12, 1.6), "F " + C(12, 12, 1.6), "F " + C(18, 12, 1.6)],
  info: [RING, "M12 11v5.5", "F " + C(12, 8, 1.1)],
  link: ["M10.3 13.7a4 4 0 0 0 5.6 0l3-3a4 4 0 0 0-5.6-5.6l-1 1M13.7 10.3a4 4 0 0 0-5.6 0l-3 3a4 4 0 0 0 5.6 5.6l1-1"],
  doc: ["M7.5 3.5h6L18.5 8.5v10.3c0 .9-.8 1.7-1.7 1.7H7.5c-.9 0-1.7-.8-1.7-1.7V5.2c0-.9.8-1.7 1.7-1.7zM13.2 3.7v5h5"],
  pencil: ["M4.5 19.5l.9-3.9L16 5a2 2 0 0 1 2.9 0l.1.1a2 2 0 0 1 0 2.9L8.4 18.6zM14.5 6.5l3 3"],
  magnet: ["M5.5 4.5h4v7.3a2.5 2.5 0 0 0 5 0V4.5h4v7.3a6.5 6.5 0 0 1-13 0zM5.5 8.5h4M14.5 8.5h4"],
  globe: [RING, "M3.5 12h17M12 3.5c2.4 2.3 3.6 5.2 3.6 8.5s-1.2 6.2-3.6 8.5c-2.4-2.3-3.6-5.2-3.6-8.5S9.6 5.8 12 3.5z"],
  arrows: ["M8 4.5v15M4.5 8L8 4.5 11.5 8M16 19.5v-15M12.5 16l3.5 3.5 3.5-3.5"],
  queue: ["M4.5 6.5h15M4.5 12h15M4.5 17.5h9"],
};

const NS = "http://www.w3.org/2000/svg";

export function icon(name: string, cls = ""): SVGSVGElement {
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "i " + cls);
  svg.setAttribute("aria-hidden", "true");
  for (const d of P[name] ?? []) {
    const p = document.createElementNS(NS, "path");
    if (d.startsWith("F ")) {
      p.setAttribute("d", d.slice(2));
      p.setAttribute("class", "f");
    } else p.setAttribute("d", d);
    svg.appendChild(p);
  }
  return svg;
}
