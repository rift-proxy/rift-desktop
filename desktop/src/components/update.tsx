import { useState } from "react";
import type { UpdateInfo } from "../backend/types";
import { Alert, Button, Dialog, Notification } from "./ui";
import { t } from "../i18n";

/** App-wide banner for a release the updater found. Install is disruptive
 * (drops the active connection and restarts the app), so it always asks first. */
export function UpdateBanner({
  info,
  onDismiss,
  onInstall,
}: {
  info: UpdateInfo;
  onDismiss: () => void;
  onInstall: () => Promise<void>;
}) {
  const [confirming, setConfirming] = useState(false);
  return (
    <>
      <Notification
        onClose={onDismiss}
        action={{
          label: t("update.install"),
          onClick: () => setConfirming(true),
        }}
      >
        {t("update.available", { version: info.version })}
      </Notification>
      {confirming && (
        <InstallDialog
          info={info}
          onClose={() => setConfirming(false)}
          onInstall={onInstall}
        />
      )}
    </>
  );
}

function InstallDialog({
  info,
  onClose,
  onInstall,
}: {
  info: UpdateInfo;
  onClose: () => void;
  onInstall: () => Promise<void>;
}) {
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Escape and the dialog's own close icon must not dismiss it mid-install, same as the
  // explicit Cancel button below — an invisible install is worse than a blocked one.
  // `Dialog` ignores both while `closeDisabled`.
  return (
    <Dialog
      title={t("update.install_title", { version: info.version })}
      onClose={onClose}
      closeDisabled={installing}
    >
      <p>{t("update.install_body")}</p>
      {info.notes && <p className="muted small">{info.notes}</p>}
      {error && <Alert>{error}</Alert>}
      <div className="dialog-actions">
        <Button onClick={onClose} disabled={installing}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="primary"
          loading={installing}
          onClick={() => {
            setInstalling(true);
            setError(null);
            onInstall().catch((e: unknown) => {
              setInstalling(false);
              setError(typeof e === "string" ? e : t("update.install_failed"));
            });
          }}
        >
          {t("update.install_and_restart")}
        </Button>
      </div>
    </Dialog>
  );
}
