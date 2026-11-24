export const MAX_LINES = 1000;
export const MAX_FILES = 15;
export const BUDGET_PATH = "tooling/perf/budgets.json";
export const BUDGET_LABEL = "budget-review";
export const COMMENT_MARKER = "<!-- rift-pr-policy -->";
const FILE_API_LIMIT = 3000;

function count(value, name) {
  if (!Number.isSafeInteger(value) || value < 0)
    throw Error(`Некорректные метаданные PR: ${name}.`);
  return value;
}

// Totals include additions AND deletions; binary files count toward size advice.
// Exemption requires both repository identifiers from trusted event context.
export function evaluatePullRequest(pullRequest, files, trustedRepository) {
  const additions = count(pullRequest.additions, "additions");
  const deletions = count(pullRequest.deletions, "deletions");
  const changedFiles = count(pullRequest.changed_files, "changed_files");
  const lines = count(additions + deletions, "additions + deletions");
  const names = new Set();
  for (const file of files) {
    if (typeof file.filename !== "string" || !file.filename || names.has(file.filename))
      throw Error("Список файлов PR некорректен или содержит повторную страницу.");
    names.add(file.filename);
  }
  const complete = changedFiles <= FILE_API_LIMIT && files.length === changedFiles;
  const budgetTouched = files.some(
    (file) => file.filename === BUDGET_PATH || file.previous_filename === BUDGET_PATH,
  );
  const sizeWithinLimits = lines <= MAX_LINES && changedFiles <= MAX_FILES;
  const headRepository = pullRequest.head?.repo;
  const sizeExempt = pullRequest.head?.ref === "dev" && pullRequest.base?.ref === "main" &&
    Number.isSafeInteger(trustedRepository?.id) && trustedRepository.id > 0 &&
    headRepository?.id === trustedRepository.id &&
    typeof trustedRepository.full_name === "string" &&
    headRepository?.full_name === trustedRepository.full_name;
  const sizeExceeded = !sizeWithinLimits && !sizeExempt;
  const recommendations = [];
  if (sizeExceeded && lines > MAX_LINES)
    recommendations.push(`Изменено ${lines} строк (добавлено ${additions}, удалено ${deletions}); рекомендуется не более ${MAX_LINES}.`);
  if (sizeExceeded && changedFiles > MAX_FILES)
    recommendations.push(`Изменено ${changedFiles} файлов; рекомендуется не более ${MAX_FILES} (включая бинарные файлы).`);
  return {
    additions,
    deletions,
    lines,
    changedFiles,
    complete,
    budgetTouched,
    sizeWithinLimits,
    sizeExempt,
    sizeExceeded,
    recommendation: recommendations.join("\n"),
    inspectionError: complete ? "" : "Не удалось получить полный список файлов PR: список неполон или превышен лимит GitHub API в 3000 файлов. Отсутствие изменений бюджета не подтверждено.",
  };
}

function revision(pullRequest) {
  const { head, base } = pullRequest;
  if (!head?.sha || !base?.sha || !base.ref)
    throw Error("Не удалось определить head/base PR.");
  return {
    head: head.sha,
    base: base.sha,
    branch: base.ref,
    headBranch: head.ref,
    headRepositoryId: head.repo?.id,
    headRepositoryName: head.repo?.full_name,
  };
}

function sameRevision(left, right) {
  return left.head === right.head && left.base === right.base && left.branch === right.branch &&
    left.headBranch === right.headBranch && left.headRepositoryId === right.headRepositoryId &&
    left.headRepositoryName === right.headRepositoryName;
}

async function listAll(fetchPage, key) {
  const items = [];
  const seen = new Set();
  for (let page = 1; ; page++) {
    const { data } = await fetchPage(page);
    if (!Array.isArray(data) || data.length > 100)
      throw Error("Некорректная страница GitHub API.");
    for (const item of data) {
      const id = key(item);
      if (id === undefined || seen.has(id))
        throw Error("GitHub API вернул повторную или некорректную страницу.");
      seen.add(id);
      items.push(item);
    }
    if (data.length < 100) return items;
  }
}
function commentBody(result) {
  const lines = [COMMENT_MARKER, "### Рекомендации по PR"];
  if (result.sizeExceeded) {
    lines.push("", "Рекомендация: по возможности разделите изменения на меньшие PR.", "", result.recommendation,
      "", "Это рекомендация, а не запрет на слияние: размер PR не блокирует слияние.");
  } else if (result.sizeExempt) {
    lines.push("", "Для релизного PR dev → main из этого же репозитория рекомендации по размеру не применяются. Проверка изменений бюджета сохраняется.");
  } else {
    lines.push("", `Рекомендации по размеру соблюдены: ${result.lines}/${MAX_LINES} строк (добавлено ${result.additions}, удалено ${result.deletions}), ${result.changedFiles}/${MAX_FILES} файлов.`);
  }
  if (result.inspectionError) lines.push("", `Ошибка получения метаданных: ${result.inspectionError}`);
  if (result.budgetTouched) {
    lines.push("", `Изменён \`${BUDGET_PATH}\` (включая удаление или переименование). Метка \`${BUDGET_LABEL}\` требует проверки владельцем: обоснуйте изменение бюджета. Бот не повышает бюджеты и не одобряет PR.`);
  } else if (result.complete) {
    lines.push("", "Изменений бюджета производительности в текущем diff нет.");
  } else {
    lines.push("", "Метка проверки бюджета не снимается без полного списка файлов.");
  }
  return lines.join("\n");
}

// Uses only Octokit REST metadata. Throws on API errors or stale event/revision.
// Size advice is returned after comment/label reconciliation; it never blocks.
export async function runPolicy({ github, context }) {
  const { owner, repo } = context.repo;
  const pull_number = context.payload.pull_request.number;
  const trustedRepository = {
    id: context.payload.repository?.id,
    full_name: `${owner}/${repo}`,
  };
  const repository = { owner, repo };
  const issue = { ...repository, issue_number: pull_number };
  const pull = { ...repository, pull_number };
  const expected = revision(context.payload.pull_request);
  const { data: initial } = await github.rest.pulls.get(pull);
  if (!sameRevision(expected, revision(initial)))
    throw Error("Устаревший запуск: head/base PR изменились. Нужен новый запуск проверки.");
  const files = [];
  const expectedFiles = count(initial.changed_files, "changed_files");
  for (let page = 1; files.length < Math.min(expectedFiles, FILE_API_LIMIT); page++) {
    const { data } = await github.rest.pulls.listFiles({ ...pull, per_page: 100, page });
    if (!Array.isArray(data) || data.length > 100)
      throw Error("Некорректный список файлов GitHub API.");
    const previousLength = files.length;
    files.push(...data);
    // Validate every page, so a server repeating page 1 cannot hide budget changes.
    evaluatePullRequest(initial, files, trustedRepository);
    if (data.length < 100 || files.length === previousLength) break;
  }
  const result = evaluatePullRequest(initial, files, trustedRepository);
  const [comments, labels] = await Promise.all([
    listAll(
      (page) => github.rest.issues.listComments({ ...issue, per_page: 100, page }),
      (comment) => comment.id,
    ),
    listAll(
      (page) => github.rest.issues.listLabelsOnIssue({ ...issue, per_page: 100, page }),
      (label) => label.name,
    ),
  ]);
  const managed = comments.find(
    (comment) => comment.user?.login === "github-actions[bot]" &&
      comment.user.type === "Bot" && comment.body?.startsWith(`${COMMENT_MARKER}\n`),
  );
  const assertCurrent = async () => {
    const { data: current } = await github.rest.pulls.get(pull);
    if (
      !sameRevision(expected, revision(current)) ||
      current.additions !== initial.additions ||
      current.deletions !== initial.deletions ||
      current.changed_files !== initial.changed_files
    ) throw Error("Устаревший запуск: head/base или метаданные PR изменились. Нужен новый запуск проверки.");
  };
  // No writes until the complete read phase has been checked against fresh SHAs.
  await assertCurrent();
  const hasLabel = labels.some((label) => label.name === BUDGET_LABEL);
  if (result.budgetTouched && !hasLabel) {
    try {
      await github.rest.issues.getLabel({ ...repository, name: BUDGET_LABEL });
    } catch (error) {
      if (error.status !== 404) throw error;
      await assertCurrent();
      try {
        await github.rest.issues.createLabel({
          ...repository,
          name: BUDGET_LABEL,
          color: "d4c5f9",
          description: "Performance budget changes need owner review",
        });
      } catch (creationError) {
        if (creationError.status !== 422) throw creationError;
        // Another PR may create the same label concurrently; confirm it exists.
        await github.rest.issues.getLabel({ ...repository, name: BUDGET_LABEL });
      }
    }
    await assertCurrent();
    await github.rest.issues.addLabels({ ...issue, labels: [BUDGET_LABEL] });
  } else if (!result.budgetTouched && result.complete && hasLabel) {
    await assertCurrent();
    await github.rest.issues.removeLabel({ ...issue, name: BUDGET_LABEL });
  }
  if (result.sizeExceeded || result.inspectionError || result.budgetTouched || managed) {
    const body = commentBody(result);
    if (managed) {
      if (managed.body !== body) {
        await assertCurrent();
        await github.rest.issues.updateComment({ ...repository, comment_id: managed.id, body });
      }
    } else {
      await assertCurrent();
      await github.rest.issues.createComment({ ...issue, body });
    }
  }
  // A revision changing during writes must not receive a successful result.
  await assertCurrent();
  return result;
}
