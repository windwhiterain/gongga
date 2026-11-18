// The wire speaks UTF-8 byte offsets on character boundaries; a textarea speaks
// UTF-16 indices. This module converts both ways and splices an accepted item in.
// Contract: crates/client/src/complete.rs (Item, apply, expand),
// crates/client/src/runtime.rs (a byte cursor is clamped to a char boundary).

const encoder = new TextEncoder();
const decoder = new TextDecoder();

const WHITESPACE = /^\p{White_Space}$/u;

/** The UTF-8 length of a text, the same unit the wire's ranges use. */
export function byteLength(text) {
  return encoder.encode(text).length;
}

// UTF-8 length of one code point, as TextEncoder would write it.
function width(code) {
  if (code < 0x80) return 1;
  if (code < 0x800) return 2;
  if (code < 0x10000) return 3;
  return 4;
}

/**
 * The UTF-16 index of a UTF-8 byte offset, or null when the offset splits a
 * character or runs past the text. Contract: crates/client/src/complete.rs apply
 * refuses a range that is not on a character boundary.
 */
export function byteToIndex(text, byteOffset) {
  if (!Number.isInteger(byteOffset) || byteOffset < 0) return null;
  let bytes = 0;
  let index = 0;
  while (index < text.length) {
    if (bytes === byteOffset) return index;
    const code = text.codePointAt(index);
    const step = width(code);
    if (bytes + step > byteOffset) return null;
    bytes += step;
    index += step === 4 ? 2 : 1;
  }
  return bytes === byteOffset ? index : null;
}

/** The UTF-8 byte offset of a UTF-16 index, rounded down off a surrogate pair. */
export function indexToByte(text, utf16Index) {
  let index = Math.min(Math.max(0, utf16Index | 0), text.length);
  if (index < text.length && isLowSurrogate(text.charCodeAt(index))) index -= 1;
  let bytes = 0;
  let at = 0;
  while (at < index) {
    const code = text.codePointAt(at);
    bytes += width(code);
    at += code > 0xffff ? 2 : 1;
  }
  return bytes;
}

function isLowSurrogate(unit) {
  return unit >= 0xdc00 && unit <= 0xdfff;
}

// A braced or bare snippet stop beginning at from: its number, where it ends, and
// the default text it writes. Contract: crates/client/src/complete.rs braced_stop.
function bracedStop(bytes, from) {
  const number = bytes[from + 2];
  if (number === undefined || number < 48 || number > 57) return null;
  const next = bytes[from + 3];
  if (next === 125) return { number: number - 48, after: from + 4, defaultRange: null };
  if (next === 58) {
    for (let at = from + 4; at < bytes.length; at += 1) {
      if (bytes[at] === 125) return { number: number - 48, after: at + 1, defaultRange: [from + 4, at] };
    }
  }
  return null;
}

/**
 * The text a snippet writes and the byte offset of its caret inside it.
 * Contract: crates/client/src/complete.rs expand. A stop is a dollar sign and a
 * digit, bare or inside braces; a braced stop may carry a default after a colon.
 */
export function expandSnippet(snippet) {
  const bytes = encoder.encode(snippet);
  const out = [];
  let stop = null;
  let at = 0;
  while (at < bytes.length) {
    let number = -1;
    let after = 0;
    let defaultRange = null;
    if (bytes[at] === 36) {
      const digit = bytes[at + 1];
      if (digit >= 48 && digit <= 57) {
        number = digit - 48;
        after = at + 2;
      } else if (digit === 123) {
        const found = bracedStop(bytes, at);
        if (found !== null) {
          number = found.number;
          after = found.after;
          defaultRange = found.defaultRange;
        }
      }
    }
    if (number < 0) {
      out.push(bytes[at]);
      at += 1;
      continue;
    }
    // The lowest stop decides, and the first of that number keeps the place.
    if (stop === null || number < stop.number) stop = { number, at: out.length };
    if (defaultRange !== null) {
      for (let copy = defaultRange[0]; copy < defaultRange[1]; copy += 1) out.push(bytes[copy]);
    }
    at = after;
  }
  return {
    text: decoder.decode(Uint8Array.from(out)),
    caret: stop === null ? null : stop.at,
  };
}

/**
 * Replaces the wire's byte range [start, end) with one item's text.
 * Contract: crates/client/src/complete.rs Completion::apply. Returns null for a
 * range that is not on a character boundary, a range past the line, or an empty
 * insertion at an empty range.
 */
export function applyCompletion(line, start, end, item) {
  const range = Array.isArray(item.range) ? item.range : [start, end];
  const fromByte = range[0];
  const toByte = range[1];
  if (!Number.isInteger(fromByte) || !Number.isInteger(toByte) || fromByte > toByte) return null;
  if (toByte > byteLength(line)) return null;
  const from = byteToIndex(line, fromByte);
  const to = byteToIndex(line, toByte);
  if (from === null || to === null) return null;
  const expanded = item.snippet ? expandSnippet(item.new_text) : { text: item.new_text, caret: null };
  let text = expanded.text;
  const tail = line.slice(to);
  if (text.endsWith(' ') && tail.length > 0 && WHITESPACE.test(String.fromCodePoint(tail.codePointAt(0)))) {
    text = text.slice(0, -1);
  }
  if (text.length === 0 && from === to) return null;
  let caret = text.length;
  if (expanded.caret !== null) {
    const at = byteToIndex(text, expanded.caret);
    caret = at === null ? text.length : at;
  }
  return { text: line.slice(0, from) + text + tail, caret: from + caret };
}
