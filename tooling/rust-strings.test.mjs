import assert from "node:assert/strict";
import { test } from "node:test";
import { onlyForTests, rustStrings } from "./rust-strings.mjs";

const texts = (code) => rustStrings(code).map((s) => s.text);

test("plain, byte and raw strings are read, with their place", () => {
  const found = rustStrings(
    'let a = "one";\nlet b = b"two";\nlet c = r#"th"ree"#;\nlet d = br"four";\n',
  );
  assert.deepEqual(
    found.map((s) => [s.line, s.text, s.prefix]),
    [
      [1, "one", '"'],
      [2, "two", 'b"'],
      [3, 'th"ree', 'r#"'],
      [4, "four", 'br"'],
    ],
  );
  assert.equal(found[0].column, 9);
});

test("comments, characters and lifetimes hide no string and make none", () => {
  assert.deepEqual(
    texts(
      [
        '// "in a line comment"',
        '/* "in a block" /* nested "text" */ still */',
        "fn f<'a>(x: &'a str) -> char { '\"' }",
        "let q = '\\'';",
        'let s = "real";',
      ].join("\n"),
    ),
    ["real"],
  );
});

test("escapes are decoded; a backslash before a newline joins the lines", () => {
  assert.deepEqual(texts('let a = "say \\"hi\\"";\nlet b = "one \\\n      two";\n'), [
    'say "hi"',
    "one two",
  ]);
});

test("Unicode and ASCII escapes are cooked once, while raw strings keep their spelling", () => {
  const code = [
    String.raw`let a = "\u{41F}\u{440}\u{438}\u{432}\u{435}\u{442}";`,
    'let b = "\\u{1F_600}\\x41\\n\\t\\0";',
    String.raw`let c = "\\u{41F}";`,
    String.raw`let d = r"\u{41F}";`,
  ].join("\n");
  const found = rustStrings(code);
  assert.deepEqual(found.map((s) => s.text), [
    "Привет",
    "😀A\n\t\0",
    String.raw`\u{41F}`,
    String.raw`\u{41F}`,
  ]);
  assert.equal(found[1].line, 2);
  assert.equal(code.slice(found[0].start, found[0].end), String.raw`"\u{41F}\u{440}\u{438}\u{432}\u{435}\u{442}"`);
});

test("raw strings scan their complete delimiter, including the maximum valid hash count", () => {
  for (const count of [11, 32, 255]) {
    const hashes = "#".repeat(count);
    const body = 'prefix" ignored "# quote "suffix';
    for (const prefix of ["r", "br"]) {
      const literal = `${prefix}${hashes}"${body}"${hashes}`;
      const found = rustStrings(`let a = ${literal};\nlet b = "after";`);
      assert.deepEqual(found.map((s) => s.text), [body, "after"]);
      assert.equal(found[0].prefix, `${prefix}${hashes}"`);
      assert.equal(found[0].end - found[0].start, literal.length);
      assert.equal(found[1].line, 2);
    }
  }
});

test("a name that ends in r or b is not a string prefix", () => {
  assert.deepEqual(texts('let user = "x"; let sub = "y";'), ["x", "y"]);
});

test("items only a test build compiles are not read", () => {
  const code = [
    'const A: &str = "kept";',
    "#[cfg(test)]",
    "mod tests {",
    '    const B: &str = "in tests";',
    '    fn f() { let c = "nested { brace"; }',
    "}",
    'const D: &str = "after";',
    "#[cfg(all(test, unix))]",
    'fn g() { let e = "unix test"; }',
    "#[cfg(test)]",
    'use crate::x::{y, z}; const F: &str = "after use";',
    "#[cfg(not(test))]",
    'const G: &str = "production";',
    "#[cfg(any(test, windows))]",
    'const H: &str = "maybe";',
  ].join("\n");
  assert.deepEqual(texts(code), [
    "kept",
    "after",
    "after use",
    "production",
    "maybe",
  ]);
});

test("a test-only statement inside a function ends at its semicolon", () => {
  const code = [
    "fn run() {",
    "    #[cfg(all(test, unix))]",
    '    let a = ["x"; 2].len();',
    '    let b = "kept";',
    "}",
  ].join("\n");
  assert.deepEqual(texts(code), ["kept"]);
});

test("the text of an attribute that is not a test is read", () => {
  assert.deepEqual(
    texts('#[derive(Debug)]\n#[error("{}", crate::text::t("core.error.x"))]\nstruct E;'),
    ["{}", "core.error.x"],
  );
});

test("`#![cfg(test)]` makes the rest of the file test code", () => {
  assert.deepEqual(texts('#![cfg(test)]\nconst A: &str = "x";'), []);
});

test("comments in cfg attributes do not close brackets or become cfg predicates", () => {
  const code = [
    "#[cfg(/* ] cfg(not(test)) /* nested ] */ */ test)]",
    'fn hidden() { let a = "test-only block comment"; }',
    "#[cfg(// ] not(test)",
    "test)]",
    'fn also_hidden() { let a = "test-only line comment"; }',
    "#[cfg(not(/* test ] */ test))]",
    'fn production() { let a = "kept"; }',
  ].join("\n");
  assert.deepEqual(texts(code), ["kept"]);
});

test("nested test cfg items preserve the outer test-only boundary", () => {
  const code = [
    "#[cfg(test)]",
    "mod tests {",
    "    #[cfg(test)]",
    '    const INNER: &str = "inner constant";',
    '    const AFTER_CONSTANT: &str = "still in tests";',
    "    #[cfg(test)]",
    '    fn nested() { let a = "inner function"; }',
    '    fn after_nested() { let a = "still in outer tests"; }',
    "    mod inner {",
    "        #![cfg(test)]",
    '        const INNER: &str = "inner module";',
    "    }",
    '    const AFTER_MODULE: &str = "outer module";',
    "}",
    'const PRODUCTION: &str = "kept";',
  ].join("\n");
  assert.deepEqual(texts(code), ["kept"]);
});

test("a module inner test cfg skips only its containing scope, not later production", () => {
  const code = [
    "mod production {",
    '    const BEFORE: &str = "before";',
    "    mod tests {",
    "        #![cfg(test)]",
    '        const HIDDEN: &str = "test constant";',
    '        fn nested() { let a = "test function"; }',
    "    }",
    '    const AFTER: &str = "inside production";',
    "}",
    'const OUTSIDE: &str = "outside production";',
  ].join("\n");
  assert.deepEqual(texts(code), ["before", "inside production", "outside production"]);
});

test("which cfg only a test build satisfies", () => {
  for (const [body, expected] of [
    ["cfg(test)", true],
    ["cfg(all(test, unix))", true],
    ["cfg(all(unix, test))", true],
    ["cfg(all(unix, windows))", false],
    ["cfg(/* ] not(test) /* nested */ */ test)", true],
    ["cfg(// ] not(test)\ntest)", true],
    ["cfg(not(/* test */ test))", false],
    ["cfg(not(test))", false],
    ["cfg(any(test, windows))", false],
    ['cfg(feature = "x")', false],
    ["cfg_attr(test, derive(Debug))", false],
    ["derive(Debug)", false],
  ])
    assert.equal(onlyForTests(body), expected, body);
});
