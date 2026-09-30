(() => {
  const existing = window.__vamprowserReader;
  if (existing) {
    existing.close();
    return;
  }

  const cleanText = (text) => (text || '').replace(/\s+/g, ' ').trim();
  // Nested article/main candidates share paragraphs and links. Reading
  // innerText repeatedly forces WebKit to traverse the same rendered text.
  let texts = new WeakMap();
  const renderedText = (element) => {
    if (!element) return '';
    const cached = texts.get(element);
    if (cached !== undefined) return cached;
    const text = cleanText(element.innerText);
    texts.set(element, text);
    return text;
  };
  const rejected = /(^|[\s_-])(ad|ads|advert|banner|breadcrumb|comment|cookie|footer|header|menu|nav|newsletter|paywall|promo|related|share|sidebar|social|subscribe|toolbar)([\s_-]|$)/i;
  const isRejected = (element) => rejected.test(`${element.id} ${typeof element.className === 'string' ? element.className : ''} ${element.getAttribute('role') || ''}`);
  const visible = (element) => {
    if (element.hidden || element.getAttribute('aria-hidden') === 'true') return false;
    const style = getComputedStyle(element);
    return style.display !== 'none' && style.visibility !== 'hidden';
  };
  const score = (element) => {
    if (!visible(element) || isRejected(element)) return -1;
    const text = renderedText(element);
    if (text.length < 160) return -1;
    const paragraphs = [...element.querySelectorAll('p')].filter((p) => renderedText(p).length >= 40);
    const paragraphLength = paragraphs.reduce((sum, p) => sum + renderedText(p).length, 0);
    const linkLength = [...element.querySelectorAll('a')].reduce((sum, a) => sum + renderedText(a).length, 0);
    const density = 1 - Math.min(1, linkLength / Math.max(text.length, 1));
    let bonus = element.matches('article, [itemprop="articleBody"]') ? 600 : 0;
    if (element.matches('main, [role="main"]')) bonus += 250;
    return (paragraphLength + paragraphs.length * 90 + bonus) * density;
  };

  const candidates = [...document.querySelectorAll('article, main, [role="main"], [itemprop="articleBody"], .article, .post, .entry-content')];
  let source = candidates.map((element) => ({ element, value: score(element) }))
    .sort((a, b) => b.value - a.value)[0];
  if (!source || source.value < 200) source = { element: document.body, value: score(document.body) };
  if (!source.element || source.value < 200) return;

  const article = source.element.cloneNode(true);
  article.querySelectorAll('script, style, template, noscript, iframe, object, embed, form, input, button, select, textarea, nav, aside, footer, header, dialog, [hidden], [aria-hidden="true"]').forEach((element) => element.remove());
  article.querySelectorAll('*').forEach((element) => {
    if (isRejected(element)) {
      element.remove();
      return;
    }
    for (const attribute of [...element.attributes]) {
      if (/^on/i.test(attribute.name) || attribute.name === 'style' || attribute.name === 'srcdoc') element.removeAttribute(attribute.name);
    }
    for (const name of ['href', 'src', 'poster']) {
      const value = element.getAttribute(name);
      if (!value) continue;
      try {
        const url = new URL(value, document.baseURI);
        if (!['http:', 'https:', 'data:'].includes(url.protocol) || (url.protocol === 'data:' && name !== 'src')) {
          element.removeAttribute(name);
        } else {
          element.setAttribute(name, url.href);
        }
      } catch (_) { element.removeAttribute(name); }
    }
    if (element instanceof HTMLImageElement) {
      const candidate = element.currentSrc || element.getAttribute('data-src');
      if (candidate) {
        try {
          const url = new URL(candidate, document.baseURI);
          if (['http:', 'https:', 'data:'].includes(url.protocol)) element.src = url.href;
        } catch (_) { /* Keep the original src. */ }
      }
      element.removeAttribute('srcset');
      element.removeAttribute('sizes');
      element.removeAttribute('loading');
    }
  });
  if (cleanText(article.textContent).length < 160) return;

  const title = cleanText(document.querySelector('meta[property="og:title"]')?.content)
    || renderedText(source.element.querySelector('h1'))
    || renderedText(document.querySelector('h1'))
    || cleanText(document.title);
  const articleHeading = article.querySelector('h1');
  if (articleHeading && cleanText(articleHeading.textContent) === title) articleHeading.remove();
  const byline = cleanText(document.querySelector('meta[name="author"]')?.content)
    || renderedText(document.querySelector('[rel="author"], [itemprop="author"]'));
  // The remaining reader callbacks only need the extracted article.
  texts = null;
  const host = document.createElement('div');
  host.id = 'vamprowser-reader';
  const shadow = host.attachShadow({ mode: 'closed' });
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(`
    :host { all: initial; position: fixed; inset: 0; z-index: 2147483647; color-scheme: light dark; }
    * { box-sizing: border-box; }
    .reader { width: 100%; height: 100%; overflow: auto; background: #f8f7f3; color: #242321; font: 19px/1.72 Georgia, 'Times New Roman', serif; font-weight: 400; }
    .top { position: sticky; top: 0; z-index: 1; display: flex; justify-content: flex-end; padding: 10px 18px; background: #f8f7f3ee; border-bottom: 1px solid #dedbd4; }
    button { border: 1px solid #c6c2ba; border-radius: 8px; background: #fff; color: #242321; padding: 7px 13px; font: 14px -apple-system, sans-serif; cursor: pointer; }
    button:hover { background: #eeeae2; }
    .page { max-width: 760px; margin: 0 auto; padding: 48px 24px 100px; }
    h1 { font-size: clamp(32px, 5vw, 52px); line-height: 1.15; letter-spacing: -.025em; margin: 0 0 14px; }
    .meta { color: #77736c; font: 14px/1.5 -apple-system, sans-serif; margin-bottom: 42px; }
    article { overflow-wrap: break-word; }
    article h1, article h2, article h3, article h4 { line-height: 1.25; margin: 1.5em 0 .5em; }
    article h1 { font-size: 1.7em; } article h2 { font-size: 1.45em; } article h3 { font-size: 1.2em; }
    article p, article ul, article ol, article blockquote, article pre, article figure { margin: 0 0 1.25em; }
    article img, article video, article picture, article svg { max-width: 100%; height: auto; }
    article img { display: block; margin: 1.5em auto; }
    article figcaption { color: #77736c; font: 14px/1.5 -apple-system, sans-serif; text-align: center; }
    article a { color: #315c87; } article pre { overflow-x: auto; padding: 16px; background: #eceae4; font-size: .8em; }
    article table { display: block; max-width: 100%; overflow-x: auto; border-collapse: collapse; }
    article th, article td { border: 1px solid #d6d2ca; padding: 6px 10px; }
    @media (prefers-color-scheme: dark) {
      .reader, .top { background: #1e1e1d; color: #e9e7e2; }
      .top { border-color: #45443f; } button { background: #33332f; color: #e9e7e2; border-color: #5c5a53; }
      button:hover, article pre { background: #45443f; } article a { color: #a4c9ee; }
      article th, article td { border-color: #55534c; }
    }
  `);
  shadow.adoptedStyleSheets = [sheet];
  const reader = document.createElement('div');
  reader.className = 'reader';
  const top = document.createElement('div');
  top.className = 'top';
  const closeButton = document.createElement('button');
  closeButton.textContent = 'Close Reader';
  closeButton.setAttribute('aria-label', 'Close Reader Mode');
  top.append(closeButton);
  const page = document.createElement('div');
  page.className = 'page';
  const heading = document.createElement('h1');
  heading.textContent = title;
  const meta = document.createElement('div');
  meta.className = 'meta';
  meta.textContent = [byline, location.hostname].filter(Boolean).join(' · ');
  const body = document.createElement('article');
  body.append(article);
  page.append(heading, meta, body);
  reader.append(top, page);
  shadow.append(reader);

  const oldOverflow = document.documentElement.style.overflow;
  const close = () => {
    if (window.__vamprowserReader?.host !== host) return;
    document.documentElement.style.overflow = oldOverflow;
    host.remove();
    delete window.__vamprowserReader;
  };
  closeButton.addEventListener('click', close);
  document.documentElement.append(host);
  document.documentElement.style.overflow = 'hidden';
  window.__vamprowserReader = { host, close };
})();
