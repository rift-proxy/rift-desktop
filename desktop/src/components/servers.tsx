import { useCallback, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Star,
  RefreshCw,
  Pencil,
  Trash2,
} from "lucide-react";
import type {
  Server,
  ServersSort,
  SortColumn,
  Subscription,
} from "../backend/types";
import { Button, StatusChip, IconButton, Badge } from "./ui";
import { ContextMenu } from "./context-menu";
import {
  availabilityText,
  checkedAt,
  dateText,
  failureText,
  latency,
  retryText,
  time,
  usageText,
} from "../format";
import { t } from "../i18n";
/** Pointer travel (px) between press and release above which a click is a drag. */
const DRAG_PX = 5;
export function SubscriptionItem({
  item,
  selected,
  refreshing,
  onSelect,
  onRefresh,
  onEdit,
  onDelete,
}: {
  item: Subscription;
  selected: boolean;
  refreshing: boolean;
  onSelect: () => void;
  onRefresh: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const failure =
    failureText(item.failure_code, t("subscription.refresh_failed")) ??
    item.error;
  const usage = item.usage ? usageText(item.usage) : null;
  const term = item.usage?.expire
    ? item.availability === "expired"
      ? t("subscription.expired", { date: dateText(item.usage.expire) })
      : t("subscription.valid_until", { date: dateText(item.usage.expire) })
    : null;
  const skipped = availabilityText(item.availability);
  return (
    <div className={`subscription ${selected ? "selected" : ""}`}>
      <button className="subscription-title" onClick={onSelect}>
        {item.name} <span className="source-count">{item.count}</span>
      </button>
      {(usage || term) && (
        <small className="subscription-meta">
          {[usage, term].filter(Boolean).join(" · ")}
        </small>
      )}
      {skipped && <small className="subscription-skipped">{skipped}</small>}
      {failure && <small>{failure}</small>}
      <div className="subscription-actions">
        <IconButton
          label={t("subscription.edit", { name: item.name })}
          onClick={onEdit}
        >
          <Pencil size={14} />
        </IconButton>
        <IconButton
          label={t("subscription.delete", { name: item.name })}
          onClick={onDelete}
        >
          <Trash2 size={14} />
        </IconButton>
        <IconButton
          label={t("subscription.refresh", { name: item.name })}
          disabled={refreshing}
          onClick={onRefresh}
        >
          <RefreshCw size={14} />
        </IconButton>
        <span className="subscription-updated">
          {item.updated_at === null
            ? t("subscription.never_updated")
            : time(item.updated_at)}
        </span>
      </div>
    </div>
  );
}
/** Where a menu of a row opens, and whether that is a guess (a key sends a click at no place). */
interface MenuPoint {
  x: number;
  y: number;
  guessed: boolean;
}
export function ServerRow({
  server,
  selected,
  source,
  onSelect,
  onDelete,
  onMenu,
  detailed = false,
}: {
  server: Server;
  selected: boolean;
  source: string;
  onSelect: () => void;
  onDelete?: () => void;
  /** The menu of the row is asked for: by a right click, or by the menu key from the name. */
  onMenu?: (point: MenuPoint, opener: HTMLElement | null) => void;
  detailed?: boolean;
}) {
  // Text cannot be selected, so a drag across the row is the only "accident" left: a pointer
  // that travelled this far between press and release did not mean to click.
  const pressed = useRef<{ x: number; y: number } | null>(null);
  const name = useRef<HTMLButtonElement>(null);
  /** Under the name of the row: where a menu asked for from the keyboard opens. */
  const underName = (): MenuPoint => {
    const box = name.current?.getBoundingClientRect();
    return { x: box?.left ?? 0, y: box?.bottom ?? 0, guessed: true };
  };
  return (
    <tr
      className={`server-row ${selected ? "selected" : ""} ${server.excluded ? "excluded" : ""}`}
      onContextMenu={(event) => {
        if (!onMenu) return;
        event.preventDefault();
        onMenu(
          event.clientX === 0 && event.clientY === 0
            ? underName()
            : { x: event.clientX, y: event.clientY, guessed: false },
          name.current,
        );
      }}
      onKeyDown={(event) => {
        if (!onMenu) return;
        if (
          event.key === "ContextMenu" ||
          (event.shiftKey && event.key === "F10")
        ) {
          event.preventDefault();
          onMenu(underName(), name.current);
        }
      }}
      onPointerDownCapture={(event) => {
        pressed.current = { x: event.clientX, y: event.clientY };
      }}
      onClickCapture={(event) => {
        const start = pressed.current;
        pressed.current = null;
        if (
          start &&
          event.detail > 0 &&
          Math.hypot(event.clientX - start.x, event.clientY - start.y) > DRAG_PX
        ) {
          event.stopPropagation();
          event.preventDefault();
        }
      }}
      onClick={(event) => {
        if (
          event.defaultPrevented ||
          (event.target instanceof Element &&
            event.target.closest("button, a, input, select, textarea, summary"))
        )
          return;
        onSelect();
      }}
    >
      <td>
        <Button
          ref={name}
          variant="ghost"
          className="server-name"
          onClick={onSelect}
        >
          {selected && <Star size={13} fill="currentColor" />}
          {server.name}
        </Button>
        {server.excluded && <Badge>{t("server.excluded")}</Badge>}
      </td>
      {detailed && (
        <td>
          <Badge>{server.protocol}</Badge>
        </td>
      )}
      {detailed && <td>{latency(server.endpoint_ms)}</td>}
      <td>{latency(server.proxy_ms)}</td>
      <td className={server.failure_code ? "status-reason" : undefined}>
        <StatusChip
          status={server.status}
          checkedAt={server.checked_at}
          label={
            failureText(server.failure_code, t("server.check_failed")) ??
            undefined
          }
        />
        {server.retry_at != null && (
          <div className="status-retry">{retryText(server.retry_at)}</div>
        )}
      </td>
      {detailed && <td>{source}</td>}
      {detailed && <td>{checkedAt(server.checked_at)}</td>}
      <td>
        {onDelete && (
          <IconButton
            label={t("server.delete", { name: server.name })}
            onClick={onDelete}
          >
            <Trash2 size={15} />
          </IconButton>
        )}
      </td>
    </tr>
  );
}
/** A column heading that orders the list by its column, and says which way it is ordered. */
function SortHeader({
  column,
  sort,
  onSort,
  children,
}: {
  column: SortColumn;
  sort: ServersSort;
  onSort: (column: SortColumn) => void;
  children: string;
}) {
  const active = sort.column === column;
  return (
    <th
      aria-sort={
        active ? (sort.descending ? "descending" : "ascending") : undefined
      }
    >
      <button type="button" className="sort" onClick={() => onSort(column)}>
        {children}
        {active &&
          (sort.descending ? (
            <ArrowDown className="sort-arrow" size={12} aria-hidden="true" />
          ) : (
            <ArrowUp className="sort-arrow" size={12} aria-hidden="true" />
          ))}
      </button>
    </th>
  );
}
/** The columns only the detailed list has, by the name of their heading. */
const HIDDEN_COLUMNS: Partial<Record<SortColumn, string>> = {
  protocol: "server_table.protocol",
  endpoint_ms: "server_table.endpoint",
  source: "server_table.source",
  checked_at: "server_table.checked",
};
export function ServerTable({
  servers,
  selected,
  sources,
  sort,
  onSort,
  onSelect,
  onDelete,
  onExclude,
  detailed = false,
}: {
  servers: Server[];
  selected: string | null;
  sources: Subscription[];
  sort: ServersSort;
  onSort: (column: SortColumn) => void;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
  /** Keep a server out of Auto Select, or let it back in; without it the rows have no menu. */
  onExclude?: (id: string, excluded: boolean) => void;
  detailed?: boolean;
}) {
  // One menu at a time: the one of the row that asked last.
  const [menu, setMenu] = useState<{
    server: Server;
    point: MenuPoint;
    opener: HTMLElement | null;
  } | null>(null);
  const closeMenu = useCallback(() => {
    setMenu((open) => {
      open?.opener?.focus();
      return null;
    });
  }, []);
  const heading = (column: SortColumn, label: string) => (
    <SortHeader column={column} sort={sort} onSort={onSort}>
      {label}
    </SortHeader>
  );
  // An order by a column the compact list does not show is said in words, or nothing would say it.
  const hidden = detailed ? undefined : HIDDEN_COLUMNS[sort.column];
  return (
    <div className="table-scroll">
      <table>
        {hidden && (
          <caption className="sort-note">
            {t("server_table.order_note", {
              column: t(hidden),
              direction: t(
                sort.descending
                  ? "server_table.descending"
                  : "server_table.ascending",
              ),
            })}
          </caption>
        )}
        <thead>
          <tr>
            {heading("name", t("server_table.name"))}
            {detailed && heading("protocol", t("server_table.protocol"))}
            {detailed && heading("endpoint_ms", t("server_table.endpoint"))}
            {heading(
              "proxy_ms",
              t(detailed ? "server_table.via_server" : "server_table.latency"),
            )}
            {heading("status", t("server_table.status"))}
            {detailed && heading("source", t("server_table.source"))}
            {detailed && heading("checked_at", t("server_table.checked"))}
            <th>
              <span className="sr-only">{t("server_table.actions")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {servers.map((s) => (
            <ServerRow
              key={`${s.source}:${s.id}`}
              server={s}
              selected={s.id === selected}
              detailed={detailed}
              source={
                sources.find((sub) => sub.id === s.source)?.name ??
                t("servers.manual")
              }
              onSelect={() => onSelect(s.id)}
              onDelete={s.source === null ? () => onDelete(s.id) : undefined}
              onMenu={
                onExclude
                  ? (point, opener) =>
                      // A key that the platform also follows with a click at no place must not open
                      // the menu a second time, nor move it to the corner.
                      setMenu((open) =>
                        open?.server.id === s.id &&
                        open.point.guessed &&
                        point.guessed
                          ? open
                          : { server: s, point, opener },
                      )
                  : undefined
              }
            />
          ))}
        </tbody>
      </table>
      {menu && onExclude && (
        <ContextMenu
          label={menu.server.name}
          x={menu.point.x}
          y={menu.point.y}
          onClose={closeMenu}
          items={[
            {
              label: menu.server.excluded
                ? t("server_table.include")
                : t("server_table.exclude"),
              onSelect: () => onExclude(menu.server.id, !menu.server.excluded),
            },
          ]}
        />
      )}
    </div>
  );
}
