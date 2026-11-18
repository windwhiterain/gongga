// One block of a window's log: what it is, and the DOM that draws it.
// Contract: crates/client/src/render_wire.rs RenderMessage, crates/tui/src/log.rs
// (seal, abstract, cap), crates/client/src/lib.rs LineKind::mark and LineKind::folds,
// crates/protocol/src/message.rs Head.

export const FOLDING_KINDS = new Set([
  'reasoning',
  'tool_call',
  'tool_output',
  'tool_error',
  'inject',
]);

const LINE_KINDS = new Set([
  'system',
  'inject',
  'command',
  'user',
  'assistant',
  'reasoning',
  'tool_call',
  'tool_output',
  'tool_error',
  'result',
  'error',
]);

// Contract: crates/client/src/lib.rs LineKind::symbol and LineKind::mark.
const MARKS = {
  system: '',
  inject: '\u21b3 ',
  command: '$ ',
  user: '\u276f ',
  assistant: '',
  reasoning: '\u25c7 ',
  tool_call: '\u25b8 ',
  tool_output: '\u2713 ',
  tool_error: '\u2715 ',
  result: '\u2192 ',
  error: '! ',
};

// Contract: crates/client/src/lib.rs kind_of maps a server-named head to a style.
const HEAD_KINDS = {
  call: 'tool_call',
  ok: 'tool_output',
  failed: 'tool_error',
  reasoning: 'reasoning',
  output: 'assistant',
  cmd: 'command',
  error: 'error',
};

export function lineKindOf(kind) {
  return LINE_KINDS.has(kind) ? kind : 'system';
}

export function headLineKind(kind) {
  return HEAD_KINDS[kind] ?? 'tool_call';
}

/** The folded row: name, summary values, description, joined by the one mark. */
export function headSummary(head) {
  const parts = [];
  if (head.name) parts.push(head.name);
  for (const field of head.fields ?? []) {
    if (field.summary && field.value) parts.push(field.value);
  }
  if (head.description) parts.push(head.description);
  return parts.join('\u00b7');
}

/** Contract: crates/client/src/lib.rs head_label keeps one trailing space. */
export function headLabel(head) {
  const summary = headSummary(head);
  return summary.length === 0 ? '' : summary + ' ';
}

// What a block draws: a summary, plain pre-wrapped text, finished document rows,
// a described call's fields, or an opened item's box.
function bodyKindOf(block) {
  if (block.boxed) return 'box';
  // A sealing fold owns its summary whatever the body turned out to hold.
  if (block.foldable && block.sealed) return 'summary';
  if (block.hasRows) return 'rows';
  return 'text';
}

// What an opened item's body holds: the same three, minus the summary.
function innerKindOf(block) {
  if (block.head !== null) return 'fields';
  if (block.hasRows) return 'rows';
  return 'text';
}

// SPEC cuts an abstract to one line; a line longer than 120 characters loses its end.
function firstLine(text) {
  const line = text.replace(/\r/g, '').split('\n')[0];
  const codepoints = Array.from(line);
  return codepoints.length > 120 ? codepoints.slice(0, 120).join('') + '\u2026' : line;
}

export class Block {
  constructor(window, key, kind, tag, text) {
    this.window = window;
    this.key = key;
    this.kind = kind;
    this.tag = tag;
    this.text = text;
    this.rows = null;
    this.head = null;
    this.sealed = false;
    this.expanded = false;
    this.capRows = 0;
    this.edgeColumns = 2;
    this.dropped = false;
    this.rev = 0;
    this.el = null;
    // The transcript rows this block came from, and where its box is scrolled.
    this.seq = 0;
    this.from = 0;
    this.inner = 0;
  }

  get foldable() {
    return FOLDING_KINDS.has(this.kind);
  }

  get hasRows() {
    return this.rows !== null && this.rows.length > 0;
  }

  // SPEC's opened item: rules above and below, and a body that scrolls inside.
  get boxed() {
    return this.expanded && this.foldable;
  }

  // SPEC caps only a sealed, opened block that folds: it holds rows it does not show.
  get capped() {
    return this.boxed && this.sealed && this.capRows > 0;
  }

  // The top rule's words: the item's own label, without the mark column's space.
  topLabel() {
    return this.tag.trim();
  }

  mark() {
    return MARKS[this.kind] ?? '';
  }

  foldMark() {
    if (!this.foldable || !this.sealed) return '';
    return this.expanded ? '\u25be' : '\u25b8';
  }

  // What a sealed fold shows: its summary, never its body's first line alone.
  // Contract: crates/tui/src/log.rs summary_text.
  abstract() {
    if (this.head !== null) return headSummary(this.head);
    const label = this.tag.trim();
    if (label.length !== 0) return label;
    return firstLine(this.text);
  }
}

function makeElement(tagName, className) {
  const element = document.createElement(tagName);
  if (className) element.className = className;
  return element;
}

// The block's frame: a fold column, the kind's mark, and whatever the body draws.
// An opened folding item is SPEC's box: a top rule carrying its label, a body that
// scrolls inside when `block_size` caps it, and a bottom rule.
// Contract: crates/tui/src/render.rs ruled_row, crates/tui/src/log.rs Rule.
export function createBlockElement(block) {
  const el = makeElement('div', 'block');
  el._fold = makeElement('span', 'fold');
  el._mark = makeElement('span', 'mark');
  el._main = makeElement('div', 'block-main');
  el.append(el._fold, el._mark, el._main);
  el._block = block;
  el._contentKind = null;
  el._innerKind = null;
  el._textHolder = null;
  el._textNode = null;
  el._rows = null;
  el._fields = null;
  el._boxBody = null;
  el._content = null;
  el._tagEl = null;
  el._topLabel = null;
  updateBlockElement(el, block);
  return el;
}

export function updateBlockElement(el, block) {
  const boxed = block.boxed;
  const folded = block.foldable && block.sealed && !block.expanded;
  el.className = 'block kind-' + block.kind + (block.foldable ? ' foldable' : '')
    + (boxed ? ' boxed' : '') + (folded ? ' folded' : '');
  el._fold.textContent = block.foldMark();
  el._mark.textContent = block.mark();

  const kind = bodyKindOf(block);
  if (el._contentKind !== kind) {
    el._contentKind = kind;
    buildStructure(el, block, kind);
  }
  if (kind === 'box') {
    if (el._topLabel !== null) el._topLabel.textContent = block.topLabel();
    const inner = innerKindOf(block);
    if (el._innerKind !== inner) {
      el._innerKind = inner;
      el._content.replaceChildren();
      el._textHolder = null;
      el._textNode = null;
      el._rows = null;
      el._fields = null;
    }
    if (inner === 'fields') setFields(el, block.head);
    else if (inner === 'rows') setRows(el, block.rows);
    else setText(el, block.text, 'text');
    const body = el._boxBody;
    body.classList.toggle('capped', block.capped);
    body.style.maxHeight = block.capped ? block.capRows * 20 + 'px' : '';
    if (Math.abs(body.scrollTop - block.inner) > 0.5) body.scrollTop = block.inner;
    return;
  }
  if (el._tagEl !== null) el._tagEl.textContent = block.tag;
  if (kind === 'summary') setText(el, block.abstract(), 'text summary');
  else if (kind === 'text') setText(el, block.text, 'text');
  else if (kind === 'rows') setRows(el, block.rows);
  else setFields(el, block.head);
}

// One frame change replaces the parts a change of kind makes impossible.
function buildStructure(el, block, kind) {
  el._main.replaceChildren();
  el._content = null;
  el._boxBody = null;
  el._tagEl = null;
  el._topLabel = null;
  el._textHolder = null;
  el._textNode = null;
  el._rows = null;
  el._fields = null;
  if (kind === 'box') {
    const top = makeElement('div', 'box-rule top');
    el._topLabel = makeElement('span', 'box-label');
    top.append(el._topLabel, makeElement('span', 'box-fill'));
    const body = makeElement('div', 'box-body');
    el._content = makeElement('div', 'block-content');
    body.append(el._content);
    body.addEventListener('scroll', () => { el._block.inner = body.scrollTop; }, { passive: true });
    const bottom = makeElement('div', 'box-rule bottom');
    bottom.append(makeElement('span', 'box-fill'));
    el._main.append(top, body, bottom);
    el._boxBody = body;
    return;
  }
  if (kind !== 'summary' && !block.foldable && block.tag.length !== 0) {
    el._tagEl = makeElement('div', 'tag');
    el._tagEl.textContent = block.tag;
    el._main.append(el._tagEl);
  }
  el._content = makeElement('div', 'block-content');
  el._main.append(el._content);
}

// A streaming body is appended to, not rewritten: a delta is the delta's worth.
function setText(el, text, className) {
  if (el._textHolder === null) {
    el._textHolder = makeElement('div', className);
    el._textNode = document.createTextNode('');
    el._textHolder.append(el._textNode);
    el._content.append(el._textHolder);
  }
  el._textHolder.className = className;
  const node = el._textNode;
  const previous = node.data;
  if (text.length > previous.length && text.startsWith(previous)) node.appendData(text.slice(previous.length));
  else node.data = text;
}

// A document is replaced whole: a delta can change a row above the last one.
function setRows(el, rows) {
  if (el._rows === null) {
    el._rows = makeElement('div', 'rows');
    el._content.append(el._rows);
  }
  const holder = el._rows;
  const fragment = document.createDocumentFragment();
  for (const row of rows) {
    const line = makeElement('div', 'doc-row');
    for (const span of row) {
      const run = makeElement('span', 'span-' + (span.class ?? 'text'));
      run.textContent = span.text ?? '';
      line.append(run);
    }
    fragment.append(line);
  }
  holder.replaceChildren(fragment);
}

// An opened call: its arguments, each under the name it is read by.
function setFields(el, head) {
  if (el._fields === null) {
    el._fields = makeElement('div', 'head-fields');
    el._content.append(el._fields);
  }
  const holder = el._fields;
  const fragment = document.createDocumentFragment();
  for (const field of head.fields ?? []) {
    const label = makeElement('div', 'head-field-label');
    label.textContent = field.label ?? '';
    const value = makeElement('div', 'head-field-value');
    value.textContent = field.value ?? '';
    fragment.append(label, value);
  }
  holder.replaceChildren(fragment);
}

// The session list as a tree: every prefix of a session name is a row, and a group
// stands where its newest session stands. Contract: crates/tui/src/tree.rs rows.
// The mark a row carries is the order tree.rs spells: error, stopped, running, idle.
export function sessionRows(listed, folded, unseen, shown) {
  const shownNames = new Set(shown);
  const nodes = [];
  const index = new Map();
  const roots = [];
  const markOf = (item) => {
    let mark = item.status === 'running' ? 1 : item.status === 'error' ? 3 : 0;
    if (mark === 0 && unseen.has(item.name)) mark = 2;
    return mark;
  };
  for (let place = 0; place < listed.length; place += 1) {
    const item = listed[place];
    const mark = markOf(item);
    let prefix = '';
    let parent = -1;
    let depth = 0;
    for (const segment of item.name.split('/')) {
      prefix = depth === 0 ? segment : prefix + '/' + segment;
      let at = index.get(prefix);
      if (at === undefined) {
        at = nodes.length;
        nodes.push({
          name: prefix,
          label: segment,
          depth,
          children: [],
          session: false,
          own: 0,
          under: 0,
          latest: place,
          title: '',
        });
        index.set(prefix, at);
        if (parent >= 0) nodes[parent].children.push(at);
        else roots.push(at);
      }
      nodes[at].under = Math.max(nodes[at].under, mark);
      nodes[at].latest = Math.max(nodes[at].latest, place);
      parent = at;
      depth += 1;
    }
    if (parent >= 0) {
      nodes[parent].session = true;
      nodes[parent].own = Math.max(nodes[parent].own, mark);
      nodes[parent].title = item.title;
    }
  }
  const byLatest = (left, right) => nodes[left].latest - nodes[right].latest;
  roots.sort(byLatest);
  for (const node of nodes) node.children.sort(byLatest);
  const rows = [];
  const stack = roots.slice().reverse();
  while (stack.length !== 0) {
    const node = nodes[stack.pop()];
    const group = node.children.length !== 0;
    const collapsed = group && folded.has(node.name);
    rows.push({
      name: node.name,
      label: node.label,
      depth: node.depth,
      session: node.session,
      group,
      collapsed,
      mark: node.session ? node.own : node.under,
      shown: node.session && shownNames.has(node.name),
      title: node.session ? node.title : '',
    });
    if (collapsed) continue;
    for (let at = node.children.length - 1; at >= 0; at -= 1) stack.push(node.children[at]);
  }
  return rows;
}
