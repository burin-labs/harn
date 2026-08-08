// The std/ui renderer script, exercised against a minimal DOM.
//
// The renderer lives as an embedded HTML string in
// crates/harn-stdlib/src/stdlib/ui/renderer.harn and had no test surface at
// all. This loads the real script — same substitutions the Harn side performs —
// drives it through the real host protocol, and asserts on the tree it builds.
//
// The property under test is node identity across a redraw. `render()` used to
// call `root.replaceChildren()`, so every update detached every node. A field
// commits on `change`, which fires on blur, which happens on `mousedown` — so
// pressing a button published an update that destroyed the button before
// `mouseup`, and the browser never synthesized the `click` (#6011).

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const RENDERER = new URL(
  "../crates/harn-stdlib/src/stdlib/ui/renderer.harn",
  import.meta.url,
);

const source = await readFile(RENDERER, "utf8");
const html = source.split('const UI_RENDERER_HTML = """')[1]?.split('"""')[0];
assert.ok(html, "renderer.harn must define UI_RENDERER_HTML");
const script = html.split("<script>")[1]?.split("</script>")[0];
assert.ok(script, "the renderer HTML must contain one script block");

/** Element-only DOM shim: enough for the renderer, and nothing it can hide in. */
class ShimNode {
  constructor(tagName) {
    this.tagName = tagName;
    this.childNodes = [];
    this.parentNode = null;
    this.dataset = {};
    this.attributes = {};
    this.className = "";
    this.ownText = "";
  }

  get children() {
    return this.childNodes;
  }

  get firstChild() {
    return this.childNodes[0] ?? null;
  }

  get nextSibling() {
    const siblings = this.parentNode?.childNodes ?? [];
    return siblings[siblings.indexOf(this) + 1] ?? null;
  }

  get classList() {
    const node = this;
    const read = () => node.className.split(" ").filter(Boolean);
    const write = (names) => {
      node.className = names.join(" ");
    };
    return {
      add(name) {
        const names = read();
        if (!names.includes(name)) {
          write([...names, name]);
        }
      },
      remove(name) {
        write(read().filter((entry) => entry !== name));
      },
      contains(name) {
        return read().includes(name);
      },
      toggle(name, force) {
        if (force) {
          this.add(name);
        } else {
          this.remove(name);
        }
      },
    };
  }

  get textContent() {
    if (this.childNodes.length === 0) {
      return this.ownText;
    }
    return this.childNodes.map((child) => child.textContent).join("");
  }

  set textContent(value) {
    for (const child of this.childNodes) {
      child.parentNode = null;
    }
    this.childNodes = [];
    this.ownText = value;
  }

  insertBefore(node, reference) {
    metrics.insertBefore += 1;
    node.parentNode?.removeChildQuietly(node);
    const index = reference ? this.childNodes.indexOf(reference) : -1;
    if (index === -1) {
      this.childNodes.push(node);
    } else {
      this.childNodes.splice(index, 0, node);
    }
    node.parentNode = this;
    return node;
  }

  removeChildQuietly(node) {
    const index = this.childNodes.indexOf(node);
    if (index !== -1) {
      this.childNodes.splice(index, 1);
    }
    node.parentNode = null;
  }

  removeChild(node) {
    metrics.removeChild += 1;
    this.removeChildQuietly(node);
    return node;
  }

  remove() {
    this.parentNode?.removeChild(this);
  }

  append(...nodes) {
    for (const node of nodes) {
      this.insertBefore(node, null);
    }
  }

  prepend(node) {
    this.insertBefore(node, this.firstChild);
  }

  replaceChildren(...nodes) {
    for (const child of [...this.childNodes]) {
      this.removeChild(child);
    }
    this.append(...nodes);
  }

  setAttribute(name, value) {
    this.attributes[name] = value;
  }

  descendants() {
    return this.childNodes.flatMap((child) => [child, ...child.descendants()]);
  }

  querySelectorAll(selector) {
    return this.descendants().filter((node) => node.tagName === selector);
  }

  getBoundingClientRect() {
    return { left: 0, top: 0, width: 100, height: 100 };
  }

  getContext() {
    return {
      clearRect() {},
      beginPath() {},
      moveTo() {},
      lineTo() {},
      stroke() {},
    };
  }
}

const metrics = { insertBefore: 0, removeChild: 0 };

function findBy(node, predicate) {
  for (const child of node.descendants()) {
    if (predicate(child)) {
      return child;
    }
  }
  return null;
}

/** Boot the renderer and return handles for driving and inspecting it. */
async function boot() {
  metrics.insertBefore = 0;
  metrics.removeChild = 0;
  const root = new ShimNode("main");
  root.id = "app";
  const outbox = [];
  let awaitOutbox = null;
  const parent = {
    postMessage(message) {
      outbox.push(message);
      awaitOutbox?.();
      awaitOutbox = null;
    },
  };
  const listeners = [];
  const document = {
    title: "",
    createElement: (tagName) => new ShimNode(tagName),
    getElementById: (id) =>
      id === "app" ? root : findBy(root, (node) => node.id === id),
    querySelector: () => null,
  };
  const context = vm.createContext({
    document,
    parent,
    setTimeout,
    structuredClone,
    addEventListener: (kind, handler) => {
      if (kind === "message") {
        listeners.push(handler);
      }
    },
  });
  vm.runInContext(
    script
      .replace("__HARN_UI_TOOL__", '"app_event"')
      .replace("__HARN_UI_PORTABLE__", "null"),
    context,
  );

  const nextRequest = async () => {
    while (true) {
      const message = outbox.shift();
      if (message?.id !== undefined) {
        return message;
      }
      if (message !== undefined) {
        continue;
      }
      await new Promise((resolve) => {
        awaitOutbox = resolve;
      });
    }
  };
  const reply = (id, result) => {
    for (const handler of listeners) {
      handler({ source: parent, data: { jsonrpc: "2.0", id, result } });
    }
  };

  const initialize = await nextRequest();
  assert.equal(initialize.method, "ui/initialize");
  reply(initialize.id, { protocolVersion: "2026-01-26" });

  /** Answer the renderer's pending tool call with one document. */
  const answer = async (elements, title) => {
    const call = await nextRequest();
    assert.equal(call.method, "tools/call");
    reply(call.id, {
      structuredContent: {
        schema: "harn.ui_update.v1",
        document: { schema: "harn.ui_document.v1", title, elements },
        effects: [],
      },
    });
    // Let the renderer's promise chain settle before the tree is inspected.
    await new Promise((resolve) => setTimeout(resolve, 0));
  };

  const byId = (id) => findBy(root, (node) => node.dataset.uiId === id);
  return {
    root,
    document,
    byId,
    /** The interactive control inside a field/select wrapper. */
    control: (id) => findBy(root, (node) => node.id === id),
    /** Answer the `ready` event the renderer sends once on start. */
    ready: (elements, title = "Test App") => answer(elements, title),
    /** Fire a real handler, then answer the event it sends. */
    exchange: async (trigger, elements, title = "Test App") => {
      trigger();
      await answer(elements, title);
    },
  };
}

const button = (id, label, parent = "") => ({
  id,
  kind: "button",
  label,
  parent,
});
const field = (id, label, value, parent = "") => ({
  id,
  kind: "field",
  label,
  value,
  parent,
});

test("a redraw keeps the node a click is already in flight on", async () => {
  const app = await boot();
  await app.ready([field("direction", "Direction", ""), button("add", "Add")]);
  const before = app.byId("add");
  assert.ok(before, "the button must be rendered");

  // Exactly what happens when the button is pressed: `mousedown` blurs the
  // field, the field's `change` publishes an update, and that update lands
  // before `mouseup`.
  await app.exchange(
    () => app.control("direction").onchange(),
    [field("direction", "Direction", "north"), button("add", "Add")],
  );
  assert.equal(
    app.byId("add"),
    before,
    "the button node must survive the field update, or mouseup lands elsewhere and no click is synthesized",
  );
});

test("an unchanged document mutates nothing", async () => {
  const app = await boot();
  const elements = [field("direction", "Direction", ""), button("add", "Add")];
  await app.ready(elements);
  const inserts = metrics.insertBefore;
  const removals = metrics.removeChild;

  await app.exchange(() => app.byId("add").onclick(), elements);
  assert.equal(metrics.insertBefore, inserts, "no node may be re-inserted");
  assert.equal(metrics.removeChild, removals, "no node may be removed");
});

test("field values and button labels update in place", async () => {
  const app = await boot();
  await app.ready([
    field("direction", "Direction", "north"),
    button("add", "Add"),
  ]);
  const control = app.control("direction");
  const action = app.byId("add");

  await app.exchange(
    () => action.onclick(),
    [
      field("direction", "Direction", "south"),
      { ...button("add", "Add"), disabled: true },
    ],
  );
  assert.equal(app.control("direction"), control, "the control node is reused");
  assert.equal(control.value, "south");
  assert.equal(action.disabled, true);
});

test("added, removed, and reordered elements are applied without recreating survivors", async () => {
  const app = await boot();
  await app.ready([button("a", "A"), button("b", "B")]);
  const a = app.byId("a");
  const b = app.byId("b");

  await app.exchange(
    () => a.onclick(),
    [button("b", "B"), button("c", "C"), button("a", "A")],
  );
  assert.equal(app.byId("a"), a, "`a` is reordered, not rebuilt");
  assert.equal(app.byId("b"), b, "`b` is reordered, not rebuilt");
  assert.deepEqual(
    app.root.childNodes.map((node) => node.dataset.uiId),
    ["b", "c", "a"],
  );

  await app.exchange(() => a.onclick(), [button("b", "B")]);
  assert.deepEqual(
    app.root.childNodes.map((node) => node.dataset.uiId),
    ["b"],
  );
});

test("an id that changes kind is rebuilt rather than reused", async () => {
  const app = await boot();
  await app.ready([button("slot", "A")]);
  const before = app.byId("slot");

  await app.exchange(
    () => before.onclick(),
    [{ id: "slot", kind: "text", text: "A", parent: "" }],
  );
  const after = app.byId("slot");
  assert.notEqual(after, before, "a button must not be reused as a paragraph");
  assert.equal(after.tagName, "p");
});

test("nested children are placed under their declared parent", async () => {
  const app = await boot();
  await app.ready([
    { id: "row", kind: "row", parent: "" },
    button("add", "Add", "row"),
  ]);
  const row = app.byId("row");
  assert.deepEqual(
    row.childNodes.map((node) => node.dataset.uiId),
    ["add"],
  );

  await app.exchange(
    () => app.byId("add").onclick(),
    [
      { id: "row", kind: "row", parent: "" },
      button("add", "Add Direction", "row"),
    ],
  );
  assert.equal(row.childNodes[0].textContent, "Add Direction");
});

test("a canvas keeps its node and its handlers across a redraw", async () => {
  const app = await boot();
  const canvas = (strokes) => ({
    id: "sketch",
    kind: "canvas",
    width: 640,
    height: 360,
    strokes,
    parent: "",
  });
  await app.ready([canvas([]), button("add", "Add")]);
  const node = app.byId("sketch");
  const onpointerup = node.onpointerup;

  await app.exchange(
    () => app.byId("add").onclick(),
    [canvas([{ points: [{ x: 0, y: 0 }] }]), button("add", "Add")],
  );
  assert.equal(app.byId("sketch"), node, "the canvas node is reused");
  assert.equal(
    node.onpointerup,
    onpointerup,
    "stroke handlers are bound once, so a redraw cannot drop an in-progress stroke",
  );
  assert.equal(
    node.harnSpec.strokes.length,
    1,
    "the handlers read the current spec from the node",
  );
});
