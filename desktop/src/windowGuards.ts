/** Field types that take typed text: only there the browser menu (copy, paste) stays useful. */
const TEXT_INPUTS = new Set([
  "text",
  "search",
  "url",
  "email",
  "tel",
  "password",
  "number",
]);
/** `Ctrl+S` save page, `Ctrl+P` print and save as PDF, `Ctrl+U` page source. */
const PAGE_KEYS = new Set(["KeyS", "KeyP", "KeyU"]);
/** `Ctrl+Shift+I/J/C` developer tools, `Ctrl+Shift+P/S` system print and web capture. */
const SHIFTED_PAGE_KEYS = new Set(["KeyI", "KeyJ", "KeyC", "KeyP", "KeyS"]);

/** A field the user types into, where the menu with copy and paste is allowed. */
export function isEditable(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  const field = target.closest("input, textarea, [contenteditable]");
  if (field instanceof HTMLTextAreaElement) return true;
  if (field instanceof HTMLInputElement) return TEXT_INPUTS.has(field.type);
  return field !== null && field.getAttribute("contenteditable") !== "false";
}

/**
 * A key that would open the browser's own tools or save the page instead of Rift. The physical
 * key is read (`code`), not the letter, so the Russian layout (`Ctrl+Ы`) is closed as well as
 * `Ctrl+S`. `AltGr` (reported as `Ctrl+Alt`) types characters and is left alone.
 */
export function isBrowserKey(event: KeyboardEvent): boolean {
  if (event.code === "F12") return true;
  if (!(event.ctrlKey || event.metaKey) || event.altKey) return false;
  return (event.shiftKey ? SHIFTED_PAGE_KEYS : PAGE_KEYS).has(event.code);
}

/**
 * Rift is an application, not a page: no WebView menu («Сохранить как…», «Печать», «Просмотреть
 * код») and no shortcut that saves or inspects the page. Fields keep the menu for copy and
 * paste. The returned function removes both listeners.
 */
export function installWindowGuards(target: Document = document): () => void {
  const contextMenu = (event: MouseEvent) => {
    if (!isEditable(event.target)) event.preventDefault();
  };
  const keyDown = (event: KeyboardEvent) => {
    if (isBrowserKey(event)) event.preventDefault();
  };
  // Capture: no component can stop the event before the guard sees it.
  target.addEventListener("contextmenu", contextMenu, true);
  target.addEventListener("keydown", keyDown, true);
  return () => {
    target.removeEventListener("contextmenu", contextMenu, true);
    target.removeEventListener("keydown", keyDown, true);
  };
}
