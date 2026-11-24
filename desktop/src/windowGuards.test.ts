import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent } from "@testing-library/react";
import { installWindowGuards, isBrowserKey, isEditable } from "./windowGuards";

let remove: () => void;
beforeEach(() => {
  document.body.innerHTML = `
    <button id="button">Подключить</button>
    <table><tr id="row"><td id="cell">Сервер</td></tr></table>
    <input id="text" type="text" />
    <input id="password" type="password" />
    <input id="check" type="checkbox" />
    <textarea id="area"></textarea>
    <div id="editable" contenteditable="true"><span id="inside">x</span></div>
    <div id="locked" contenteditable="false">x</div>`;
  remove = installWindowGuards();
});
afterEach(() => {
  remove();
  document.body.innerHTML = "";
});

/** `fireEvent` returns false when the default action was prevented. */
function menu(id: string): boolean {
  return fireEvent.contextMenu(document.getElementById(id)!);
}
function press(init: KeyboardEventInit, id = "button"): boolean {
  return fireEvent.keyDown(document.getElementById(id)!, init);
}

describe("window guards", () => {
  it("closes the WebView menu on everything that is not a field", () => {
    for (const id of ["button", "cell", "row", "check", "locked"])
      expect(menu(id), id).toBe(false);
    expect(fireEvent.contextMenu(document.body)).toBe(false);
    expect(fireEvent.contextMenu(document)).toBe(false);
  });

  it("keeps the menu with copy and paste in fields where text is typed", () => {
    for (const id of ["text", "password", "area", "editable", "inside"])
      expect(menu(id), id).toBe(true);
  });

  it("tells a field from other elements", () => {
    expect(isEditable(document.getElementById("text"))).toBe(true);
    expect(isEditable(document.getElementById("check"))).toBe(false);
    expect(isEditable(document.getElementById("locked"))).toBe(false);
    expect(isEditable(document.getElementById("button"))).toBe(false);
    expect(isEditable(null)).toBe(false);
    expect(isEditable(document)).toBe(false);
  });

  it("closes the developer tools and the page save, print and source keys", () => {
    expect(press({ key: "F12", code: "F12" })).toBe(false);
    for (const code of ["KeyS", "KeyP", "KeyU"])
      expect(press({ code, ctrlKey: true }), code).toBe(false);
    for (const code of ["KeyI", "KeyJ", "KeyC", "KeyP", "KeyS"])
      expect(press({ code, ctrlKey: true, shiftKey: true }), code).toBe(false);
    expect(press({ code: "KeyS", metaKey: true })).toBe(false);
  });

  it("closes the shortcut on the Russian layout, where the letter is another one", () => {
    expect(press({ key: "ы", code: "KeyS", ctrlKey: true })).toBe(false);
    expect(press({ key: "з", code: "KeyP", ctrlKey: true })).toBe(false);
  });

  it("closes them in a field as well", () => {
    expect(press({ code: "KeyS", ctrlKey: true }, "text")).toBe(false);
    expect(press({ code: "F12" }, "area")).toBe(false);
  });

  it("leaves editing, navigation and typed characters alone", () => {
    for (const code of ["KeyV", "KeyC", "KeyA", "KeyX", "KeyZ", "KeyF"])
      expect(press({ code, ctrlKey: true }, "text"), code).toBe(true);
    expect(press({ key: "s", code: "KeyS" }, "text")).toBe(true);
    expect(press({ key: "Tab", code: "Tab" })).toBe(true);
    expect(press({ key: "F5", code: "F5" })).toBe(true);
    expect(press({ code: "KeyS", shiftKey: true })).toBe(true);
    expect(press({ code: "KeyI", ctrlKey: true })).toBe(true);
    // AltGr arrives as Ctrl+Alt and types a character on some layouts.
    expect(press({ code: "KeyS", ctrlKey: true, altKey: true })).toBe(true);
  });

  it("reads the key without a DOM event", () => {
    expect(isBrowserKey(new KeyboardEvent("keydown", { code: "F12" }))).toBe(
      true,
    );
    expect(isBrowserKey(new KeyboardEvent("keydown", { code: "F11" }))).toBe(
      false,
    );
  });

  it("sees the event before a component that stops it", () => {
    const button = document.getElementById("button")!;
    button.addEventListener("keydown", (event) => event.stopPropagation());
    button.addEventListener("contextmenu", (event) => event.stopPropagation());
    expect(press({ code: "KeyS", ctrlKey: true })).toBe(false);
    expect(menu("button")).toBe(false);
  });

  it("is removed by the function it returns, and twice does no harm", () => {
    remove();
    remove();
    expect(menu("button")).toBe(true);
    expect(press({ code: "F12" })).toBe(true);
    remove = installWindowGuards();
    expect(menu("button")).toBe(false);
  });

  it("guards the document it is given, not the others", () => {
    const other = document.implementation.createHTMLDocument("other");
    other.body.innerHTML = '<button id="b">x</button>';
    const button = other.getElementById("b")!;
    const click = () =>
      button.dispatchEvent(
        new MouseEvent("contextmenu", { bubbles: true, cancelable: true }),
      );
    expect(click()).toBe(true);
    const off = installWindowGuards(other);
    expect(click()).toBe(false);
    expect(menu("button")).toBe(false);
    off();
    expect(click()).toBe(true);
  });
});
