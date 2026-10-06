// Run from the repository root: node tests/browser-scripts.cjs
// These DOM fixtures exercise script behavior without a browser or network.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function testFind() {
  const source = fs.readFileSync('src/browser/find.rs', 'utf8');
  const script = name => source.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`))[1];
  let reads = 0;
  let now = 0;
  let text = 'Alpha needle BETA needle';
  const body = { get innerText() { reads++; return text; } };
  const timers = new Map();
  const listeners = new Map();
  const observers = [];
  const messages = [];
  let timerId = 0;
  const context = vm.createContext({
    document: {
      body, documentElement: {},
      fonts: {
        addEventListener(name, callback) { listeners.set(`font:${name}`, callback); },
        removeEventListener(name) { listeners.delete(`font:${name}`); }
      }
    },
    window: { ipc: { postMessage(message) { messages.push(message); } } },
    performance: { now: () => now },
    MutationObserver: class {
      constructor(callback) { this.callback = callback; observers.push(this); }
      observe(target, options) { this.target = target; this.options = options; }
      disconnect() { this.disconnected = true; }
    },
    setTimeout(callback) { const id = ++timerId; timers.set(id, callback); return id; },
    clearTimeout(id) { timers.delete(id); },
    addEventListener(name, callback) { listeners.set(name, callback); },
    removeEventListener(name, callback) { if (listeners.get(name) === callback) listeners.delete(name); }
  });
  const count = vm.runInContext(script('COUNT_SCRIPT'), context);
  const watch = vm.runInContext(script('WATCH_SCRIPT'), context);
  watch(1);
  assert.equal(count('needle'), 2);
  assert.equal(count('alpha'), 1);
  for (let iteration = 0; iteration < 100; iteration++) assert.equal(count('needle'), 2);
  assert.equal(reads, 1, 'Unchanged queries reuse rendered text');
  watch(2);
  assert.equal(observers.length, 1, 'Next match reuses the observer');
  assert.equal(observers[0].target, context.document.documentElement);
  assert(observers[0].options.attributeFilter.includes('hidden'));
  text += ' needle';
  observers[0].callback();
  watch(3);
  for (const [id, callback] of timers) { timers.delete(id); callback(); }
  assert.equal(messages.at(-1), 'find-dirty:3', 'Pending recount uses the latest serial');
  assert.equal(count('needle'), 3);
  assert.equal(reads, 2, 'Mutation refreshes text exactly once');
  for (const [event, argument] of [
    ['resize'], ['load', {target:{tagName:'LINK'}}],
    ['transitionend'], ['animationend'], ['font:loadingdone']
  ]) {
    const before = reads;
    listeners.get(event)(argument);
    assert.equal(count('needle'), 3);
    assert.equal(reads, before + 1, `${event} invalidates rendered text`);
  }
  const beforeImageLoad = reads;
  listeners.get('load')({target:{tagName:'IMG'}});
  count('needle');
  assert.equal(reads, beforeImageLoad, 'Image completion keeps the text cache');
  text += ' needle';
  now += 1500;
  assert.equal(count('needle'), 4, 'Age limit covers CSSOM changes with no DOM mutation');
  assert.equal(count(''), 0);
  context.document.body = { get innerText() { reads++; return 'New needle'; } };
  watch(4);
  assert.equal(observers[0].disconnected, true);
  assert.equal(observers.length, 2);
  assert.equal(count('needle'), 1, 'Replacing the body invalidates old text');
  const stop = source.match(/const STOP_WATCH_SCRIPT: &str =\s*"([^"]*)";/)[1];
  vm.runInContext(stop, context);
  assert.equal(context.window.__vamprowserFindWatch, undefined);
  assert.equal(listeners.size, 0);
  assert.equal(timers.size, 0);
  assert.equal(observers[1].disconnected, true);
}

function testReader() {
  const source = fs.readFileSync('src/browser/reader.js', 'utf8');
  let reads = 0;
  const make = (text, tag='p') => ({
    id: 'story', className: '', hidden: false, tag,
    getAttribute() { return null; },
    get innerText() { reads++; return text; },
    matches(selector) { return selector.startsWith(tag + ','); }
  });
  const paragraphs = Array.from({length:40}, (_, index) =>
    make(`Paragraph ${index}: A substantial article paragraph with enough text to be scored.`));
  const links = [make('Read the source', 'a')];
  const articleText = paragraphs.map(paragraph => paragraph.innerText).join('\n');
  const candidates = Array.from({length:8}, (_, index) => {
    const candidate = make(articleText, index ? 'article' : 'main');
    candidate.id = `story-${index}`;
    candidate.querySelectorAll = selector => selector === 'p' ? paragraphs : links;
    return candidate;
  });
  candidates[2].hidden = true;
  candidates[3].className = 'advert-banner';
  reads = 0;
  const beforeClone = source.indexOf('  const article = source.element.cloneNode(true);');
  assert(beforeClone > 0);
  const selection = source.slice(0, beforeClone)
    + 'return { value: source.value, id: source.element.id };})()';
  const result = vm.runInNewContext(selection, {
    window: {}, document: {body:candidates[0], querySelectorAll:()=>candidates},
    getComputedStyle:()=>({display:'block',visibility:'visible'})
  });
  assert.equal(result.id, 'story-1', 'Visible article wins over main and rejected candidates');
  assert.equal(reads, 47, 'Shared paragraphs and links are read once across nested candidates');
  assert(Math.abs(result.value - 7194.662756598241) < 1e-6, 'Article scoring stays unchanged');
}

testFind();
testReader();
console.log('Browser script regressions passed.');
