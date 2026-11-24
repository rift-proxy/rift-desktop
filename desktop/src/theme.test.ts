import { readFileSync } from "node:fs";
import path from "node:path";
import { describe, it, expect } from "vitest";

// The design system is `src/theme.css`: tokens in two :root blocks, rules built from them.
// These checks keep a rule from bringing a colour, a font or a size of its own back.
const css = readFileSync(path.join(import.meta.dirname, "theme.css"), "utf8");
const lightBlock = /:root\s*\{([^}]*)\}/.exec(css)![1];
const darkBlock = /:root\[data-theme="dark"\]\s*\{([^}]*)\}/.exec(css)![1];
const rules = css
  .replace(/:root\s*\{[^}]*\}/, "")
  .replace(/:root\[data-theme="dark"\]\s*\{[^}]*\}/, "")
  .replace(/@font-face\s*\{[^}]*\}/g, "")
  .replace(/\/\*[\s\S]*?\*\//g, "");
const names = (block: string) =>
  [...block.matchAll(/(--[\w-]+)\s*:/g)].map((m) => m[1]);

/** `oklch(L% C H)` as the relative luminance of the colour on screen (WCAG), through linear sRGB. */
function luminance(colour: string): number {
  const [, l, c, h] = /oklch\(\s*([\d.]+)%\s+([\d.]+)\s+([\d.]+)/.exec(colour)!;
  const L = Number(l) / 100;
  const a = Number(c) * Math.cos((Number(h) * Math.PI) / 180);
  const b = Number(c) * Math.sin((Number(h) * Math.PI) / 180);
  const lms = [
    L + 0.3963377774 * a + 0.2158037573 * b,
    L - 0.1055613458 * a - 0.0638541728 * b,
    L - 0.0894841775 * a - 1.291485548 * b,
  ].map((v) => v ** 3);
  const [r, g, bl] = [
    4.0767416621 * lms[0] - 3.3077115913 * lms[1] + 0.2309699292 * lms[2],
    -1.2684380046 * lms[0] + 2.6097574011 * lms[1] - 0.3413193965 * lms[2],
    -0.0041960863 * lms[0] - 0.7034186147 * lms[1] + 1.707614701 * lms[2],
  ].map((v) => Math.min(1, Math.max(0, v)));
  return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
}
const contrast = (a: string, b: string) => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
};
const token = (block: string, name: string) => {
  const at = block.indexOf(`${name}:`);
  return /^\s*(oklch\([^)]*\))/.exec(block.slice(at + name.length + 1))![1];
};

describe("the dot of an active connection", () => {
  it("is a green of its own and is seen against the page in both themes", () => {
    for (const block of [lightBlock, darkBlock]) {
      const dot = token(block, "--live");
      expect(contrast(dot, token(block, "--paper"))).toBeGreaterThanOrEqual(3);
      expect(contrast(dot, token(block, "--surface"))).toBeGreaterThanOrEqual(
        3,
      );
      // Green: the hue is the one of --ok, and it is brighter than the text colour of --ok.
      expect(/oklch\([\d.]+%\s+[\d.]+\s+(\d+)/.exec(dot)![1]).toBe("150");
    }
    expect(names(darkBlock)).toContain("--live");
  });
  it("is what 'connected' paints, and the other states keep their colours", () => {
    expect(
      /\[data-status="connected"\] \.status-dot\s*\{([^}]*)\}/.exec(rules)![1],
    ).toMatch(/background:\s*var\(--live\)/);
    expect(
      /\[data-status="connecting"\] \.status-dot\s*\{([^}]*)\}/.exec(rules)![1],
    ).toMatch(/background:\s*var\(--accent\)/);
    expect(
      /\[data-status="error"\] \.status-dot,\s*\[data-status="lost"\] \.status-dot\s*\{([^}]*)\}/.exec(
        rules,
      )![1],
    ).toMatch(/background:\s*var\(--bad\)/);
  });
});

describe("design system tokens", () => {
  it("keeps colour literals out of the rules", () => {
    expect(rules).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(rules).not.toMatch(
      /\b(?:rgba?|hsla?|oklch|oklab|lab|lch|color-mix)\(/,
    );
    expect(rules).not.toMatch(
      /(?<![-\w])(?:white|black|red|blue|gray|grey)(?![-\w])/,
    );
  });
  it("sets every font through a token", () => {
    const declarations = [...rules.matchAll(/\bfont(?:-family)?:\s*([^;]+);/g)];
    expect(declarations.length).toBeGreaterThan(2);
    for (const [, value] of declarations)
      expect(value).toMatch(/^inherit$|var\(--font\)/);
  });
  it("uses Onest for text and a bounded offline face for regional flags", () => {
    const stack = /--font:\s*([^;]+);/.exec(lightBlock)![1];
    expect(stack.trim()).toBe(
      '"Twemoji Country Flags", "Onest Variable", system-ui, sans-serif',
    );
    expect(lightBlock).not.toMatch(/--mono/);
    expect(css).not.toMatch(/monospace|Cascadia|Consolas|Segoe/);
  });
  it("never sets text below 12px, except the 11px explanations under settings", () => {
    const sizes = [...css.matchAll(/(--t-[\w]+):\s*(\d+)px/g)];
    expect(sizes.length).toBeGreaterThan(4);
    for (const [, name, px] of sizes) {
      if (name === "--t-hint") expect(Number(px), name).toBe(11);
      else expect(Number(px), name).toBeGreaterThanOrEqual(12);
    }
    // The 11px size belongs to that one rule and to no other.
    const small = [...rules.matchAll(/([^{}]+)\{[^{}]*var\(--t-hint\)/g)];
    expect(small.map((m) => m[1].trim())).toEqual([".setting-hint"]);
    for (const [, value] of rules.matchAll(/font-size:\s*([^;]+);/g))
      expect(value).toMatch(/var\(--t-[\w]+\)/);
  });
  it("sets an explanation 4px under its switch, in the 11px hint size", () => {
    expect(lightBlock).toMatch(/--s1:\s*4px;/);
    expect(/\.setting-hint\s*\{([^}]*)\}/.exec(rules)![1]).toMatch(
      /font-size:\s*var\(--t-hint\)/,
    );
    expect(
      /\.group \.toggle-label:has\(\+ \.setting-hint\)\s*\{([^}]*)\}/.exec(
        rules,
      )![1],
    ).toMatch(/margin-bottom:\s*var\(--s1\)/);
  });
  it("makes «Все серверы» 18px bold and keeps text unselectable except in fields", () => {
    expect(lightBlock).toMatch(/--t-section:\s*18px;/);
    expect(/\.source-all\s*\{([^}]*)\}/.exec(rules)![1]).toMatch(
      /font-size:\s*var\(--t-section\);[\s\S]*font-weight:\s*600;/,
    );
    expect(/(?<![-\w.])body\s*\{([^}]*)\}/.exec(rules)![1]).toMatch(
      /(?<![-\w])user-select:\s*none;/,
    );
    expect(/input,\s*textarea\s*\{([^}]*)\}/.exec(rules)![1]).toMatch(
      /(?<![-\w])user-select:\s*text;/,
    );
  });
  it("has no decoration the audit removed", () => {
    expect(rules).not.toMatch(/box-shadow|text-shadow|drop-shadow/);
    expect(rules).not.toMatch(/backdrop-filter|-gradient\(/);
    expect(rules).not.toMatch(/background-clip:\s*text/);
  });
  it("uses the named easing, never the browser default", () => {
    expect(rules).not.toMatch(/(?<![-\w])ease(?:-in|-out|-in-out)?(?![-\w])/);
    expect(rules).not.toMatch(/transition:\s*all|transition-property:\s*all/);
  });
  it("defines every dark token also in the light theme", () => {
    const light = new Set(names(lightBlock));
    for (const name of names(darkBlock))
      expect(light.has(name), name).toBe(true);
    // Colours are the part that changes: each light colour has a dark twin.
    const colours = names(lightBlock).filter((n) =>
      /^--(?:paper|surface|sunken|ink|ink-2|rule|track|accent|accent-hover|accent-ink|accent-soft|ok|ok-soft|live|bad|bad-ink|bad-soft|scrim)$/.test(
        n,
      ),
    );
    const dark = new Set(names(darkBlock));
    for (const name of colours) expect(dark.has(name), name).toBe(true);
  });
});
