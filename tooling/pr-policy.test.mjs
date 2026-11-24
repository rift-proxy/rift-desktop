import assert from "node:assert/strict";
import test from "node:test";
import {
  BUDGET_LABEL,
  BUDGET_PATH,
  COMMENT_MARKER,
  evaluatePullRequest,
  runPolicy,
} from "./pr-policy.mjs";

const clone = (value) => structuredClone(value);
const bot = { login: "github-actions[bot]", type: "Bot" };
const trustedRepository = { id: 123, full_name: "example/rift" };
const fileList = (length) => Array.from({ length }, (_, index) => ({
  filename: `src/file-${index}.rs`, additions: 0, deletions: 0,
}));
const notFound = () => Object.assign(Error("Not Found"), { status: 404 });

// Stateful service: tests inspect PR-visible outcomes, not mock forwarding.
function service({ additions = 1, deletions = 0, files = fileList(1), changedFiles = files.length, headRef = "feature", baseRef = "main", headRepository = trustedRepository } = {}) {
  const state = {
    pr: {
      number: 7, additions, deletions, changed_files: changedFiles,
      head: { sha: "head-1", ref: headRef, repo: clone(headRepository) },
      base: { sha: "base-1", ref: baseRef },
    },
    files: clone(files), comments: [], labels: new Set(), repositoryLabels: new Set(),
    writes: [], gets: 0, filePages: [], nextId: 1000,
  };
  const page = (items, options) => ({ data: clone(items.slice((options.page - 1) * options.per_page, options.page * options.per_page)) });
  const github = { rest: {
    pulls: {
      async get() {
        state.gets++;
        state.onGet?.(state);
        return { data: clone(state.pr) };
      },
      async listFiles(options) {
        state.filePages.push(options.page);
        state.onFiles?.(state, options.page);
        return page(state.files, options);
      },
    },
    issues: {
      async listComments(options) { return page(state.comments, options); },
      async listLabelsOnIssue(options) {
        return page([...state.labels].map((name) => ({ name })), options);
      },
      async getLabel({ name }) {
        if (!state.repositoryLabels.has(name)) throw notFound();
        return { data: { name } };
      },
      async createLabel({ name }) {
        state.onCreateLabel?.(state, name);
        if (state.repositoryLabels.has(name))
          throw Object.assign(Error("Already exists"), { status: 422 });
        state.repositoryLabels.add(name);
        state.writes.push("repository-label-created");
        return { data: { name } };
      },
      async addLabels({ labels }) {
        for (const name of labels) {
          if (!state.repositoryLabels.has(name)) throw notFound();
          state.labels.add(name);
        }
        state.writes.push("labels-added");
        return { data: [...state.labels].map((name) => ({ name })) };
      },
      async removeLabel({ name }) {
        if (!state.labels.delete(name)) throw notFound();
        state.writes.push("label-removed");
        return { data: [...state.labels].map((label) => ({ name: label })) };
      },
      async createComment({ body }) {
        const comment = { id: state.nextId++, user: clone(bot), body };
        state.comments.push(comment);
        state.writes.push("comment-created");
        state.onComment?.(state);
        return { data: clone(comment) };
      },
      async updateComment({ comment_id, body }) {
        const comment = state.comments.find((item) => item.id === comment_id);
        if (!comment) throw notFound();
        comment.body = body;
        state.writes.push("comment-updated");
        state.onComment?.(state);
        return { data: clone(comment) };
      },
    },
  } };
  const context = {
    repo: { owner: "example", repo: "rift" },
    payload: { repository: clone(trustedRepository), pull_request: clone(state.pr) },
  };
  return {
    state, github, context,
    run: () => runPolicy({ github, context }),
    nextRevision() {
      state.pr.head.sha += "-next";
      context.payload.pull_request = clone(state.pr);
    },
  };
}

for (const [additions, deletions] of [[1000, 0], [0, 1000], [500, 500]]) {
  test(`exact inclusive boundary ${additions}+${deletions} lines and 15 files produces no noise`, async () => {
    const fixture = service({ additions, deletions, files: fileList(15) });
    const result = await fixture.run();
    assert.equal(result.sizeWithinLimits, true);
    assert.equal(result.sizeExceeded, false);
    assert.equal(result.lines, 1000);
    assert.equal(result.changedFiles, 15);
    assert.equal(result.inspectionError, "");
    assert.deepEqual(fixture.state.writes, []);
  });
}

for (const [additions, deletions, files] of [[1001, 0, 15], [0, 1001, 15], [501, 500, 15], [0, 0, 16]]) {
  test(`oversized ${additions}+${deletions} lines/${files} files returns advice, not a policy failure`, async () => {
    const fixture = service({ additions, deletions, files: fileList(files) });
    const result = await fixture.run();
    assert.equal(result.sizeWithinLimits, false);
    assert.equal(result.sizeExceeded, true);
    assert.equal(result.inspectionError, "");
    assert.match(result.recommendation, /рекомендуется не более/);
    assert.equal(fixture.state.comments.length, 1);
    assert.match(fixture.state.comments[0].body, /Рекомендация/);
    assert.match(fixture.state.comments[0].body, /не блокирует слияние/);
    assert.doesNotMatch(fixture.state.comments[0].body, /Проверка не пройдена|запрещено/);
  });
}

test("binary files with zero lines count toward file size advice", async () => {
  const files = fileList(16).map((file, index) => ({ ...file, filename: `images/${index}.png` }));
  const fixture = service({ additions: 0, files });
  assert.equal((await fixture.run()).sizeExceeded, true);
  assert.match(fixture.state.comments[0].body, /16 файлов/);
});

test("repeated recommendations update one comment and resolve on a smaller revision", async () => {
  const fixture = service({ additions: 1001 });
  await fixture.run();
  const id = fixture.state.comments[0].id;
  const writes = fixture.state.writes.length;
  await fixture.run();
  assert.equal(fixture.state.writes.length, writes);
  fixture.state.pr.additions = 2000;
  fixture.nextRevision();
  assert.equal((await fixture.run()).sizeExceeded, true);
  assert.equal(fixture.state.comments.length, 1);
  assert.equal(fixture.state.comments[0].id, id);
  assert.match(fixture.state.comments[0].body, /2000/);
  fixture.state.pr.additions = 10;
  fixture.nextRevision();
  assert.equal((await fixture.run()).sizeExceeded, false);
  assert.equal(fixture.state.comments.length, 1);
  assert.equal(fixture.state.comments[0].id, id);
  assert.match(fixture.state.comments[0].body, /Рекомендации по размеру соблюдены/);
  assert.doesNotMatch(fixture.state.comments[0].body, /разделите изменения/);
});

test("same-repository dev to main release is exempt only from size advice", async () => {
  const fixture = service({ additions: 10000, files: fileList(20), headRef: "dev" });
  const result = await fixture.run();
  assert.equal(result.sizeWithinLimits, false);
  assert.equal(result.sizeExempt, true);
  assert.equal(result.sizeExceeded, false);
  assert.equal(result.recommendation, "");
  assert.equal(result.complete, true);
  assert.equal(result.inspectionError, "");
  assert.deepEqual(fixture.state.writes, []);
  assert.deepEqual(fixture.state.filePages, [1]);
});

for (const headRepository of [
  { id: 999, full_name: "fork/rift" },
  { id: 999, full_name: "example/rift" },
  { id: 123, full_name: "fork/rift" },
  { full_name: "example/rift" },
  null,
]) {
  test(`fork or incomplete repository identity cannot spoof a dev release exemption: ${JSON.stringify(headRepository)}`, async () => {
    const fixture = service({ additions: 1001, headRef: "dev", headRepository });
    const result = await fixture.run();
    assert.equal(result.sizeExempt, false);
    assert.equal(result.sizeExceeded, true);
    assert.match(fixture.state.comments[0].body, /не блокирует слияние/);
  });
}

test("same-repository dev to a non-main base is not release-exempt", async () => {
  const fixture = service({ additions: 1001, headRef: "dev", baseRef: "stable" });
  assert.equal((await fixture.run()).sizeExempt, false);
  assert.equal(fixture.state.comments.length, 1);
});

test("missing trusted repository ID never silently grants a release exemption", async () => {
  const fixture = service({ additions: 1001, headRef: "dev" });
  delete fixture.context.payload.repository.id;
  const result = await fixture.run();
  assert.equal(result.sizeExempt, false);
  assert.equal(result.sizeExceeded, true);
});

for (const file of [
  { filename: BUDGET_PATH, status: "modified" },
  { filename: BUDGET_PATH, status: "removed" },
  { filename: "tooling/perf/old-budget.json", previous_filename: BUDGET_PATH, status: "renamed" },
  { filename: BUDGET_PATH, previous_filename: "tooling/perf/old-budget.json", status: "renamed" },
]) {
  test(`budget ${file.status} ${file.filename} flags owner review without a size failure`, async () => {
    const fixture = service({ files: [file] });
    fixture.state.labels.add("keep-me");
    const result = await fixture.run();
    assert.equal(result.sizeExceeded, false);
    assert.equal(result.budgetTouched, true);
    assert.equal(result.inspectionError, "");
    assert.deepEqual([...fixture.state.labels], ["keep-me", BUDGET_LABEL]);
    assert.equal(fixture.state.repositoryLabels.has(BUDGET_LABEL), true);
    assert.match(fixture.state.comments[0].body, /проверки владельцем/);
    assert.match(fixture.state.comments[0].body, /Бот не повышает бюджеты и не одобряет PR/);
    assert.deepEqual(fixture.state.files, [file]);
  });
}

test("budget review still runs on an oversized exempt release and resolves on revert", async () => {
  const fixture = service({ additions: 10000, files: [{ filename: BUDGET_PATH }], headRef: "dev" });
  const result = await fixture.run();
  assert.equal(result.sizeExempt, true);
  assert.equal(result.sizeExceeded, false);
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
  assert.match(fixture.state.comments[0].body, /рекомендации по размеру не применяются/);
  assert.match(fixture.state.comments[0].body, /проверки владельцем/);
  fixture.state.files = fileList(1);
  fixture.nextRevision();
  await fixture.run();
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), false);
  assert.equal(fixture.state.comments.length, 1);
  assert.match(fixture.state.comments[0].body, /Изменений бюджета производительности.*нет/);
});

test("release retarget resolves an earlier size recommendation without skipping budget review", async () => {
  const fixture = service({ additions: 1001, files: [{ filename: BUDGET_PATH }] });
  await fixture.run();
  fixture.state.pr.head.ref = "dev";
  fixture.nextRevision();
  assert.equal((await fixture.run()).sizeExempt, true);
  assert.equal(fixture.state.comments.length, 1);
  assert.doesNotMatch(fixture.state.comments[0].body, /разделите изменения/);
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
});

test("only the exact standalone budget path is managed", async () => {
  const fixture = service({ files: [
    { filename: "project/tooling/perf/budgets.json" },
    { filename: "tooling/perf/budgets.json.backup" },
  ] });
  assert.equal((await fixture.run()).budgetTouched, false);
  assert.equal(fixture.state.comments.length, 0);
  assert.equal(fixture.state.labels.size, 0);
});

test("budget label/comment reconcile on repeat, revert and reintroduction while retaining other labels", async () => {
  const fixture = service({ files: [{ filename: BUDGET_PATH }] });
  fixture.state.labels.add("maintainer-label");
  fixture.state.repositoryLabels.add(BUDGET_LABEL);
  await fixture.run();
  const id = fixture.state.comments[0].id;
  const writes = fixture.state.writes.length;
  await fixture.run();
  assert.equal(fixture.state.writes.length, writes);
  fixture.state.files = fileList(1);
  fixture.nextRevision();
  await fixture.run();
  assert.deepEqual([...fixture.state.labels], ["maintainer-label"]);
  assert.match(fixture.state.comments[0].body, /Изменений бюджета производительности.*нет/);
  fixture.state.files = [{ filename: BUDGET_PATH }];
  fixture.nextRevision();
  await fixture.run();
  assert.deepEqual([...fixture.state.labels], ["maintainer-label", BUDGET_LABEL]);
  assert.equal(fixture.state.comments.length, 1);
  assert.equal(fixture.state.comments[0].id, id);
});

test("size advice returns after budget label and comment become visible", async () => {
  const fixture = service({ additions: 1001, files: [{ filename: BUDGET_PATH }] });
  const result = await fixture.run();
  assert.equal(result.sizeExceeded, true);
  assert.equal(result.inspectionError, "");
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
  assert.match(fixture.state.comments[0].body, /1001/);
  assert.match(fixture.state.comments[0].body, /проверки владельцем/);
});

test("human comments, other bots and quoted/spoofed markers stay untouched", async () => {
  const fixture = service({ additions: 1001 });
  fixture.state.comments = [
    { id: 1, user: { login: "contributor", type: "User" }, body: `${COMMENT_MARKER}\nHuman text` },
    { id: 2, user: { login: "other[bot]", type: "Bot" }, body: `${COMMENT_MARKER}\nOther bot text` },
    { id: 3, user: clone(bot), body: `Quoted marker: ${COMMENT_MARKER}\nNot managed` },
    { id: 4, user: { login: bot.login, type: "User" }, body: `${COMMENT_MARKER}\nWrong principal type` },
  ];
  const originals = clone(fixture.state.comments);
  await fixture.run();
  assert.deepEqual(fixture.state.comments.slice(0, 4), originals);
  assert.equal(fixture.state.comments.length, 5);
  assert.match(fixture.state.comments[4].body, /Рекомендация/);
});

test("files, comments and current labels are fully paginated", async () => {
  const files = fileList(201);
  files[200] = { filename: "moved-budget.json", previous_filename: BUDGET_PATH };
  const fixture = service({ files });
  fixture.state.comments = Array.from({ length: 100 }, (_, index) => ({
    id: index, user: { login: "contributor", type: "User" }, body: `Discussion ${index}`,
  }));
  fixture.state.comments.push({ id: 101, user: clone(bot), body: `${COMMENT_MARKER}\nOld recommendation` });
  for (let index = 0; index < 101; index++) fixture.state.labels.add(`other-${index}`);
  fixture.state.labels.add(BUDGET_LABEL);
  fixture.state.repositoryLabels.add(BUDGET_LABEL);
  const discussion = clone(fixture.state.comments.slice(0, 100));
  const originalLabels = [...fixture.state.labels];
  const result = await fixture.run();
  assert.equal(result.budgetTouched, true);
  assert.equal(result.sizeExceeded, true);
  assert.equal(result.inspectionError, "");
  assert.deepEqual(fixture.state.filePages, [1, 2, 3]);
  assert.equal(fixture.state.comments.length, 101);
  assert.deepEqual(fixture.state.comments.slice(0, 100), discussion);
  assert.match(fixture.state.comments[100].body, /201 файлов/);
  assert.deepEqual([...fixture.state.labels], originalLabels);
  fixture.state.files[200] = { filename: "not-budget.json" };
  fixture.nextRevision();
  await fixture.run();
  assert.deepEqual([...fixture.state.labels], originalLabels.filter((label) => label !== BUDGET_LABEL));
});

test("exactly 3000 files are complete, including the last budget entry", async () => {
  const files = fileList(3000);
  files[2999] = { filename: BUDGET_PATH };
  const fixture = service({ files, additions: 0 });
  const result = await fixture.run();
  assert.equal(result.complete, true);
  assert.equal(result.budgetTouched, true);
  assert.equal(result.inspectionError, "");
  assert.equal(fixture.state.filePages.length, 30);
  assert.equal(new Set(fixture.state.filePages).size, 30);
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
});

for (const headRef of ["feature", "dev"]) {
  for (const knownBudget of [false, true]) {
    test(`over 3000 files reports inspection error distinct from size advice: ${headRef}/${knownBudget}`, async () => {
      const files = fileList(3001);
      files[knownBudget ? 0 : 3000] = { filename: BUDGET_PATH };
      const fixture = service({ files, additions: 0, headRef });
      if (!knownBudget) {
        fixture.state.labels.add(BUDGET_LABEL);
        fixture.state.repositoryLabels.add(BUDGET_LABEL);
      }
      const result = await fixture.run();
      assert.equal(result.complete, false);
      assert.equal(result.sizeExempt, headRef === "dev");
      assert.equal(result.sizeExceeded, headRef !== "dev");
      assert.match(result.inspectionError, /полный список/);
      assert.equal(fixture.state.filePages.length, 30);
      assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
      assert.match(fixture.state.comments[0].body, /Ошибка получения метаданных/);
      assert.match(fixture.state.comments[0].body, /Отсутствие изменений бюджета не подтверждено/);
      assert.doesNotMatch(fixture.state.comments[0].body, /Изменений бюджета производительности.*нет/);
    });
  }
}

test("early file truncation is an inspection error even when size is within recommendations", async () => {
  const fixture = service({ files: fileList(1), changedFiles: 2 });
  fixture.state.labels.add(BUDGET_LABEL);
  const result = await fixture.run();
  assert.equal(result.complete, false);
  assert.equal(result.sizeExceeded, false);
  assert.match(result.inspectionError, /полный список/);
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
  assert.match(fixture.state.comments[0].body, /Ошибка получения метаданных/);
});

test("repeated file pages throw before mutations instead of hiding budget changes", async () => {
  const fixture = service({ files: fileList(200) });
  fixture.state.onFiles = (state, page) => {
    if (page === 2) state.files = [...fileList(100), ...fileList(100)];
  };
  await assert.rejects(fixture.run(), /повторную страницу/);
  assert.deepEqual(fixture.state.writes, []);
});

for (const change of [
  (state) => { state.pr.head.sha = "new-head"; },
  (state) => { state.pr.base.sha = "new-base"; },
  (state) => { state.pr.base.ref = "release"; },
  (state) => { state.pr.head.ref = "dev"; },
  (state) => { state.pr.head.repo.id = 999; },
]) {
  test("stale event revisions or exemption identity cannot mutate PR metadata", async () => {
    const fixture = service({ files: [{ filename: BUDGET_PATH }] });
    change(fixture.state);
    await assert.rejects(fixture.run(), /Устаревший запуск/);
    assert.deepEqual(fixture.state.writes, []);
  });
}

test("revision changing during file retrieval throws before any mutations", async () => {
  const fixture = service({ files: [{ filename: BUDGET_PATH }] });
  fixture.state.onFiles = (state) => { state.pr.head.sha = "new-head"; };
  await assert.rejects(fixture.run(), /Устаревший запуск/);
  assert.deepEqual(fixture.state.writes, []);
});

test("revision changing during writes never returns stale results", async () => {
  const fixture = service({ files: [{ filename: BUDGET_PATH }] });
  fixture.state.onComment = (state) => { state.pr.head.sha = "new-head"; };
  await assert.rejects(fixture.run(), /Устаревший запуск/);
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
  assert.equal(fixture.state.comments.length, 1);
});

test("fresh checks reject changing metadata totals", async () => {
  const fixture = service();
  fixture.state.onGet = (state) => {
    if (state.gets === 2) state.pr.additions = 1001;
  };
  await assert.rejects(fixture.run(), /Устаревший запуск/);
  assert.deepEqual(fixture.state.writes, []);
});

test("first-label creation race is accepted only after existence is confirmed", async () => {
  const fixture = service({ files: [{ filename: BUDGET_PATH }] });
  fixture.state.onCreateLabel = (state, name) => { state.repositoryLabels.add(name); };
  assert.equal((await fixture.run()).inspectionError, "");
  assert.equal(fixture.state.labels.has(BUDGET_LABEL), true);
  assert.equal(fixture.state.comments.length, 1);
});

test("an unrelated label creation error is not treated as successful", async () => {
  const fixture = service({ files: [{ filename: BUDGET_PATH }] });
  fixture.state.onCreateLabel = () => { throw Object.assign(Error("Invalid label"), { status: 422 }); };
  await assert.rejects(fixture.run(), /Not Found/);
  assert.equal(fixture.state.labels.size, 0);
  assert.equal(fixture.state.comments.length, 0);
});

test("actual API failures throw rather than being downgraded to size advice", async () => {
  const fixture = service();
  fixture.github.rest.pulls.listFiles = async () => { throw Error("API unavailable"); };
  await assert.rejects(fixture.run(), /API unavailable/);
  assert.deepEqual(fixture.state.writes, []);
});

test("pure API validates counters and duplicate files; release exemption requires trusted identity", () => {
  const metadata = {
    additions: 500, deletions: 500, changed_files: 1,
    head: { ref: "dev", repo: trustedRepository }, base: { ref: "main" },
  };
  assert.equal(evaluatePullRequest(metadata, fileList(1), trustedRepository).sizeExempt, true);
  assert.equal(evaluatePullRequest(metadata, fileList(1)).sizeExempt, false);
  for (const invalid of [-1, 0.5, undefined, NaN, Infinity, "1000"]) {
    assert.throws(() => evaluatePullRequest({ ...metadata, additions: invalid }, fileList(1)), /Некорректные метаданные/);
  }
  assert.throws(() => evaluatePullRequest(metadata, [{ filename: "a" }, { filename: "a" }]), /повторную страницу/);
  assert.throws(() => evaluatePullRequest(metadata, [{}]), /Список файлов PR/);
});
