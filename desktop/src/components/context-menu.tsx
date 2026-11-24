import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

export interface MenuItem {
  label: string;
  onSelect: () => void;
}

/**
 * A small menu at a point of the window, opened by a right click or from the keyboard. It is
 * taken inside the window, takes the focus, answers the arrows, `Home`/`End`, `Escape` and `Tab`
 * (which closes it), and closes on a click anywhere else, on scrolling and on losing the window (not on a change of the window's size: a screenshot of the whole page makes one).
 */
export function ContextMenu({
  label,
  x,
  y,
  items,
  onClose,
}: {
  /** What the menu is about; the name a screen reader reads. */
  label: string;
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}) {
  const menu = useRef<HTMLDivElement>(null);
  const [place, setPlace] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const element = menu.current;
    if (!element) return;
    const box = element.getBoundingClientRect();
    setPlace({
      left: Math.max(0, Math.min(x, window.innerWidth - box.width)),
      top: Math.max(0, Math.min(y, window.innerHeight - box.height)),
    });
    // Without scrolling: the menu is fixed, and a scroll of the page would close it at once.
    element
      .querySelector<HTMLElement>('[role="menuitem"]')
      ?.focus({ preventScroll: true });
  }, [x, y]);
  useEffect(() => {
    const outside = (event: Event) => {
      if (
        !(event.target instanceof Node) ||
        !menu.current?.contains(event.target)
      )
        onClose();
    };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("scroll", onClose, true);
    window.addEventListener("blur", onClose);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("scroll", onClose, true);
      window.removeEventListener("blur", onClose);
    };
  }, [onClose]);
  const move = (step: number | "first" | "last") => {
    const all = [
      ...(menu.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ??
        []),
    ];
    if (!all.length) return;
    const at = all.indexOf(document.activeElement as HTMLElement);
    const next =
      step === "first"
        ? 0
        : step === "last"
          ? all.length - 1
          : (at + step + all.length) % all.length;
    all[next].focus({ preventScroll: true });
  };
  return createPortal(
    <div
      ref={menu}
      role="menu"
      aria-label={label}
      className="context-menu"
      style={{ left: place.left, top: place.top }}
      onKeyDown={(event) => {
        switch (event.key) {
          case "Escape":
          case "Tab":
            event.preventDefault();
            event.stopPropagation();
            onClose();
            break;
          case "ArrowDown":
            event.preventDefault();
            move(1);
            break;
          case "ArrowUp":
            event.preventDefault();
            move(-1);
            break;
          case "Home":
            event.preventDefault();
            move("first");
            break;
          case "End":
            event.preventDefault();
            move("last");
            break;
        }
      }}
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          onClick={() => {
            onClose();
            item.onSelect();
          }}
        >
          {item.label}
        </button>
      ))}
    </div>,
    document.body,
  );
}
