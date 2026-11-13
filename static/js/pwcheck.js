// pwcheck.js - password policy hints (server enforces: >= 10 chars, not equal to the username) + show/hide toggle.
import { el, icon, clear } from '/js/ui.js';

export function policy(pw, username = '') {
  const checks = [
    { id: 'len', label: 'At least 10 characters', ok: pw.length >= 10 },
    { id: 'user', label: 'Not the same as the username', ok: !!pw && pw.toLowerCase() !== String(username).trim().toLowerCase() },
    { id: 'mix', label: 'Mix of letters and numbers or symbols (recommended)', ok: /[A-Za-z]/.test(pw) && /[^A-Za-z]/.test(pw), soft: true },
  ];
  const ok = checks.filter((c) => !c.soft).every((c) => c.ok);
  const score = !pw ? 0 : Math.min(4, (pw.length >= 10 ? 1 : 0) + (pw.length >= 14 ? 1 : 0) + (checks[2].ok ? 1 : 0) + (/[A-Z]/.test(pw) && /[a-z]/.test(pw) ? 1 : 0));
  return { ok, checks, score: ok ? Math.max(1, score) : Math.min(score, 1) };
}
export function passwordRules(listEl, meterEl, pw, username) {
  const p = policy(pw, username);
  clear(listEl);
  p.checks.forEach((c) => listEl.append(el('li', { class: c.ok ? 'met' : '' }, icon(c.ok ? 'check-circle' : 'minus'), c.label)));
  if (meterEl) meterEl.dataset.score = String(pw ? p.score : 0);
  return p;
}
export function wirePasswordToggle(btn, inputs) {
  btn.addEventListener('click', () => {
    const show = inputs[0].type === 'password';
    inputs.forEach((i) => (i.type = show ? 'text' : 'password'));
    btn.setAttribute('aria-pressed', String(show)); btn.setAttribute('aria-label', show ? 'Hide password' : 'Show password');
    btn.replaceChildren(icon(show ? 'eye-off' : 'eye'));
  });
}
