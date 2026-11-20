// The Glass theme's lens: how the picture behind the panel is bent near the
// panel's rim, the way a pane of thick glass bends what is seen through its
// edge. Pure maths here; main.ts turns the map into an SVG filter.

// How far in from the rim the bending reaches, and the largest shift (CSS px),
// reached only at the rim itself: the edge is a rounded slab of glass (see
// `refraction`), so inside the band the picture barely moves.
export const LENS_EDGE = 56;
export const LENS_SHIFT = 40;

// Glass bends light by its refractive index; ordinary glass is about 1.5.
const GLASS_INDEX = 1.5;

// How far light is bent through the rounded edge at `t` (0 where the band
// starts inside, 1 at the rim), as a share of the bend at the rim. The edge's
// profile is a squircle, height (1 - t^4)^(1/4): flat at first, falling
// steeply at the rim. Light coming straight through a surface tilted by
// angle a leaves it bent by a - asin(sin a / n) (Snell's law); the picture
// moves by the tangent of that.
export function refraction(t: number): number {
  const deviation = (x: number) => {
    const slope = x >= 1 ? Infinity : x ** 3 / (1 - x ** 4) ** 0.75;
    const a = Math.atan(slope);
    return Math.tan(a - Math.asin(Math.sin(a) / GLASS_INDEX));
  };
  return deviation(t) / deviation(1);
}

// How much of the band and the shift a panel `w` x `h` gets, 0 to 1: at most
// 22% of its shorter side, so on the short empty bar the bend never reaches
// across its middle (where it would smear the picture into stripes).
export function lensFit(w: number, h: number, edge: number): number {
  return Math.min(1, (Math.min(w, h) * 0.22) / edge);
}

// The displacement map for a panel `width` x `height` with corners of
// `radius`: RGBA bytes, row by row. Red is the horizontal shift and green the
// vertical one, 128 meaning none; a pixel at the rim is shifted towards the
// panel's middle by `LENS_SHIFT`, easing to nothing `edge` px in as glass
// would (`refraction`). So the rim shows what lies further inside, stretched:
// a magnifying edge.
export function lensMap(width: number, height: number, radius: number, edge: number = LENS_EDGE): Uint8ClampedArray {
  const w = Math.max(1, Math.round(width));
  const h = Math.max(1, Math.round(height));
  const out = new Uint8ClampedArray(w * h * 4);
  const hx = w / 2;
  const hy = h / 2;
  const r = Math.min(radius, hx, hy);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      // Position from the middle, folded into one quadrant.
      const px = x + 0.5 - hx;
      const py = y + 0.5 - hy;
      const qx = Math.abs(px) - (hx - r);
      const qy = Math.abs(py) - (hy - r);
      // Depth inside the rounded rectangle, and the outward direction there.
      let depth: number;
      let nx: number;
      let ny: number;
      if (qx > 0 && qy > 0) {
        const len = Math.hypot(qx, qy);
        depth = r - len;
        nx = len > 0 ? qx / len : 0;
        ny = len > 0 ? qy / len : 0;
      } else if (qx > qy) {
        depth = -qx + r;
        nx = 1;
        ny = 0;
      } else {
        depth = -qy + r;
        nx = 0;
        ny = 1;
      }
      // 1 at the rim, 0 from `edge` px in, bent as real glass would.
      const t = Math.min(1, Math.max(0, 1 - depth / edge));
      const bend = refraction(t);
      const i = (y * w + x) * 4;
      out[i] = 128 - Math.sign(px) * nx * bend * 127;
      out[i + 1] = 128 - Math.sign(py) * ny * bend * 127;
      out[i + 2] = 128;
      out[i + 3] = 255;
    }
  }
  return out;
}

// `feDisplacementMap`'s scale for a map made by `lensMap`: a full red or
// green byte then moves a pixel by `LENS_SHIFT`.
export const LENS_SCALE = LENS_SHIFT * 2;

// Glass splits light a little: red bends most at the rim and blue least, so
// edges in the picture get a thin colour fringe there. Each channel's share
// of `LENS_SCALE`.
export const LENS_SPREAD: [number, number, number] = [1.25, 1, 0.78];

// Whether a picture is light enough that dark text reads better on it.
// `mean` is its average brightness, 0 (black) to 255 (white).
export function inkIsDark(mean: number): boolean {
  return mean > 150;
}

const SVG = "http://www.w3.org/2000/svg";

// Keeps an SVG filter in step with `panel`'s size and applies it to `back`
// (the layer holding the picture), while the Glass theme is on. Call the
// returned function when the theme changes; resizes are followed by itself.
export function installLens(
  panel: HTMLElement,
  back: HTMLElement,
  id: string,
  lens: { edge: number; shift: number; spread: [number, number, number] } = { edge: LENS_EDGE, shift: LENS_SHIFT, spread: LENS_SPREAD },
  // The selected row's mode: bend what lies behind an element (CSS
  // backdrop-filter, through `--row-lens` on <html>) instead of `back`
  // itself. `size` gives the element's width, height and corner radius;
  // `extra` is added after the lens, e.g. a brightness.
  behind?: { size: () => [number, number, number]; extra: string },
): () => void {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("width", "0");
  svg.setAttribute("height", "0");
  svg.setAttribute("aria-hidden", "true");
  svg.style.position = "absolute";
  const filter = document.createElementNS(SVG, "filter");
  filter.id = id;
  filter.setAttribute("filterUnits", "userSpaceOnUse");
  filter.setAttribute("color-interpolation-filters", "sRGB");
  const image = document.createElementNS(SVG, "feImage");
  image.setAttribute("result", "map");
  image.setAttribute("preserveAspectRatio", "none");
  filter.append(image);
  // The picture is bent once per colour channel, each by its own amount, and
  // the three are added back together: where nothing bends they agree, and
  // at the rim they drift apart into a fringe.
  const KEEP = ["1 0 0 0 0  0 0 0 0 0  0 0 0 0 0  0 0 0 1 0", "0 0 0 0 0  0 1 0 0 0  0 0 0 0 0  0 0 0 1 0", "0 0 0 0 0  0 0 0 0 0  0 0 1 0 0  0 0 0 1 0"];
  const bends: [SVGElement, number][] = [];
  lens.spread.forEach((share, channel) => {
    const bend = document.createElementNS(SVG, "feDisplacementMap");
    bends.push([bend, share]);
    bend.setAttribute("in", "SourceGraphic");
    bend.setAttribute("in2", "map");
    bend.setAttribute("scale", String(lens.shift * 2 * share));
    bend.setAttribute("xChannelSelector", "R");
    bend.setAttribute("yChannelSelector", "G");
    const keep = document.createElementNS(SVG, "feColorMatrix");
    keep.setAttribute("type", "matrix");
    keep.setAttribute("values", KEEP[channel]);
    keep.setAttribute("result", `c${channel}`);
    filter.append(bend, keep);
  });
  for (const [a, b, result] of [["c0", "c1", "c01"], ["c01", "c2", ""]]) {
    const add = document.createElementNS(SVG, "feBlend");
    add.setAttribute("mode", "screen");
    add.setAttribute("in", a);
    add.setAttribute("in2", b);
    if (result) add.setAttribute("result", result);
    filter.append(add);
  }
  svg.append(filter);
  document.body.append(svg);

  const canvas = document.createElement("canvas");
  let made = "";
  const update = () => {
    if (document.documentElement.dataset.glass !== "true") {
      if (behind) document.documentElement.style.removeProperty("--row-lens");
      else back.style.filter = "";
      made = "";
      return;
    }
    const [w, h, radius] = behind ? behind.size() : [panel.clientWidth, panel.clientHeight, parseFloat(getComputedStyle(panel).borderTopLeftRadius) || 0];
    const key = `${w}x${h}r${radius}`;
    if (w === 0 || h === 0 || key === made) return;
    made = key;
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const fit = lensFit(w, h, lens.edge);
    for (const [bend, share] of bends) bend.setAttribute("scale", String(lens.shift * 2 * share * fit));
    ctx.putImageData(new ImageData(lensMap(w, h, radius, lens.edge * fit), w, h), 0, 0);
    for (const el of [filter, image]) {
      el.setAttribute("x", "0");
      el.setAttribute("y", "0");
      el.setAttribute("width", String(w));
      el.setAttribute("height", String(h));
    }
    image.setAttribute("href", canvas.toDataURL());
    if (behind) document.documentElement.style.setProperty("--row-lens", `url(#${id})${behind.extra}`);
    else back.style.filter = `url(#${id})`;
  };
  new ResizeObserver(update).observe(panel);
  update();
  return update;
}
