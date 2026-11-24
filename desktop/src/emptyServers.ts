import { t } from "./i18n";
import { failureText } from "./format";
import type { Snapshot } from "./backend/types";

/** What the user can do about an empty list of servers. */
export type EmptyAction = {
  label: string;
  kind: "refresh" | "refresh_all" | "edit" | "add_server" | "add_subscription";
  /** The subscription the action is about. */
  id?: string;
};
export interface EmptyServers {
  title: string;
  description: string;
  action: EmptyAction | null;
}

/**
 * What an empty list of servers says. A subscription that did not give its servers is not a list
 * waiting for a server added by hand: it says why, and offers to ask again.
 * `source`: `all`, `manual` or the id of a subscription.
 */
export function emptyServers(
  state: Pick<Snapshot, "servers" | "subscriptions" | "refreshing">,
  source: string,
): EmptyServers {
  const subscription = state.subscriptions.find((s) => s.id === source);
  if (subscription) {
    const reason =
      failureText(subscription.failure_code, t("empty_servers.fetch_failed")) ??
      subscription.error ??
      "";
    if (state.refreshing)
      return {
        title: t("empty_servers.loading_title"),
        description: "",
        action: null,
      };
    const refresh: EmptyAction = {
      label: t("empty_servers.refresh"),
      kind: "refresh",
      id: subscription.id,
    };
    if (subscription.failure_code === "subscription_filtered")
      return {
        title: t("empty_servers.filtered_title"),
        description: t("empty_servers.filtered_description"),
        action: {
          label: t("empty_servers.edit_subscription"),
          kind: "edit",
          id: subscription.id,
        },
      };
    if (subscription.failure_code === "subscription_invalid_body")
      return {
        title: t("empty_servers.subscription_empty_title"),
        description: reason,
        action: refresh,
      };
    if (subscription.failure_code || subscription.error)
      return {
        title: t("empty_servers.subscription_failed_title", {
          name: subscription.name,
        }),
        description: reason,
        action: refresh,
      };
    return {
      title:
        subscription.updated_at === null
          ? t("empty_servers.not_loaded_title")
          : t("empty_servers.subscription_empty_title"),
      description: "",
      action: refresh,
    };
  }
  if (source === "manual")
    return state.servers.length
      ? {
          title: t("empty_servers.source_empty_title"),
          description: t("empty_servers.source_empty_description"),
          action: { label: t("empty_servers.add_server"), kind: "add_server" },
        }
      : {
          title: t("empty_servers.none_title"),
          description: t("empty_servers.add_manually"),
          action: { label: t("empty_servers.add_server"), kind: "add_server" },
        };
  if (state.servers.length)
    return {
      title: t("empty_servers.source_empty_title"),
      description: t("empty_servers.source_empty_description"),
      action: { label: t("empty_servers.add_server"), kind: "add_server" },
    };
  if (state.subscriptions.length) {
    const failed = state.subscriptions.find((s) => s.failure_code || s.error);
    return {
      title: t("empty_servers.subscriptions_failed_title"),
      description:
        (failed &&
          (failureText(failed.failure_code, t("empty_servers.fetch_failed")) ??
            failed.error)) ||
        t("empty_servers.refresh_subscriptions"),
      action: { label: t("empty_servers.refresh_all"), kind: "refresh_all" },
    };
  }
  return {
    title: t("empty_servers.none_title"),
    description: t("empty_servers.add_link_or_manually"),
    action: {
      label: t("empty_servers.add_subscription"),
      kind: "add_subscription",
    },
  };
}
