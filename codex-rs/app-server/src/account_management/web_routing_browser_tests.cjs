const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');

class Element {
  constructor(tag, text = '', className = '') {
    Object.assign(this, { tag, text, className, children: [], events: {}, hidden: false });
  }
  append(...children) {
    for (const child of children) {
      child.parent = this;
      this.children.push(child);
    }
  }
  replaceChildren() { this.children = []; }
  setAttribute(name, value) { this[name] = value; }
  addEventListener(name, handler) { this.events[name] = handler; }
  contains(node) { return node === this || this.children.some(child => child.contains(node)); }
  matches(selector) {
    return selector.startsWith('.') ? this.className.split(' ').includes(selector.slice(1)) : this.tag === selector;
  }
  querySelectorAll(selector) {
    const selectors = selector.split(',').map(value => value.trim());
    return this.children.flatMap(child => [
      ...(selectors.some(value => child.matches(value)) ? [child] : []),
      ...child.querySelectorAll(selector),
    ]);
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0]; }
  closest(selector) { return this.matches(selector) ? this : this.parent?.closest(selector); }
  get textContent() { return [this.text, ...this.children.map(child => child.textContent)].filter(Boolean).join('\n'); }
  set textContent(text) { this.text = text; this.children = []; }
  reportValidity() { return this.querySelectorAll('input').every(input => !input.required || input.checked); }
}

function setup() {
  let language = 'en';
  const root = new Element('section');
  const inputs = new Map();
  const calls = [];
  const context = vm.createContext({
    window: { AccountManagerMessages: { language: () => language, locale: () => language } },
    document: { getElementById: () => root, activeElement: null },
  });
  for (const name of ['routing-messages.js', 'routing.js'])
    vm.runInContext(readFileSync(join(__dirname, 'webui', name), 'utf8'), context);
  const api = {
    element: (tag, text, className) => new Element(tag, text, className),
    field: (parent, name, label, type, value, help, attributes = {}) => {
      const group = new Element('div', '', 'form-field');
      const input = new Element(type === 'select' ? 'select' : 'input');
      Object.assign(input, attributes, { id: `field-${name}`, type, value: String(value), checked: type === 'checkbox' && value });
      group.append(new Element('label', label), input, new Element('small', help));
      parent.append(group);
      inputs.set(name, input);
      return input;
    },
    button: (label, handler) => {
      const button = new Element('button', label);
      button.addEventListener('click', handler);
      return button;
    },
    perform: async request => { calls.push(request); },
    operation: async request => { calls.push(request); return { data: null }; },
    busy: false,
  };
  const config = { mode: 'automatic', source: 'decision_service', main_tasks: true, subagents: true, preference: 50, max_effort: 'high', send_task_description: false, local_fallback: false, allowed_models: [], model_roles: { 'gpt-6-luna': 'capability' } };
  const view = {
    config, effectiveConfig: { ...config, preference: 0, max_effort: 'low', subagents: false, allowed_models: ['gpt-6-luna'] },
    overridden: true, userConfigVersion: 'v1', decisionServiceReady: true,
    models: [{ model: 'gpt-6-luna', label: 'Light tasks', role: 'capability', catalogRole: 'economy', effectiveRole: 'capability' }, { model: 'gpt-6.1-sol', label: 'Workhorse', role: 'balanced', catalogRole: 'balanced', effectiveRole: 'balanced' }],
  };
  const render = () => context.window.AccountManagerRouting.render(view, api);
  render();
  return { root, inputs, calls, view, render, changeLanguage: value => { language = value; render(); } };
}

(async () => {
  const harness = setup();
  const { root, inputs, calls } = harness;
  const consent = inputs.get('routing-consent');
  assert.equal(consent.closest('details'), undefined, 'Required consent must remain visible outside advanced settings');
  assert.equal(consent.required, true);
  const role = inputs.get('routing-role-0');
  assert.equal(role.options[0][1], 'Use catalog role (Economy)');
  assert.match(role.parent.textContent, /Current role: Capability/);
  assert.match(root.textContent, /Saved → Effective/);
  assert.match(root.textContent, /Preference: 50 → 0/);
  assert.match(root.textContent, /Highest automatic effort: high → low/);
  assert.match(root.textContent, /New subagents: On → Off/);
  assert.match(root.textContent, /Available models: All verified models → gpt-6-luna/);

  const form = root.querySelector('form');
  inputs.get('routing-source').value = 'local';
  for (const index of [0, 1]) inputs.get(`routing-model-${index}`).checked = false;
  form.events.change();
  harness.changeLanguage('zh-CN');
  assert.equal(root.querySelector('h2').textContent, '自动选择模型');
  assert.deepEqual([0, 1].map(index => inputs.get(`routing-model-${index}`).checked), [false, false]);
  await root.querySelectorAll('button').find(button => button.text === '刷新模型目录').events.click();
  assert.deepEqual([0, 1].map(index => inputs.get(`routing-model-${index}`).checked), [false, false]);
  assert.equal(calls[0].type, 'routingRefreshModels');
  await root.querySelector('form').events.submit({ preventDefault() {} });
  assert.equal(calls.length, 1, 'Saving an empty draft must fail before sending a policy');
  assert.match(root.textContent, /请至少选择一个模型/);
  inputs.get('routing-task').value = 'Translate this sentence.';
  await root.querySelectorAll('button').find(button => button.text === '模拟选择').events.click();
  assert.equal(calls.length, 1, 'Preview must also validate model selection');
  harness.changeLanguage('en');
  inputs.get('routing-model-0').checked = true;
  inputs.get('routing-role-0').value = '';
  await root.querySelector('form').events.submit({ preventDefault() {} });
  assert.equal(calls[1].type, 'routingSave');
  assert.deepEqual(JSON.parse(JSON.stringify(calls[1].config.allowed_models)), ['gpt-6-luna']);
  assert.deepEqual(JSON.parse(JSON.stringify(calls[1].config.model_roles)), {});
  assert.equal(calls[1].expectedVersion, 'v1');

  const stale = setup();
  stale.inputs.get('routing-preference').value = '90';
  stale.root.querySelector('form').events.input();
  stale.view.userConfigVersion = 'v2';
  stale.changeLanguage('zh-CN');
  assert.equal(stale.inputs.get('routing-preference').value, '90');
  assert.equal(stale.root.querySelector('.routing-conflict').hidden, false);

  const empty = setup();
  empty.inputs.get('routing-source').value = 'local';
  for (const index of [0, 1]) empty.inputs.get(`routing-model-${index}`).checked = false;
  empty.root.querySelector('form').events.change();
  empty.view.models = [];
  await empty.root.querySelectorAll('button').find(button => button.text === 'Refresh model catalog').events.click();
  await empty.root.querySelector('form').events.submit({ preventDefault() {} });
  assert.equal(empty.calls.length, 1, 'An empty catalog must not turn an explicitly empty draft into unrestricted selection');
  empty.inputs.get('routing-task').value = 'Translate this sentence.';
  await empty.root.querySelectorAll('button').find(button => button.text === 'Simulate selection').events.click();
  assert.equal(empty.calls.length, 1, 'Preview must reject the empty draft even when no catalog fields are visible');
  empty.view.models = setup().view.models;
  await empty.root.querySelectorAll('button').find(button => button.text === 'Refresh model catalog').events.click();
  assert.deepEqual([0, 1].map(index => empty.inputs.get(`routing-model-${index}`).checked), [false, false]);

  const undiscovered = setup();
  undiscovered.view.models = [];
  undiscovered.render();
  undiscovered.inputs.get('routing-source').value = 'local';
  undiscovered.root.querySelector('form').events.change();
  await undiscovered.root.querySelector('form').events.submit({ preventDefault() {} });
  assert.equal(undiscovered.calls[0].type, 'routingSave', 'An initially unrestricted empty catalog remains a valid policy');
  assert.deepEqual(JSON.parse(JSON.stringify(undiscovered.calls[0].config.allowed_models)), []);
  console.log('WebUI routing: visible consent, catalog roles, policy differences, invalid draft language/refresh, submit/preview validation and stale versions passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
