// A small Rust lexer for the text check: the string literals of a source file, without comments
// and without the code that only the tests build (`#[cfg(test)]` items).
//
// It is not a Rust parser: it knows comments (line, block, nested), strings (plain, byte, raw
// with any number of `#`), characters against lifetimes, braces, and the `#[cfg(test)]` attribute.
// That is all the check needs to tell a text a person reads from a comment or a test.
//
// A text carries `i18n-ignore: <why>` on its line or the line above, like in the TypeScript check.
// `text` is the runtime value: cooked escapes and line continuations for ordinary strings,
// unchanged contents for raw strings. Source offsets and prefixes still refer to the literal.

/**
 * @typedef {{ line: number, column: number, text: string, prefix: string, start: number, end: number }} RustString
 */

const IDENT = /[\p{L}\p{N}_]/u;
const SIMPLE_ESCAPES = {
  "0": "\0",
  n: "\n",
  r: "\r",
  t: "\t",
  "'": "'",
  '"': '"',
  "\\": "\\",
};

/** The first character after a comment, or `from` when no comment starts there. */
function commentEnd(code, from) {
  if (code.startsWith("//", from)) {
    const end = code.indexOf("\n", from + 2);
    return end === -1 ? code.length : end;
  }
  if (!code.startsWith("/*", from)) return from;
  let nested = 1;
  let at = from + 2;
  while (at < code.length && nested > 0) {
    if (code.startsWith("/*", at)) {
      nested++;
      at += 2;
    } else if (code.startsWith("*/", at)) {
      nested--;
      at += 2;
    } else at++;
  }
  return at;
}

/** A raw delimiter has no fixed lookahead limit (Rust permits up to 255 hashes). */
function rawOpening(code, from) {
  if (from > 0 && IDENT.test(code[from - 1])) return null;
  let at = from;
  if (code[at] === "b") at++;
  if (code[at++] !== "r") return null;
  const hashesStart = at;
  while (code[at] === "#") at++;
  if (code[at] !== '"') return null;
  return {
    length: at + 1 - from,
    hashes: code.slice(hashesStart, at),
  };
}

/** The index of the `]` that closes an attribute whose body starts at `from`, brackets and strings inside it aside. */
function attributeEnd(code, from) {
  let nested = 1;
  for (let k = from; k < code.length; k++) {
    const afterComment = commentEnd(code, k);
    if (afterComment !== k) {
      k = afterComment - 1;
      continue;
    }
    const raw = rawOpening(code, k);
    if (raw) {
      const close = `"${raw.hashes}`;
      const end = code.indexOf(close, k + raw.length);
      if (end === -1) return code.length;
      k = end + close.length - 1;
    } else if (code[k] === '"') {
      k++;
      while (k < code.length && code[k] !== '"') k += code[k] === "\\" ? 2 : 1;
    } else if (code[k] === "[") nested++;
    else if (code[k] === "]" && --nested === 0) return k;
  }
  return code.length;
}

/**
 * Whether an attribute body is a `cfg` that only a test build satisfies: `cfg(test)`, or
 * `cfg(all(…, test, …))`. `not(test)` and `any(…)` also hold outside tests, so they are production.
 */
export function onlyForTests(body) {
  const tokens = [];
  const token = /\/\/[^\n]*|\/\*|[A-Za-z_][A-Za-z0-9_]*|[(),]|"(?:\\.|[^"\\])*"|=/g;
  for (let match; (match = token.exec(body)); ) {
    if (match[0].startsWith("//")) continue;
    if (match[0] === "/*") token.lastIndex = commentEnd(body, match.index);
    else tokens.push(match[0]);
  }
  let at = 0;
  const parse = () => {
    const name = tokens[at++];
    if (tokens[at] === "=") {
      at += 2; // `feature = "x"`
      return false;
    }
    if (tokens[at] !== "(") return name === "test";
    at++;
    const parts = [];
    while (tokens[at] !== ")" && at < tokens.length) {
      parts.push(parse());
      if (tokens[at] === ",") at++;
    }
    at++;
    return name === "all" && parts.some(Boolean);
  };
  if (tokens[0] !== "cfg" || tokens[1] !== "(") return false;
  at = 2;
  return parse();
}

/**
 * @param {string} code
 * @returns {RustString[]} the literals outside comments and outside `#[cfg(test)]` items.
 */
export function rustStrings(code) {
  /** @type {RustString[]} */
  const found = [];
  const n = code.length;
  let i = 0;
  let line = 1;
  let lineStart = 0;
  // After `#[cfg(test)]` the next item is test code: skip it up to its closing brace (or `;`).
  let skipItem = false;
  let skipDepth = 0;
  let depth = 0;

  const advance = (count = 1) => {
    for (let k = 0; k < count && i < n; k++) {
      if (code[i] === "\n") {
        line++;
        lineStart = i + 1;
      }
      i++;
    }
  };
  const startsWith = (text) => code.startsWith(text, i);

  while (i < n) {
    const c = code[i];
    const afterComment = commentEnd(code, i);
    if (afterComment !== i) {
      advance(afterComment - i);
      continue;
    }
    if (c === "#") {
      const attribute = /^#(!?)\[/.exec(code.slice(i, i + 3));
      if (attribute) {
        const close = attributeEnd(code, i + attribute[0].length);
        const body = code.slice(i + attribute[0].length, close);
        if (onlyForTests(body)) {
          // An inner cfg gates its containing module, or the whole crate at depth zero.
          if (attribute[1] && depth === 0) return found;
          if (!skipItem) {
            skipItem = true;
            skipDepth = attribute[1] ? depth - 1 : depth;
          }
          advance(close + 1 - i);
          continue;
        }
        // Any other attribute is read like code: `#[error("…")]` carries a text.
      }
    }
    if (c === "{" || c === "(" || c === "[") {
      depth++;
      advance();
      continue;
    }
    if (c === "}" || c === ")" || c === "]") {
      depth--;
      advance();
      if (skipItem && (depth < skipDepth || (c === "}" && depth === skipDepth)))
        skipItem = false;
      continue;
    }
    if (c === ";" && skipItem && depth === skipDepth) {
      skipItem = false;
      advance();
      continue;
    }
    // A raw string: r"…", r#"…"#, br"…", with a prefix that is not part of a longer name.
    const raw = rawOpening(code, i);
    if (raw) {
      const hashes = raw.hashes;
      const open = i;
      const startLine = line;
      const startColumn = i - lineStart + 1;
      advance(raw.length);
      const bodyStart = i;
      const close = `"${hashes}`;
      const end = code.indexOf(close, i);
      const bodyEnd = end === -1 ? n : end;
      advance(bodyEnd - i + close.length);
      if (!skipItem)
        found.push({
          line: startLine,
          column: startColumn,
          text: code.slice(bodyStart, bodyEnd),
          prefix: code.slice(open, bodyStart),
          start: open,
          end: i,
        });
      continue;
    }
    // A string: "…" or b"…".
    if (
      c === '"' ||
      (c === "b" &&
        code[i + 1] === '"' &&
        (i === 0 || !IDENT.test(code[i - 1])))
    ) {
      const startLine = line;
      const startColumn = i - lineStart + 1;
      const startIndex = i;
      const prefix = c === "b" ? 'b"' : '"';
      advance(prefix.length);
      let text = "";
      while (i < n && code[i] !== '"') {
        if (code[i] === "\\") {
          // A backslash before a newline joins the lines and drops the next indentation.
          if (
            code[i + 1] === "\n" ||
            (code[i + 1] === "\r" && code[i + 2] === "\n")
          ) {
            advance(code[i + 1] === "\r" ? 3 : 2);
            while (i < n && /[ \t\r\n]/.test(code[i])) advance();
            continue;
          }
          const escaped = /^\\(?:u\{([0-9a-fA-F][0-9a-fA-F_]*)\}|x([0-9a-fA-F]{2})|([0nrt'"\\]))/.exec(code.slice(i));
          if (escaped) {
            const hex = (escaped[1] ?? escaped[2])?.replaceAll("_", "");
            const point = hex === undefined ? null : Number.parseInt(hex, 16);
            if (
              point !== null &&
              hex.length <= 6 &&
              point <= 0x10ffff &&
              !(point >= 0xd800 && point <= 0xdfff)
            ) {
              text += String.fromCodePoint(point);
              advance(escaped[0].length);
              continue;
            }
            if (escaped[3]) {
              text += SIMPLE_ESCAPES[escaped[3]];
              advance(escaped[0].length);
              continue;
            }
          }
          text += code.slice(i, i + 2);
          advance(2);
          continue;
        }
        text += code[i];
        advance();
      }
      advance();
      if (!skipItem)
        found.push({
          line: startLine,
          column: startColumn,
          text,
          prefix,
          start: startIndex,
          end: i,
        });
      continue;
    }
    // A character or a lifetime.
    if (c === "'") {
      if (code[i + 1] === "\\") {
        // '\n', '\'', '\u{1F600}'
        const end = code.indexOf("'", i + 3);
        advance((end === -1 ? 2 : end - i) + 1);
        continue;
      }
      const point = String.fromCodePoint(code.codePointAt(i + 1) ?? 0x20);
      if (code[i + 1 + point.length] === "'") {
        advance(point.length + 2);
        continue;
      }
      advance(); // a lifetime or a label
      continue;
    }
    advance();
  }
  return found;
}
