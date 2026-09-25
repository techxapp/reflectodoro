// Allowlist HTML sanitizer for the Quote API attribution field (see
// CLAUDE.md's "Quote API (end-of-break panel)"). This is the one place in
// the app that renders user-configured content as HTML rather than plain
// text -- everywhere else, including the quote text itself, is deliberately
// escaped/textContent-only (see native_overlay.html's own comment on that).
// attribution is admin/self-configured, but it still lives in an ordinary
// app_setting row that a file import can also write, so it's sanitized
// before render rather than trusted outright.
//
// Mirrored as plain JS in native_overlay.html's own sanitizer (that file has
// no bundler/imports). The default attribution text itself is NOT duplicated
// here -- db.rs's DEFAULT_QUOTE_API_ATTRIBUTION const is the single source of
// truth (it's what seeds/backfills the app_setting row), fetched by the
// frontend via the default_quote_api_attribution command when it needs it
// (Settings' placeholder/"Reset to default"), same as any other app_setting
// default.

const ALLOWED_TAGS = new Set(["A", "B", "I", "EM", "STRONG", "BR", "SPAN", "P"]);
const ALLOWED_ATTRS: Record<string, string[]> = {
  A: ["href", "target"],
};

function sanitizeNode(node: Node, out: Node[]): void {
  if (node.nodeType === Node.TEXT_NODE) {
    out.push(node.cloneNode(false));
    return;
  }
  if (node.nodeType !== Node.ELEMENT_NODE) {
    // Comments, etc. -- dropped entirely.
    return;
  }
  const el = node as Element;
  if (!ALLOWED_TAGS.has(el.tagName)) {
    // Not an allowed element: drop the tag but keep sanitizing its children,
    // so e.g. a stray <div>text</div> still surfaces "text" rather than
    // vanishing along with the wrapper.
    for (const child of Array.from(el.childNodes)) {
      sanitizeNode(child, out);
    }
    return;
  }

  const clean = document.createElement(el.tagName);
  for (const attrName of ALLOWED_ATTRS[el.tagName] ?? []) {
    const value = el.getAttribute(attrName);
    if (value == null) continue;
    if (attrName === "href") {
      // Only http(s) links -- blocks javascript:/data:/vbscript: etc.
      if (!/^https?:\/\//i.test(value.trim())) continue;
      clean.setAttribute("href", value);
    } else if (attrName === "target") {
      if (value === "_blank") {
        clean.setAttribute("target", "_blank");
        // Forced whenever target="_blank" is kept, regardless of what the
        // stored HTML said -- never trust rel from the input.
        clean.setAttribute("rel", "noopener noreferrer");
      }
    }
  }

  const childOut: Node[] = [];
  for (const child of Array.from(el.childNodes)) {
    sanitizeNode(child, childOut);
  }
  for (const c of childOut) clean.appendChild(c);
  out.push(clean);
}

/** Sanitizes a Quote API attribution HTML string down to a small allowlist
 * (a/b/i/em/strong/br/span/p; only href/target on <a>, href restricted to
 * http(s), target="_blank" always paired with rel="noopener noreferrer").
 * Everything else -- script/style tags, event handler attributes,
 * javascript: URLs -- is stripped. Safe to feed straight into `{@html}`. */
export function sanitizeAttributionHtml(html: string): string {
  if (!html) return "";
  const doc = new DOMParser().parseFromString(html, "text/html");
  const container = document.createElement("div");
  const out: Node[] = [];
  for (const child of Array.from(doc.body.childNodes)) {
    sanitizeNode(child, out);
  }
  for (const n of out) container.appendChild(n);
  return container.innerHTML;
}
