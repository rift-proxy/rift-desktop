import { useCallback, useEffect, useRef, useState } from "react";
import type { Action, Settings } from "./backend/types";
import { settingsProblems } from "./viewmodel";

/** How long typing may pause before a changed field is saved. Lists and switches save at once. */
export const SETTINGS_SAVE_DELAY_MS = 600;

/**
 * The settings form: every edit saves itself through the ordinary `save_settings` action.
 *
 * A draft that fails `settingsProblems` is never sent and stays on screen with its reason.
 * While an edit is waiting or being saved, backend snapshots do not overwrite the draft: the
 * answer to an earlier keystroke would otherwise erase the next one. A rejected save puts the
 * form back to the saved values; the error itself is shown by the usual error path.
 */
export function useSettingsDraft(
  settings: Settings,
  execute: (action: Action) => Promise<boolean>,
) {
  const [draft, setDraft] = useState(settings);
  const [dirty, setDirty] = useState(false);
  const incoming = JSON.stringify(settings);
  const [seen, setSeen] = useState(incoming);
  const editedKeys = useRef(new Set<keyof Settings>());
  if (incoming !== seen) {
    // Always adopt new settings snapshot, even if dirty (e.g. profile import).
    setSeen(incoming);
    setDraft(settings);
    if (dirty) {
      setDirty(false);
      editedKeys.current.clear();
    }
  }
  const latest = useRef(draft);
  const saved = useRef(settings);
  const run = useRef(execute);
  const version = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => {
    latest.current = draft;
    saved.current = settings;
    run.current = execute;
  });
  const commit = useCallback(() => {
    clearTimeout(timer.current);
    timer.current = undefined;
    const next = latest.current;
    if (Object.keys(settingsProblems(next)).length > 0) return;
    const sent = version.current;
    const settled = (ok: boolean) => {
      if (version.current !== sent) return;
      if (!ok) setDraft(saved.current);
      else editedKeys.current.clear();
      setDirty(false);
    };
    // Rebase edited fields onto latest backend settings to avoid stale overwrite.
    const rebased: Settings = { ...saved.current };
    for (const key of editedKeys.current) {
      (rebased[key] as Settings[typeof key]) = next[key];
    }
    if (JSON.stringify(rebased) === JSON.stringify(saved.current)) {
      settled(true);
      return;
    }
    void run
      .current({ type: "save_settings", settings: rebased })
      .then(settled);
  }, []);
  const edit = <K extends keyof Settings>(
    key: K,
    value: Settings[K],
    now: boolean,
  ) => {
    latest.current = { ...latest.current, [key]: value };
    version.current += 1;
    editedKeys.current.add(key);
    setDraft(latest.current);
    setDirty(true);
    clearTimeout(timer.current);
    if (now) commit();
    else timer.current = setTimeout(commit, SETTINGS_SAVE_DELAY_MS);
  };
  // Leaving the page must not lose a value that is still waiting for its pause.
  useEffect(
    () => () => {
      if (timer.current !== undefined) commit();
    },
    [commit],
  );
  return {
    draft,
    problems: settingsProblems(draft),
    /** A list or a switch: saved at once. */
    choose: <K extends keyof Settings>(key: K, value: Settings[K]) => {
      // List/switch edits send the last SAVED settings with only this key changed,
      // independent of any invalid typed fields in the local draft.
      const immediate: Settings = { ...saved.current, [key]: value };
      if (JSON.stringify(immediate) === JSON.stringify(saved.current)) return;
      version.current += 1;
      const sent = version.current;
      latest.current = { ...latest.current, [key]: value };
      setDraft(latest.current);
      void run
        .current({ type: "save_settings", settings: immediate })
        .then((ok) => {
          if (version.current !== sent) return;
          if (!ok) setDraft(saved.current);
        });
    },
    /** A typed field: saved after the pause, on Enter or when it loses focus. */
    type: <K extends keyof Settings>(key: K, value: Settings[K]) =>
      edit(key, value, false),
    flush: () => {
      if (timer.current !== undefined) commit();
    },
  };
}
