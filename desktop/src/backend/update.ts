import { useCallback, useEffect, useRef, useState } from "react";
import { useBackend } from "./provider";
import type { UpdateInfo } from "./types";
import { t } from "../i18n";

/** A separate small channel, like errors: update state is orthogonal to core `Snapshot`. */
export function useUpdateInfo() {
  const api = useBackend();
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const events = useRef(0);
  useEffect(() => {
    if (!api.onUpdate) return;
    let active = true;
    let dispose: (() => void) | undefined;
    void api
      .onUpdate((pushed) => {
        events.current += 1;
        if (active) setInfo(pushed);
      })
      .then((fn) => {
        if (active) dispose = fn;
        else fn();
      })
      .catch(() => {
        // Push notifications are a convenience; the manual check still works without them.
      });
    return () => {
      active = false;
      dispose?.();
    };
  }, [api]);
  const check = useCallback(async () => {
    if (!api.checkForUpdate) return null;
    const before = events.current;
    const result = await api.checkForUpdate();
    // A pushed event may have arrived while this manual check was in flight.
    if (events.current === before) setInfo(result);
    return result;
  }, [api]);
  const dismiss = useCallback(async () => {
    if (!info || !api.dismissUpdate) return;
    const version = info.version;
    setInfo(null);
    await api.dismissUpdate(version);
  }, [api, info]);
  const install = useCallback(() => {
    if (!api.installUpdate) return Promise.reject(t("update.unavailable"));
    return api.installUpdate();
  }, [api]);
  return { info, check, dismiss, install };
}
export type UpdateController = ReturnType<typeof useUpdateInfo>;
