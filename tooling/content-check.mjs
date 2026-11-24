#!/usr/bin/env node
// A narrow gate for resolvable links and the generated Rust/Storybook contract, not prose meaning.
import { isDeepStrictEqual } from "node:util";
import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(import.meta.dirname, "..");
const generatedDirectories = new Set([
  ".git", ".tools", "node_modules", "target", "dist", "coverage", "storybook-static",
  "playwright-report", "test-results", "reports", "mutants.out", "mutants.out.old",
  "corpus", "artifacts", "tmp",
]);

function markdownFiles(root) {
  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory() && !generatedDirectories.has(entry.name)) visit(file);
      else if (entry.isFile() && /\.md$/i.test(entry.name)) files.push(file);
    }
  }
  visit(root);
  return files.sort();
}

// Preserve line numbers while ignoring fenced examples and HTML comments.
function sourceLines(text) {
  let fence = null;
  return text.replace(/<!--[\s\S]*?-->/g, (comment) => comment.replace(/[^\n]/g, " "))
    .split(/\r?\n/).map((line) => {
      const marker = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
      if (fence) {
        if (marker && marker[1][0] === fence[0] && marker[1].length >= fence.length && !marker[2].trim()) fence = null;
        return "";
      }
      if (marker) {
        fence = marker[1];
        return "";
      }
      return line;
    });
}

const labelKey = (label) => label.replace(/\\([\[\]\\])/g, "$1").trim().replace(/\s+/g, " ").toLowerCase();
const unescape = (text) => text.replace(/\\([!"#$%&'()*+,\-./:;<=>?@[\]\\^_`{|}~])/g, "$1").replace(/&amp;/g, "&");

function destination(text, start, inline = false) {
  let index = start;
  while (/\s/.test(text[index] ?? "") && index < text.length) index++;
  if (text[index] === "<") {
    const end = text.indexOf(">", index + 1);
    return end < 0 ? null : unescape(text.slice(index + 1, end));
  }
  const begin = index;
  let depth = 0;
  for (; index < text.length; index++) {
    if (text[index] === "\\") { index++; continue; }
    if (text[index] === "(") depth++;
    else if (text[index] === ")") {
      if (depth === 0 && inline) break;
      depth--;
    }
    if (/\s/.test(text[index]) && depth === 0) break;
  }
  return unescape(text.slice(begin, index));
}

function parseMarkdown(text) {
  const lines = sourceLines(text);
  const anchors = new Set();
  const slugCounts = new Map();
  const definitions = new Map();
  const links = [];
  function heading(text) {
    const slug = text.replace(/<[^>]*>/g, "").replace(/!?\[([^\]]+)\]\([^)]*\)/g, "$1")
      .replace(/&amp;/g, "").toLowerCase().replace(/[^\p{L}\p{N}\p{M}_\- ]/gu, "").replace(/ /g, "-");
    let count = slugCounts.get(slug) ?? 0;
    let anchor = count ? `${slug}-${count}` : slug;
    while (anchors.has(anchor)) anchor = `${slug}-${++count}`;
    slugCounts.set(slug, count + 1);
    anchors.add(anchor);
  }
  for (const [index, line] of lines.entries()) {
    const definition = /^ {0,3}\[([^\]]+)\]:\s*(.*)$/.exec(line);
    if (definition) definitions.set(labelKey(definition[1]), destination(definition[2], 0));
    const atx = /^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$/.exec(line);
    if (atx) heading(atx[1]);
    else if (index > 0 && /^ {0,3}(?:=+|-+)\s*$/.test(line) && lines[index - 1].trim()) heading(lines[index - 1].trim());
    const html = line.replace(/(`+)[\s\S]*?\1/g, "");
    for (const tag of html.matchAll(/<[A-Za-z][^>]*>/g))
      for (const match of tag[0].matchAll(/\b(?:id|name)\s*=\s*["']([^"']+)["']/g))
        anchors.add(match[1]);
  }
  for (const [index, original] of lines.entries()) {
    if (/^ {0,3}\[[^\]]+\]:/.test(original)) continue;
    const line = original.replace(/(`+)[\s\S]*?\1/g, "");
    for (const match of line.matchAll(/!?\[((?:\\.|[^\]\\])*)\](?:\[([^\]]*)\])?/g)) {
      const end = match.index + match[0].length;
      let target;
      if (line[end] === "(") target = destination(line, end + 1, true);
      else if (match[2] !== undefined) {
        const key = labelKey(match[2] || match[1]);
        target = definitions.get(key);
        if (target === undefined) links.push({ line: index + 1, error: `Undefined link reference: ${key}` });
      } else target = definitions.get(labelKey(match[1]));
      if (target !== undefined && target !== null) links.push({ line: index + 1, target });
    }
    for (const match of line.matchAll(/\b(?:href|src)\s*=\s*["']([^"']+)["']/g)) links.push({ line: index + 1, target: match[1] });
  }
  return { anchors, links };
}

export function checkContent(root = projectRoot, { referencePath } = {}) {
  root = path.resolve(root);
  const files = markdownFiles(root);
  const documents = new Map(files.map((file) => [file, parseMarkdown(readFileSync(file, "utf8"))]));
  const failures = [];
  let internalLinks = 0;
  for (const [file, document] of documents) {
    for (const link of document.links) {
      const location = `${path.relative(root, file)}:${link.line}`;
      if (link.error) { failures.push(`${location}: ${link.error}`); continue; }
      if (!link.target || /^(?:[a-z][a-z\d+.-]*:|\/\/)/i.test(link.target)) continue;
      internalLinks++;
      try {
        const [pathname, fragment] = link.target.split("#", 2);
        const decoded = decodeURIComponent(pathname.split("?", 1)[0]);
        const target = decoded.startsWith("/") ? path.resolve(root, `.${decoded}`) : path.resolve(path.dirname(file), decoded || path.basename(file));
        const relative = path.relative(root, target);
        if (relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) throw new Error("Link escapes project root");
        if (!existsSync(target)) throw new Error("Missing internal target");
        if (fragment) {
          if (!statSync(target).isFile() || !/\.md$/i.test(target)) throw new Error("Cannot resolve anchor in non-Markdown target");
          const targetDocument = documents.get(target) ?? parseMarkdown(readFileSync(target, "utf8"));
          if (!targetDocument.anchors.has(decodeURIComponent(fragment))) throw new Error("Missing internal anchor");
        }
      } catch (error) { failures.push(`${location}: ${link.target}: ${error.message}`); }
    }
  }
  if (referencePath) {
    try {
      const fixture = JSON.parse(readFileSync(path.join(root, "desktop/src/testing/core-contract.json"), "utf8"));
      const current = JSON.parse(readFileSync(referencePath, "utf8"));
      if (!isDeepStrictEqual(fixture, current)) failures.push("Rust story contract differs from desktop/src/testing/core-contract.json; regenerate the fixture and review mock parity");
    } catch (error) { failures.push(`Rust story contract: ${error.message}`); }
  }
  return { markdownFiles: files.length, internalLinks, rustReferenceChecked: Boolean(referencePath), failures };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    const options = {};
    for (let index = 0; index < args.length; index += 2) {
      if (!["--root", "--reference", "--report"].includes(args[index]) || !args[index + 1]) throw new Error("Usage: content-check.mjs [--root project-root] [--reference generated.json] [--report report.json]");
      options[args[index].slice(2)] = args[index + 1];
    }
    const report = checkContent(options.root ?? projectRoot, { referencePath: options.reference });
    if (options.report) {
      mkdirSync(path.dirname(options.report), { recursive: true });
      writeFileSync(options.report, `${JSON.stringify(report, null, 2)}\n`);
    }
    console.log(JSON.stringify(report, null, 2));
    if (report.failures.length) process.exitCode = 1;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
